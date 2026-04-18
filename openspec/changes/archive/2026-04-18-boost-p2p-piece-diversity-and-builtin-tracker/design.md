## Context

目前 mesh-p2p 的 P2P 分享效率極低。實際運行數據顯示：HTTP 上傳 95.69 GB，P2P 僅 146.68 MB（不到 0.15%），HTTP fallback 次數高達 57,628 次。

根本原因有三：

1. **Piece 同質化**：所有下載端以相同順序下載 pieces，導致大家手上的片段相同，無法互相交換
2. **Peer 發現延遲**：依賴外部公共 WebSocket tracker（wss://tracker.openwebtorrent.com 等），海外延遲高且不穩定，peers 要數十秒才能互相發現
3. **HTTP 太強勢**：HTTP web seed 全速提供，WebTorrent 排程器自然優先使用最快來源，P2P 完全搶不到排程

現有架構：

- 後端 `src-tauri/src/share.rs`：Rust + axum HTTP/HTTPS 伺服器，負責 metadata API、torrent 生成、檔案 serving
- 前端：嵌入式 HTML/JS 下載頁，使用 WebTorrent 瀏覽器版本
- Tracker：完全依賴 3 個外部公共 WSS tracker
- Piece 策略：WebTorrent 預設 rarest-first，但初期無人有 piece，全 fallback 到 HTTP

## Goals / Non-Goals

**Goals:**

- 讓 P2P 流量佔比從 < 0.15% 提升至 30%+
- 實現毫秒級 peer 發現（同一伺服器內 signaling）
- 讓不同下載端優先取得不同 pieces，產生真正的 piece 互補效果
- HTTP web seed 智慧退讓，當 P2P peer 充足時自動降低 HTTP 流量
- 保持向後相容，外部 tracker 作為 fallback

**Non-Goals:**

- 不實作完整 BitTorrent tracker 規範（僅需支援 WebTorrent 所用的 WebSocket announce/offer/answer）
- 不實作 DHT（瀏覽器環境不支援）
- 不改變 FileSystemChunkStore 的儲存機制
- 不修改 Tauri 桌面端 UI（僅影響嵌入式下載頁）

## Decisions

### Decision 1: 在 axum 伺服器內嵌 WebSocket tracker

**選擇**：在現有 axum 路由上新增 `/announce` WebSocket endpoint，直接實作 bittorrent-tracker 的 WebSocket JSON protocol。

**替代方案考量**：

- **外掛獨立 tracker 程序**：增加部署複雜度，需要額外 port 和程序管理 → 拒絕
- **使用自架的公共 tracker Docker image**：延遲仍有網路 hop → 拒絕
- **僅優化外部 tracker 的連線數**：無法解決根本延遲問題 → 拒絕

**理由**：分享端伺服器本身已經是所有下載端的連線目標，tracker 功能內建在同一伺服器上，peer signaling 延遲趨近於零，且不需要額外基礎設施。

**實作方式**：

- axum 0.8 原生支援 WebSocket upgrade（透過 `axum::extract::ws`）
- 維護一個 `HashMap<InfoHash, HashMap<PeerId, WebSocketSender>>` 做 swarm 管理
- 接收 announce message（含 WebRTC offers）→ 轉發給同 info_hash 下的其他 peers
- 接收 answer message → 轉發給指定 peer
- 在 torrent 生成時將內建 tracker URL（`wss://{host}/announce`）置為 announce-list 第一項

### Decision 2: Server 分配 piece priority offset

**選擇**：metadata API 為每個請求回傳一個 `piecePriorityOffset`（基於目前 client 數量的輪轉 offset），下載端用此 offset 打亂 piece 優先順序。

**替代方案考量**：

- **Client 端純隨機**：無法保證多樣性分佈均勻 → 拒絕
- **Server 端分配具體 piece 列表**：overhead 太大且不彈性 → 拒絕
- **修改 WebTorrent 原始碼實作 custom piece picker**：維護成本高、升級困難 → 拒絕

**理由**：offset 方式最簡潔，server 只需維護一個遞增 counter，每個 client 拿到不同的 offset 後自行計算 piece 優先順序。N 個 client 會自然形成 N 個錯開的下載波前，piece 多樣性最大化。

