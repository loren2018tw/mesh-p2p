## Purpose

定義分享伺服器上的內建 WebSocket tracker endpoint，用於在下載端之間轉發 WebRTC signaling 以建立 P2P 連線。

## Requirements

### Requirement: 分享端內建 WebSocket tracker endpoint

系統 MUST 在分享伺服器上提供 `/announce` WebSocket endpoint，實作 BitTorrent WebSocket tracker protocol，接受下載端的 announce 訊息並在同一 info_hash 的 peers 之間轉發 WebRTC signaling（offer/answer）。

#### Scenario: 下載端連線至內建 tracker

- **WHEN** 下載端的 WebTorrent client 以 WebSocket 連線至 `wss://{shareHost}/announce`
- **THEN** 伺服器接受 WebSocket upgrade 並維持長連線，準備接收 announce 訊息

#### Scenario: 下載端發送 announce 含 WebRTC offers

- **WHEN** 下載端透過 WebSocket 發送 JSON announce 訊息，包含 `action: "announce"`、`info_hash`、`peer_id` 及 `offers` 陣列（每個 offer 含 RTCSessionDescription 與 offer_id）
- **THEN** 伺服器將該 peer 註冊至對應 info_hash 的 swarm 中，並將每個 offer 分別轉發給 swarm 中的其他不同 peers

#### Scenario: 下載端發送 answer 回應

- **WHEN** 收到 offer 的 peer 透過 WebSocket 回傳 JSON answer 訊息，包含 `action: "announce"`、`answer`、`offer_id` 及 `to_peer_id`
- **THEN** 伺服器將 answer 轉發至 `to_peer_id` 指定的 peer，完成 WebRTC signaling

#### Scenario: peer 斷線自動清理

- **WHEN** 某個 peer 的 WebSocket 連線斷開（關閉頁面、網路中斷等）
- **THEN** 伺服器在 30 秒內將該 peer 從所有 swarm 中移除，後續不再將 offers 轉發給該 peer

### Requirement: tracker 回應必須包含 swarm 統計

系統 MUST 在 announce 回應中包含 `complete`（已完成下載的 seeder 數）與 `incomplete`（下載中的 leecher 數），以及建議的 `interval`（下次 announce 間隔秒數）。

#### Scenario: announce 回應含 swarm 統計

- **WHEN** 伺服器處理完 announce 訊息並將 offers 轉發後
- **THEN** 伺服器回傳 JSON 回應，包含 `action: "announce"`、`info_hash`、`complete`、`incomplete` 與 `interval` 欄位

### Requirement: tracker 連線數有上限保護

系統 MUST 限制同時連線的 WebSocket tracker 連線數上限（預設 200），超過上限時 MUST 拒絕新連線並回傳 HTTP 503 狀態碼。

#### Scenario: 連線數達到上限

- **WHEN** 同時有 200 個 WebSocket 連線已建立，第 201 個下載端嘗試連線
- **THEN** 伺服器回傳 HTTP 503 Service Unavailable，該下載端回退使用外部公共 tracker

### Requirement: tracker 優先於外部 tracker

系統 MUST 在生成 torrent metadata 時，將內建 tracker URL（`wss://{host}/announce`）放在 announce-list 的第一位，外部公共 tracker 排列在後。

#### Scenario: torrent 的 announce-list 順序

- **WHEN** 系統為分享檔案生成 torrent metadata
- **THEN** torrent 的 announce 欄位為內建 tracker URL，announce-list 中內建 tracker 為第一項，外部 tracker 依序排列在後
