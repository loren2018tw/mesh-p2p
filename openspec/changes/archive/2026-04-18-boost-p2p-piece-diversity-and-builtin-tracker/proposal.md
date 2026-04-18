## Why

目前 P2P 分享效率極低（HTTP 上傳 95 GB vs P2P 僅 146 MB），所有下載端從相同順序下載 pieces 導致互相之間沒有可交換的片段，加上依賴外部公共 WebSocket tracker（延遲高、不穩定），peers 發現過慢。需要從根本上改變 piece 取得策略並內建 tracker，讓下載端已取得的片段能立即被其他下載端利用，達成真正的協同分享。

## What Changes

- 分享端伺服器內建 WebSocket tracker（實作 BitTorrent tracker protocol over WebSocket），取代對外部公共 tracker 的依賴，讓所有下載端透過分享端本身做 peer signaling，實現毫秒級 peer 發現
- 下載端每個 client 採用不同的 piece 優先順序策略（server 分配隨機 seed 或 offset），讓不同下載端優先取得不同片段，產生 piece 互補效果
- HTTP web seed 根據目前 swarm 中 seeder 數量動態調整回應速度，當 P2P peer 充足時降低 HTTP 輸出，強制讓 P2P 承擔更多流量
- metadata API 擴充，回傳每個 client 專屬的 piece priority hint 與內建 tracker URL

## Capabilities

### New Capabilities

- `builtin-websocket-tracker`: 分享端伺服器內建 WebSocket tracker，實作 BitTorrent tracker protocol（announce/offer/answer），讓所有下載端透過分享端直接做 WebRTC signaling，不依賴外部 tracker
- `piece-diversity-strategy`: 下載端 piece 優先順序多樣化策略，每個 client 根據 server 分配的 hint 以不同順序下載 pieces，確保 swarm 中 piece 分佈多樣化，最大化 P2P 交換效率

### Modified Capabilities

- `local-share-web-server`: 新增內建 tracker 的 WebSocket endpoint，metadata API 回傳 builtin tracker URL 與 piece priority hint
- `browser-p2p-swarm-download`: 下載端優先使用內建 tracker，並根據 piece priority hint 調整下載順序；HTTP web seed 依 swarm seeder 數量動態限速

## Impact

- **後端** (`src-tauri/src/share.rs`): 新增 WebSocket upgrade handler 實作 tracker protocol；metadata API 擴充欄位；HTTP file serving 加入動態限速邏輯
- **前端** (嵌入式下載頁 JS): 修改 WebTorrent client 初始化，優先註冊內建 tracker；加入 piece priority 調整邏輯
- **依賴**: 可能需要 `tokio-tungstenite` 或 axum 內建 WebSocket 支援
- **相容性**: 外部公共 tracker 保留為 fallback，不影響已有下載端行為
