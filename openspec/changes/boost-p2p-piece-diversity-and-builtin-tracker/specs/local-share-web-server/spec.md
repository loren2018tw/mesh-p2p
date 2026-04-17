## MODIFIED Requirements

### Requirement: 分享 API 提供必要下載描述

系統 MUST 提供下載頁初始化所需的 metadata API，至少包含 session id、檔案長度、piece 大小與 info hash 或 magnet 資訊，並包含 metadata 版本欄位以支援相容性；下載頁初始化流程 MUST 使用此 API，而非一般瀏覽器檔案下載流程。metadata API 回應 MUST 額外包含 `piecePriorityOffset`（piece 優先順序偏移值）與 `builtinTrackerUrl`（內建 WebSocket tracker URL）欄位。

#### Scenario: 下載頁初始化

- **WHEN** 瀏覽器載入分享頁並請求 metadata API
- **THEN** 系統回傳完整初始化資料與 metadata 版本，足以啟動後續 HTTP 與 P2P 下載流程

#### Scenario: API 版本不相容

- **WHEN** 下載頁使用不支援的 metadata 版本
- **THEN** 系統回應明確錯誤碼與升級提示，避免靜默失敗

#### Scenario: metadata 包含 piece priority offset 與內建 tracker URL

- **WHEN** 下載端請求 metadata API
- **THEN** 回應中包含 `piecePriorityOffset` 數值欄位與 `builtinTrackerUrl` 字串欄位，下載端可據此調整 piece 下載順序並優先連線內建 tracker

## ADDED Requirements

### Requirement: HTTP web seed 根據 swarm seeder 數量動態限速

系統 MUST 根據當前 swarm 中 seeding peer 數量，動態調整 HTTP file serving 的回應速率。seeding peers 越多，HTTP 回應速率越低，讓 P2P 有更多機會分擔流量。

#### Scenario: 無 seeding peer 時 HTTP 全速

- **WHEN** 目前 swarm 中無任何 seeding peer（所有 client 皆在下載中或尚未有 client）
- **THEN** HTTP file serving 不做任何限速，以最大速率回應 range request

#### Scenario: 少量 seeding peers 時 HTTP 適度限速

- **WHEN** swarm 中有 1 至 3 個 seeding peers
- **THEN** HTTP file serving 對每個 chunk 回應加入適度延遲（約 50ms/chunk），降低 HTTP 輸出速率約 50%

#### Scenario: 充足 seeding peers 時 HTTP 大幅限速

- **WHEN** swarm 中有 8 個以上 seeding peers
- **THEN** HTTP file serving 對每個 chunk 回應加入顯著延遲（約 300ms/chunk），HTTP 僅作為補漏角色

### Requirement: 內建 tracker WebSocket endpoint 可用

系統 MUST 在分享伺服器的 `/announce` 路徑提供 WebSocket upgrade 支援，作為 `builtin-websocket-tracker` capability 的路由入口。

#### Scenario: WebSocket upgrade 請求

- **WHEN** 下載端對 `wss://{host}/announce` 發送 WebSocket upgrade 請求
- **THEN** 伺服器接受 upgrade 並建立 WebSocket 長連線
