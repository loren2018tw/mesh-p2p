# 5GB 大檔案傳輸架構分析

## 執行摘要

目前架構在傳輸接近 5GB 的檔案時存在以下**關鍵風險**：

| 類別                      | 風險等級 | 問題                                     | 影響                            |
| ------------------------- | -------- | ---------------------------------------- | ------------------------------- |
| **瀏覽器記憶體**          | 🔴 高    | Piece metadata 展開、WebTorrent 內部緩衝 | 可能導致 OOM、頁面卡頓          |
| **Tracker 連續性**        | 🟡 中    | WebRTC/Tracker 無法在 LAN 環境可靠探索   | P2P 失敗回退 HTTP，分享端壓力大 |
| **分享端伺服器負載**      | 🔴 高    | HTTP Range request 頻繁、無限制連線數    | 帶寬飽和、連線耗盡              |
| **Torrent Metadata 大小** | 🟡 中    | ~100MB piece hash 資料                   | 初始化延遲、傳輸花費額外頻寬    |
| **WebTorrent 穩定性**     | 🟡 中    | 30+ 分鐘長時間 seeding 未驗證            | 記憶體洩漏、連線掉線            |

---

## 詳細分析

### 1. 瀏覽器記憶體限制

#### 1.1 Piece Metadata 展開

```
檔案大小: 5 GB = 5,368,709,120 bytes
Piece 大小: 256 KB = 262,144 bytes
Piece 數量: 5,368,709,120 / 262,144 = 20,480 pieces
SHA1 哈希大小: 20 bytes/piece
總 Piece Hash 資料: 20 × 20,480 = 409.6 KB
```

✅ **Piece hash 本身不是主要瓶頸** (~409KB)

#### 1.2 WebTorrent 內部緩衝

WebTorrent.js 在瀏覽器中的主要記憶體消耗:

1. **下載中的片段快取** (per file):
   - 同時活躍下載: ~10-50 pieces
   - 每個 piece: 256 KB
   - 快取規模: 2.5-12.8 MB (可接受)

2. **Torrent 物件狀態**:
   - 每個 torrent: ~5-15 MB (取決於 peer 連線數)
   - 連線數越多記憶體越大

3. **DataChannel 緩衝區**:
   - 每個 peer: ~1-5 MB
   - 連線數 × 5 MB = 潛在高記憶體

**問題**: 如果同時連接 50+ peers，記憶體占用可達 250-500 MB，加上瀏覽器本身開銷，可能觸發 GC 壓力甚至 OOM。

#### 1.3 IndexedDB / 本地儲存配額限制

- **Chrome/Firefox/Safari**: 通常 50MB-1GB per origin
- **目前實作**: 未實裝本地快取持久化 (見 `README.md` 備註)
- **風險**: 無法重用已下載片段跨 session（需另起新下載）

**建議**: 監控記憶體，限制同時 peer 連線數至 10-20。

---

### 2. Torrent Metadata 傳輸成本

#### 2.1 Metadata 大小

```
5GB 檔案 torrent 結構:
- 檔名: ~200 bytes
- Piece 雜湊 (20,480 × 20): 409.6 KB
- Tracker 清單: ~1-5 KB
- Web seed URL: ~200 bytes
總: ~410-420 KB per file
```

#### 2.2 傳輸成本

- **初始化**: 每位下載者獲取一次 (~410 KB)
- **多下載**:
  - 10 位下載者 × 410 KB = 4.1 MB
  - 100 位下載者 × 410 KB = 41 MB

**問題**: 如果 metadata 被重複請求（重新整理、多標籤頁），會產生不必要流量。

**現狀**:

- 無版本快取 (Cache-Control 不明確)
- 無 metadata 壓縮
- 未避免重複序列化

---

### 3. 分享端伺服器負載

#### 3.1 HTTP Range Request 處理

```rust
// 當前 share.rs 限制
const RATE_LIMIT_MAX: usize = 5000;           // 每秒 5000 requests
const RATE_LIMIT_WINDOW_MS: u64 = 1000;
const DEFAULT_PIECE_SIZE: usize = 256 * 1024; // 256 KB per piece
```

#### 3.2 痛點分析

**場景**: 10 位下載者同時下載 5GB

```
最壞情況:
- 每位下載者: 20,480 pieces
- 共 HTTP fallback (無 P2P): 204,800 individual requests
- 每個 request: 256 KB
- 總頻寬需求: 5GB × 10 = 50 GB

分享端限制:
- 允許 chunk_size ≤ 200 MB
- 每秒 range request: 5000 limit
- 實際瓶頸: 網卡頻寬 (通常 1Gbps = 125 MB/s)
```

