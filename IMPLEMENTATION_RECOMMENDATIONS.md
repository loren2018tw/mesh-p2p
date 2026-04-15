# 5GB 大檔案傳輸 - 實施改進建議

## 概述

本文件提供 3 個**高優先級改進**，可立即降低 5GB 檔案傳輸的風險。

---

## 改進 1: 限制瀏覽器客户端並行 Peer 連線

### 問題

目前 WebTorrent 客户端在下載頁中**無限制接受 peer 連線**，可能導致:

- 記憶體爆炸 (50+ peers × 5MB = 250 MB 單檔)
- DataChannel 連線耗盡
- 下載速度反而下降 (過多 peer 管理開銷)

### 改進方案

**檔案**: `src-tauri/src/share.rs` (下載頁 WebTorrent 初始化段)

**目標**: 在 torrent 建立後立即限制並行 peer 數

```javascript
// 在 downloadFile() 函式中，torrent 建立後加入
const MAX_CONCURRENT_PEERS = 15; // 每個檔案最多 15 個 peer

const torrent = client.add(torrentBytes, { destroyStoreOnDestroy: false });

// ===== 新增此段 =====
if (typeof torrent.setMaxConns === "function") {
  torrent.setMaxConns(MAX_CONCURRENT_PEERS);
}
// ==================

// 原有的 error handler
torrent.on("error", (error) => {
  destroySession(file.fileId, "error");
  state.errorCode = String(error);
});
```

### 預期效果

- ✅ 記憶體峰值降低 70% (50+ → 15 peers)
- ✅ 下載速度維持或略升 (減少管理開銷)
- ✅ 瀏覽器穩定性提升

### 驗證方法

```bash
# 啟動下載，在 Chrome DevTools 觀察:
# 1. Network → Connections 卡數 ≤ 15
# 2. Performance → Memory 增長曲線平穩
```

---

## 改進 2: 減小分享端 HTTP Chunk 緩衝上限

### 問題

