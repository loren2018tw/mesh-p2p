## 1. 內建 WebSocket Tracker 基礎建設

- [x] 1.1 在 `Cargo.toml` 加入 WebSocket 相關依賴（啟用 axum 的 `ws` feature）
- [x] 1.2 在 `share.rs` 新增 tracker 資料結構：`TrackerState`（含 `HashMap<InfoHash, HashMap<PeerId, WsSender>>` swarm 管理、連線數 counter、連線上限常數 200）
- [x] 1.3 實作 `/announce` WebSocket upgrade handler，包含連線數上限檢查（超過回傳 503）
- [x] 1.4 實作 WebSocket 訊息解析：解析 JSON announce 訊息（action, info_hash, peer_id, offers, answer, offer_id, to_peer_id, event）
- [x] 1.5 實作 announce 處理邏輯：將 peer 註冊到對應 info_hash 的 swarm，將 offers 分別轉發給 swarm 中的其他 peers，回傳 announce response（含 complete, incomplete, interval）
- [x] 1.6 實作 answer 轉發邏輯：根據 `to_peer_id` 將 answer 轉發到指定 peer
- [x] 1.7 實作 peer 斷線清理：WebSocket 關閉時從所有 swarm 移除該 peer，更新 complete/incomplete 計數

## 2. Tracker 整合到現有路由

- [x] 2.1 將 `TrackerState` 加入 `ShareRuntime`，在 `start_sharing` 時初始化
- [x] 2.2 在 axum Router 註冊 `/announce` WebSocket route
- [x] 2.3 修改 `build_torrent_bytes()` 與 `build_seed_metadata()`：將內建 tracker URL（`wss://{host}/announce`）作為 announce-list 第一項，外部 tracker 排列在後
- [x] 2.4 在分享停止時清理所有 tracker WebSocket 連線與 swarm 狀態

## 3. Piece 多樣性策略 — Server 端

- [x] 3.1 在 `ShareRuntime` 中為每個 file 新增 `next_piece_offset: AtomicU64` counter
- [x] 3.2 修改 metadata API handler：每次請求時遞增 counter，計算 `piecePriorityOffset = (counter * totalPieces / NUM_SLICES) % totalPieces`，加入回應 JSON
- [x] 3.3 在 metadata API 回應中加入 `builtinTrackerUrl` 欄位（`wss://{host}/announce`）

## 4. Piece 多樣性策略 — Client 端

- [x] 4.1 修改下載頁 JS 的 `loadMetadata()` 函式：從 API 回應中讀取 `piecePriorityOffset` 與 `builtinTrackerUrl`，存入對應狀態
- [x] 4.2 修改 `downloadFile()` 函式：在 torrent ready 後，根據 `piecePriorityOffset` 將 pieces 分為高低優先級群組，使用 `torrent.select(start, end, priority)` 設定優先順序
- [x] 4.3 確保所有 pieces 最終都會被下載（低優先級群組 priority > 0）

## 5. HTTP Web Seed 動態限速

- [x] 5.1 新增函式 `compute_http_throttle_delay()`：根據 `ShareRuntime` 中的 seeding_peer_count 計算每個 chunk 的延遲時間（0 seeder → 0ms, 1-3 → 50ms, 4-7 → 150ms, ≥8 → 300ms）
- [x] 5.2 修改 `file_handler`（HTTP range request handler）：在每個 chunk 寫入 response body 後，呼叫 `tokio::time::sleep` 插入對應延遲
- [x] 5.3 確認 seeding_peer_count 統計包含 tracker 內的 complete peers 數量（結合 client-stats 與 tracker swarm 資料）

## 6. 驗證與測試

- [ ] 6.1 手動測試：啟動分享，用兩個瀏覽器同時下載，確認兩者透過內建 tracker 互相發現並建立 P2P 連線
- [x] 6.2 驗證 piece diversity：觀察兩個 client 的下載順序確實不同，P2P uploaded bytes 開始成長
- [x] 6.3 驗證 HTTP 限速生效：當有 seeder 時，HTTP fallback 次數增長速度明顯降低
- [ ] 6.4 驗證 fallback：斷開內建 tracker 連線後，client 仍可透過外部 tracker 或 HTTP web seed 完成下載