**問題:**

1. **連線數無上限**→ 可能耗盡 file descriptor
2. **Memory 緩衝每個 chunk**→ `vec![0; chunk_size]` 可達 200 MB
3. **無連線池管理**→ 慢客戶端拖累整體吞吐

#### 3.3 當前風險代碼

[share.rs#L1450](share.rs#L1450):

```rust
let mut buffer = vec![0; chunk_size];  // 最高 200 MB per request
```

**改進建議**:

- 使用 zero-copy streaming (e.g., `sendfile`)
- 限制並行 range request 數
- 實裝連線複用 (HTTP/2 推薦但 Tauri/localhost 可能不支援)

---

### 4. WebRTC/Tracker 可達性問題

#### 4.1 LAN 環境的 P2P 探索失敗

設計文檔已識別此風險:

> [Risk] 瀏覽器環境中 WebRTC/Tracker 可達性受網路政策影響，可能出現 peer 探索不穩定。

#### 4.2 典型故障場景

```
分享者 (192.168.1.10:服務埠)
    ↓
下載者A/B 啟動 WebTorrent client
    ↓
tracker announce (UDP/HTTP)
    ↗↙ 問題: 外部 tracker 在 LAN 內無法回應
         WebRTC 信令困難 (STUN server 可能不可達)
    ↓
Fallback → HTTP Range request only
    ↓
分享端單點支撐 → 頻寬飽和
```

#### 4.3 影響

- **P2P 失效率**: 在無外部 tracker/STUN 的 LAN 中可達 90%+
- **分享端壓力**: 所有下載者都仰賴 HTTP → 帶寬翻倍需求

**緩解措施** (已在設計文檔):

- ✅ HTTP fallback (已實裝)
- ⚠️ 多 tracker 清單 (待驗證)
- ⚠️ 本地 mDNS/Bonjour (未實裝)

---

### 5. WebTorrent 長時間運行穩定性

#### 5.1 測試覆蓋範圍

從 `replay-webtorrent-lifecycle.mjs` 看:

- ✅ 單一下載完成測試
- ✅ Peer 發現測試
- ❌ **30+ 分鐘持續 seeding 未測試**
- ❌ **多檔案同時 seeding 未測試**
- ❌ **記憶體洩漏壓力測試缺失**

#### 5.2 已知風險

```javascript
// share.rs 中的清理邏輯
if (session.torrent) {
  void reportClientStats(session.file, session.torrent, false);
  session.torrent.destroy({ destroyStore: false }, () => {}); // 非阻塞
}
delete torrentSessions[fileId];
```

**問題**:

- `destroy()` 回呼未等待 → 可能資源洩漏
- 長時間 seeding 中的 wire connection 未定期掃描
- WebRTC DataChannel 未定期驗證存活性

---

## 瀏覽器資源限制總結

### 記憶體限制

| 瀏覽器  | 單一頁面限制 | 5GB 下載評估                      |
| ------- | ------------ | --------------------------------- |
| Chrome  | ~2 GB        | ⚠️ 邊界 (需監控)                  |
| Firefox | ~1.5 GB      | ⚠️ 風險 (同時 >20 peers 可能 OOM) |
| Safari  | ~1 GB        | 🔴 不建議                         |
| Edge    | ~2 GB        | ⚠️ 邊界                           |

### 儲存配額

| 類型         | 限制                  | 現況      |
| ------------ | --------------------- | --------- |
| IndexedDB    | Per origin (50MB-1GB) | ❌ 未利用 |
| localStorage | 5-10 MB               | ❌ 不適合 |
| Cache API    | 500 MB (可配)         | ❌ 未實裝 |

### 網路連線限制

| 限制類型                   | 數值                  | 影響                                |
| -------------------------- | --------------------- | ----------------------------------- |
| 同時 TCP 連線 (per origin) | ~6-10                 | ✅ 在限制內 (rate limit 5000 req/s) |
| WebRTC DataChannel 數量    | ~10-50 (取決於瀏覽器) | ⚠️ 需監控                           |
| WebSocket 連線             | ~1 per page           | ✅ 未使用                           |

---

## 推薦改進方案

### 短期 (立即)

1. **限制並行 peer 連線**:

   ```javascript
   // 下載頁客戶端
   const MAX_CONCURRENT_PEERS = 15; // 每個 file
   torrent.setMaxConns(MAX_CONCURRENT_PEERS);
   ```

2. **監控記憶體壓力**:

   ```javascript
   if (performance.memory && performance.memory.usedJSHeapSize > 800_000_000) {
     // 觸發記憶體警告，建議減少 peer 或停止其他文件
   }
   ```

3. **驗證伺服器側 zero-copy**:
   - 確認 HTTP Range response 使用 streaming 而非 buffering
   - 使用 `Content-Range` 並支援 gzip (可選)

4. **增強 Tracker 失敗偵測**:
   ```javascript
   const TRACKER_TIMEOUT_MS = 5000; // 快速失敗回退到 HTTP
   ```

### 中期 (1-2 weeks)

1. **IndexedDB 快取**:
   - 為已下載 piece 建立持久化索引
   - 下次下載相同檔案時檢查並標示 "可重用"

2. **Metadata 版本與壓縮**:
   - 加入 ETag/Cache-Control
   - 將 piece hash 改為 uint8array 並使用 MessagePack 或 protobuf

3. **伺服器側連線管理**:
   - 實裝連線複用 pool
   - 限制單一客户端並行 range request ≤ 3

### 長期 (後續 change)

1. **本地檔案快取掃描與重用** (已在 non-goals 中提及)
2. **多 tracker + mDNS 回源** (LAN 內本地探索)
3. **漸進式 torrent metadata 載入** (大檔案分段下載 metadata)

---

## 5GB 檔案可行性評估

### ✅ 可行場景

- **可控環境**: 固定 5-10 下載者、LAN 內、外部 tracker 可達
- **下載時間預期**:
  - 100 Mbps 網路: ~7-8 分鐘 (HTTP only)
  - 1 Gbps 網路: 40-50 秒 (HTTP only)
- **瀏覽器**: Chrome/Edge (記憶體充足)

### ⚠️ 風險場景

- **無外部 tracker**: 所有下載者回退 HTTP → 分享端成為單點
- **多並行下載** (>50 downloaders): 分享端頻寬飽和、記憶體壓力升高
- **低配置瀏覽器** (Firefox/Safari): OOM 風險
- **不穩定網路**: WebRTC DataChannel 頻繁中斷

### 🔴 不可行場景

- **> 10GB 檔案**: 考慮 piece hash 展開與 torrent metadata 本身大小
- **100+ 並行下載者**: 需要分時段或多伺服器
- **瀏覽器單頁面**: 建議改為呼叫系統下載器

---

## 檢查清單

### 上線前應驗證

- [ ] 單一 5GB 檔案、10 下載者、3000ms metadata API 延遲的壓力測試
- [ ] Chrome DevTools 記憶體分析 (Heap snapshot)
- [ ] 伺服器側連線數與頻寬監控設置
- [ ] WebTorrent destroy 回呼確實阻塞（非異步洩漏）
- [ ] HTTP fallback 成功率測量
- [ ] 30+ 分鐘 seeding 的穩定性驗證

### 監控指標

```
分享端:
  - HTTP 上傳速率 (分鐘平均)
  - 活躍連線數
  - 檔案 descriptor 使用率

下載端頁面:
  - JS 堆記憶體使用
  - WebTorrent peer 連線數
  - Download speed (P2P vs HTTP fallback 比例)
```

---

## 附錄: 關鍵代碼指標

| 位置                             | 指標               | 現狀       | 建議                                |
| -------------------------------- | ------------------ | ---------- | ----------------------------------- |
| [share.rs#L26](share.rs#L26)     | DEFAULT_PIECE_SIZE | 256 KB     | ✅ (權衡 metadata 與 I/O 細度)      |
| [share.rs#L1440](share.rs#L1440) | MAX_CHUNK_SIZE     | 200 MB     | ⚠️ 減至 50 MB 避免記憶體尖刺        |
| [share.rs#L27](share.rs#L27)     | RATE_LIMIT_MAX     | 5000 req/s | ⚠️ 考慮降至 1000 req/s 以保護分享端 |
| download_page                    | MAX_PEERS (JS)     | 無限制     | 🔴 改為 15                          |

---

## 參考資料

1. 設計文檔: [design.md](openspec/changes/implement-p2p-seeding-and-downloader-cache-reuse/design.md)
2. 規格: [browser-p2p-swarm-download/spec.md](openspec/specs/browser-p2p-swarm-download/spec.md)
3. WebTorrent 官方限制: https://github.com/webtorrent/webtorrent/issues
4. 瀏覽器記憶體限制參考: https://dev.chromium.org/blink/gc-goals