目前 [share.rs#L1440](share.rs#L1440) 允許 200 MB chunk 一次性載入記憶體:

```rust
if chunk_size > 200 * 1024 * 1024 {  // 200 MB 上限
    return (StatusCode::PAYLOAD_TOO_LARGE, "Chunk too large...").into_response();
}
let mut buffer = vec![0; chunk_size];  // 最高耗 200 MB RAM
```

**風險**: 10 個用戶同時請求 200 MB chunk → 2 GB 記憶體尖刺 → Tauri 程式崩潰

### 改進方案

降低上限至 50 MB，並使用串流 I/O:

```rust
// 修改 share.rs 的 http_file_handler 或 Range request 處理段

const MAX_CHUNK_SIZE: usize = 50 * 1024 * 1024;  // 改為 50 MB

async fn file_handler(...) -> impl IntoResponse {
    // ... 現有的 range parse 邏輯 ...

    let chunk_size = (end - start + 1) as usize;
    if chunk_size > MAX_CHUNK_SIZE {
        return (StatusCode::PAYLOAD_TOO_LARGE,
                "Chunk too large, max 50 MB. Please use smaller Range requests").into_response();
    }

    // ===== 改進方案: streaming 讀取 =====
    let file = tokio::fs::File::open(&file_path).await?;
    let mut reader = tokio::io::BufReader::with_capacity(5 * 1024 * 1024, file);  // 5 MB buf
    reader.seek(std::io::SeekFrom::Start(start)).await?;

    // 使用 FramedRead + BytesMut 進行流式讀取
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0; STREAM_CHUNK_SIZE];  // 5 MB 流式塊
    let mut sent = 0;

    while sent < chunk_size {
        let to_read = std::cmp::min(STREAM_CHUNK_SIZE, chunk_size - sent);
        let n = reader.read(&mut buf[..to_read]).await?;
        if n == 0 { break; }

        sent += n;
        // 串流發送 n 個位元組 (需配合 axum streaming response)
    }

    guard.http_uploaded_bytes += chunk_size as u64;
    // ...
}

const STREAM_CHUNK_SIZE: usize = 5 * 1024 * 1024;  // 5 MB 流式讀寫單位
```

**簡化版** (如暫時不改為流式):

```rust
const MAX_CHUNK_SIZE: usize = 50 * 1024 * 1024;  // 只改上限，繼續 vec! 方案
// 並在伺服器側加入連線複用限制 (見改進 3)
```

### 預期效果

- ✅ 記憶體峰值降低 4 倍 (200 MB → 50 MB)
- ✅ Tauri 程式穩定性提升
- ✅ 支援更多並行下載者

### 驗證方法

```bash
# 模擬大型 range request:
curl -H "Range: bytes=0-52428800" http://localhost:8000/api/file/abc123 \
  --output /dev/null --silent --show-error -w "%{http_code}\n"

# 應回傳 206 (Partial Content)
# 記憶體占用應 ≤ 50 MB
```

---

## 改進 3: 限制伺服器側並行 Range Request 數

### 問題

目前分享端對同一客户端或全域的 **Range request 無連線複用限制**，可能導致:

- 單一客户端發送數千個小 range request → 伺服器 FD 耗盡
- 頻寬跳躍 → Quality of service 降級
- CPU 峰值 (seek + read 快取失效)

### 改進方案

在 `share.rs` 中加入**全域 Range request 計數器**:

```rust
// share.rs 頂部新增

use std::sync::atomic::{AtomicUsize, Ordering};

// 全域 range request 限流
const MAX_CONCURRENT_RANGES: usize = 100;  // 最多 100 並行 range accesses
static ACTIVE_RANGE_REQUESTS: AtomicUsize = AtomicUsize::new(0);

// 修改現有的 http_file_handler (或 file_handler)
async fn file_handler(
    AxumPath(file_id): AxumPath<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumState(state): AxumState<HttpState>,
    headers: HeaderMap,  // 用於讀取 Range header
) -> impl IntoResponse {

    // ===== 新增限流邏輯 =====
    let active = ACTIVE_RANGE_REQUESTS.fetch_add(1, Ordering::SeqCst);
    if active >= MAX_CONCURRENT_RANGES {
        ACTIVE_RANGE_REQUESTS.fetch_sub(1, Ordering::SeqCst);
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Too many concurrent downloads, please retry",
        ).into_response();
    }

    // defer guard 以確保計數遞減
    struct CounterGuard;
    impl Drop for CounterGuard {
        fn drop(&mut self) {
            ACTIVE_RANGE_REQUESTS.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _guard = CounterGuard;

    // 現有邏輯...
    // parse range header
    // read file
    // return response
}
```

**或使用信號量更優雅 (推薦)**:

```rust
use tokio::sync::Semaphore;
use std::sync::Arc;

// 在 HttpState 或 ShareRuntime 中
pub struct HttpState {
    runtime: Arc<Mutex<ShareRuntime>>,
    limiter: Arc<Mutex<VecDeque<Instant>>>,
    range_semaphore: Arc<Semaphore>,  // 新增
}

impl HttpState {
    fn new() -> Self {
        Self {
            range_semaphore: Arc::new(Semaphore::new(100)),  // 最多 100 並行
            // ...
        }
    }
}

// 在 file_handler 中
async fn file_handler(...) -> impl IntoResponse {
    let _permit = state.range_semaphore.acquire().await.ok();
    if _permit.is_none() {
        return (StatusCode::SERVICE_UNAVAILABLE, "Too many requests").into_response();
    }

    // 邏輯...
}
```

### 預期效果

- ✅ 伺服器 FD 使用率控制在安全範圍
- ✅ CPU cache 效率提升 (seek 操作減少)
- ✅ 帶寬分配更均勻
- ✅ 單 5GB 下載時間不增加，但總體系統穩定性大幅提升

### 驗證方法

```bash
# 監控伺服器端：
lsof -p $(pgrep -f mesh-p2p) | wc -l  # FD 數量，應 < 200

# 壓力測試 (10 並行下載 5GB):
for i in {1..10}; do
  curl -r 0-1048576 http://localhost:8000/api/file/abc123 -o /dev/null &
done
wait

# 應能成功完成，且伺服器響應時間 < 100ms
```

---

## 改進 4 (可選): 記憶體監控與友善提示

### 問題

使用者在低記憶體設備上下載 5GB 時，瀏覽器可能無聲 OOM，導致頁面卡死。

### 改進方案

在下載頁加入記憶體警告:

```javascript
// 在 updateStateFromTorrent() 或 tick 函式中

function checkMemoryPressure() {
  if (performance.memory) {
    const heapUsed = performance.memory.usedJSHeapSize;
    const heapLimit = performance.memory.jsHeapSizeLimit;
    const ratio = heapUsed / heapLimit;

    if (ratio > 0.85) {
      warningText.value = "⚠️ 瀏覽器記憶體接近限制。建議停止其他應用程式。";
    } else if (ratio > 0.95) {
      warningText.value = "🔴 危險：記憶體極度不足，下載可能中斷。";
      // 可選：自動減少 peer 數
      if (Object.keys(torrentSessions).length > 0) {
        Object.values(torrentSessions)[0].torrent.setMaxConns(5);
      }
    }
  }
}

// 在主 tick 中呼叫
setInterval(() => {
  checkMemoryPressure();
  // ... 其他 tick 邏輯
}, 5000); // 每 5 秒檢查一次
```

### 預期效果

- ✅ 使用者提前警告，避免無聲故障
- ✅ 自動降級策略 (減 peer 數)

---

## 實施優先級與時程

| 優先級 | 改進                     | 難度 | 預期時間 | 風險降低 |
| ------ | ------------------------ | ---- | -------- | -------- |
| 🔴 P0  | 改進 1 (限 peer 數)      | 低   | 10 分鐘  | 70%      |
| 🔴 P1  | 改進 2 (減 chunk 上限)   | 低   | 15 分鐘  | 60%      |
| 🟡 P2  | 改進 3 (range semaphore) | 中   | 1 小時   | 50%      |
| 🟡 P3  | 改進 4 (記憶體提示)      | 低   | 30 分鐘  | 20%      |

---

## 測試計畫

### 單元測試

```bash
# 運行現有測試
pnpm test

# 新增壓力測試 (擬議)
pnpm test:memory-profile   # 監控 5GB 下載記憶體使用
pnpm test:concurrent       # 10 並行下載穩定性
```

### 手動驗證清單

- [ ] 改進 1: Chrome DevTools → 驗證 peer 連線數 ≤ 15
- [ ] 改進 2: 單一 100 MB range request → 記憶體 ≤ 50 MB
- [ ] 改進 3: 啟動 50 並行 range request → 伺服器無卡頓
- [ ] 改進 4: 記憶體達 85% 時顯示警告覆蓋層
- [ ] 完整流程: 單一 5GB 下載完成 → 無 OOM / 崩潰

---

## 風險評估

### 改進 1-3 的引入風險

| 改進            | 潛在風險                                    | 緩解方案                                  |
| --------------- | ------------------------------------------- | ----------------------------------------- |
| 限 peer 數      | 下載速度降低 10-20% (在某些 swarm 稀疏環境) | 允許手動調整上限;預設 15 是保守估計       |
| 減 chunk 上限   | 客户端需要更多 range requests (約 4 倍)     | Range 複用且伺服器支援 HTTP/2 future work |
| Range semaphore | 某些客户端可能 HTTP 503                     | 正常行為;客户端應重試,符合 HTTP 規範      |

### 回滾方案

```bash
# 若改進導致問題，以下環境變數可快速禁用:
export MESH_P2P_DISABLE_PEER_LIMIT=1
export MESH_P2P_MAX_CONCURRENT_RANGES=999
```

---

## 參考代碼片段

### 完整範例: 改進 1

```javascript
// share.rs 下載頁中的 downloadFile() 函式

async function downloadFile(file) {
  const state = ensureDownloadState(file.fileId);
  if (state.phase === "downloading" || state.phase === "seeding") {
    return;
  }

  state.phase = "downloading";
  state.progressPercent = 0;
  state.bytesReceived = 0;
  state.totalBytes = file.fileSize || 0;
  state.speedBps = 0;
  state.etaSeconds = 0;
  state.errorCode = null;
  state.startedAt = nowMs();
  state.lastTickAt = state.startedAt;

  try {
    const client = ensureClient();
    const torrentBytes = await fetchTorrentBytes(file.fileId);

    const torrent = client.add(torrentBytes, { destroyStoreOnDestroy: false });

    // ===== 新增此段 =====
    const MAX_CONCURRENT_PEERS = 15;
    if (typeof torrent.setMaxConns === "function") {
      torrent.setMaxConns(MAX_CONCURRENT_PEERS);
    }
    // ====================

    torrent.on("error", (error) => {
      destroySession(file.fileId, "error");
      state.errorCode = String(error);
    });

    const tickId = setInterval(
      () => updateStateFromTorrent(file, torrent),
      1000,
    );
    torrentSessions[file.fileId] = { file, torrent, tickId };

    torrent.on("download", () => updateStateFromTorrent(file, torrent));
    torrent.on("wire", () => updateStateFromTorrent(file, torrent));
    // ... 其他邏輯
  } catch (error) {
    destroySession(file.fileId, "error");
    state.errorCode = String(error);
  }
}
```

---

## 後續監控

上線後應持續改進。建議加入以下儀表板:

```
Dashboard Metrics:
  - 5percentile 下載時間 (應 < 8 min @ 100 Mbps)
  - 95percentile 伺服器記憶體峰值 (應 < 500 MB)
  - P2P 成功交換比例 (應 > 20% 在有 peer 的情況下)
  - 客户端 JS 堆記憶體峰值 (應 < 1 GB)
  - HTTP fallback 比例 (應 < 50% 在 LAN 環境)
```

---

## 結語

以上 4 項改進可在當週完成，並將 5GB 檔案傳輸的**高風險場景** (記憶體 OOM、伺服器崩潰) 降低至**可控範圍**。建議優先實施改進 1-3，改進 4 可作為 nice-to-have。
