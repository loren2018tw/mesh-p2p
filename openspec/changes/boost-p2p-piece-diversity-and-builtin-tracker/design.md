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

### Decision 3: HTTP web seed 動態限速

**選擇**：根據當前 swarm 中 seeding peer 數量，在 HTTP range response 中插入 delay，降低 HTTP 輸出速率。

**策略**：

```
seeding_peers == 0 → 不限速（全速 HTTP）
seeding_peers 1-3  → 每 chunk 延遲 50ms（約限速 50%）
seeding_peers 4-7  → 每 chunk 延遲 150ms（約限速 75%）
seeding_peers >= 8 → 每 chunk 延遲 300ms（HTTP 僅補漏）
```

**理由**：直接在 HTTP 回應層控制速率最簡單，不需要修改 WebTorrent 的排程邏輯。讓 HTTP 變慢後，WebTorrent 自然會更多地從 P2P peers 取得資料。

### Decision 4: 內建 tracker 為首選，外部 tracker 作為 fallback

**選擇**：torrent metadata 的 announce-list 中，內建 tracker URL 排在第一位，外部公共 tracker 排在後面。

**理由**：WebTorrent 會按照 announce-list 順序連接 tracker，內建 tracker 排在第一位確保最先建立 peer 連線。外部 tracker 保留作為極端情況的備援。

## Risks / Trade-offs

- **WebSocket 連線數壓力** → 每個下載端都會建立 WebSocket 長連線到 tracker，如果同時 100+ 下載端可能有記憶體壓力。Mitigation：設定連線數上限，超過時拒絕新連線並回退到外部 tracker。
- **Piece priority 與 WebTorrent 內部排程衝突** → WebTorrent 有自己的 rarest-first 策略，外部設定 priority 可能被覆蓋。Mitigation：使用 `torrent.select()` API 是 WebTorrent 官方支援的介面，會被 piece picker 尊重。
- **HTTP 限速可能降低首個 client 的體驗** → 當只有 1 個 seeder 時就開始限速，但此時只有 2 人在下載。Mitigation：seeder 數為 0 時完全不限速，確保首個下載者體驗不變。
- **Tracker state 在伺服器記憶體中** → 重啟會丟失所有 peer 連線。Mitigation：WebTorrent 客戶端有自動重連機制，重啟後 peers 會自動重新 announce。