**實作方式**：

- Server 維護每個 file 的 `next_piece_offset` atomic counter
- metadata API 回傳 `piecePriorityOffset` 欄位
- Client 端在 torrent ready 後，將 pieces 分為 `numSlices` 組（依據 client 總數），優先下載 offset 對應的那組
- 使用 `torrent.select(start, end, priority)` 設定片段優先級

### Decision 3: HTTP web seed「Seeder 培養」策略（取代均勻限速）

**選擇**：Server 維護固定數量的「全速下載名額」（fast slots，預設 2 個）。擁有名額的 client 以全速 HTTP 下載，其餘 client 的 HTTP 請求被大幅延遲（僅靠 P2P 取得資料）。當全速 client 完成下載成為 seeder 後，名額釋放給下一位等待的 client。

**被取代的方案**：

- **依 seeder 數量均勻限速（原 Decision 3）**：實測發現效果差。所有 client 都被限速 → 都慢慢下載 → 都只有部分 pieces → 上傳能力弱 → P2P 流量仍低。根本問題是「下載中的 client 上傳能力遠低於已完成的 seeder」。

**策略邏輯**：

```
fast_slots = 2  （可全速 HTTP 下載的名額數）

file_handler 收到 range request:
  if client IP 在 fast_slots 中 → 全速回應（0ms delay）
  elif fast_slots 未滿           → 加入 fast_slots，全速回應
  else                           → 大幅延遲（2000ms/chunk），迫使依賴 P2P

client_stats_handler 收到 is_seeding=true:
  if client IP 在 fast_slots 中 → 從 fast_slots 移除（名額釋放）

超時保護:
  if fast_slot client 超過 60 秒無 range request → 移除（防止 slot 被占死）
```

**理由**：

1. **完整 seeder 的上傳效率遠高於下載中 client**：seeder 擁有全部 pieces、無需保留頻寬給自己的下載、能滿足所有 peer 的任何請求
2. **Seeder 數量指數成長**：每一輪培養出的 seeder 都能加速下一輪，1→2→4→8→16
3. **Server 頻寬利用率最大化**：永遠在全速培養下一個完整 seeder，不浪費頻寬在「半成品」
4. **Non-slot client 不會卡死**：仍可從已完成的 seeder 透過 P2P 全速下載，且如果 P2P 不可用，HTTP 仍有（很慢的）fallback

### Decision 4: 內建 tracker 為首選，外部 tracker 作為 fallback

**選擇**：torrent metadata 的 announce-list 中，內建 tracker URL 排在第一位，外部公共 tracker 排在後面。

**理由**：WebTorrent 會按照 announce-list 順序連接 tracker，內建 tracker 排在第一位確保最先建立 peer 連線。外部 tracker 保留作為極端情況的備援。

## Risks / Trade-offs

- **WebSocket 連線數壓力** → 每個下載端都會建立 WebSocket 長連線到 tracker，如果同時 100+ 下載端可能有記憶體壓力。Mitigation：設定連線數上限，超過時拒絕新連線並回退到外部 tracker。
- **Piece priority 與 WebTorrent 內部排程衝突** → WebTorrent 有自己的 rarest-first 策略，外部設定 priority 可能被覆蓋。Mitigation：使用 `torrent.select()` API 是 WebTorrent 官方支援的介面，會被 piece picker 尊重。
- **Non-slot client 初期體驗** → 未獲得 fast slot 的 client 初期可能幾乎無進度（P2P seeder 尚未就緒）。Mitigation：fast slots 為 2 個，第一批 seeder 培養完成後即可 P2P 服務後續 client；且 HTTP 仍有極慢 fallback（2s/chunk）不至於完全卡死。
- **Fast slot 被慢速 client 佔用** → 如果一個 slot client 網路極慢，會拖累整體效率。Mitigation：60 秒無活動超時自動釋放 slot。
- **Tracker state 在伺服器記憶體中** → 重啟會丟失所有 peer 連線。Mitigation：WebTorrent 客戶端有自動重連機制，重啟後 peers 會自動重新 announce。
