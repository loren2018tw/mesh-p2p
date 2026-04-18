## Purpose

定義本機分享伺服器對瀏覽器下載頁提供的頁面、連線位址、metadata API 與 session 生命週期行為。

## Requirements

### Requirement: 分享頁面可由瀏覽器存取

系統 MUST 在分享啟動後提供可由瀏覽器直接連線的 HTTP 頁面，且頁面必須顯示分享檔案清單、總大小與目前分享狀態。

#### Scenario: 分享啟動後取得頁面

- **WHEN** 使用者在桌面程式啟動檔案分享
- **THEN** 系統提供可連線 URL，且瀏覽器開啟後可看到對應檔案資訊

### Requirement: 分享連結必須使用主機可存取 IP

系統 MUST 在分享啟動後回傳帶有目前主機可存取 IP 的分享 URL，而非僅限 loopback 位址。

#### Scenario: 區域網路裝置存取分享連結

- **WHEN** 分享者在同一網段啟動分享
- **THEN** 系統回傳的分享 URL 使用主機 IP 與對應 port，其他裝置可直接使用該連結連線

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

### Requirement: 分享狀態資訊必須可被人類理解

系統 MUST 提供可直接呈現在 app 右側資訊面板的狀態資訊 API，至少包含分享狀態、可達性、活躍連線數與最近錯誤摘要。

#### Scenario: app 更新右側資訊面板

- **WHEN** app 定期請求狀態資訊 API
- **THEN** 系統回傳語意化欄位，前端可直接呈現「可分享」「傳輸中」「異常待處理」等狀態

### Requirement: 下載端可主動更新檔案清單

系統 MUST 讓下載端可定期重新取得 metadata，並在分享端追加檔案後主動更新可下載檔案清單。

#### Scenario: 分享端新增檔案後下載端同步清單

- **WHEN** 分享端在既有分享 session 中加入新檔案
- **THEN** 已開啟下載頁的使用者端可於下一次 metadata 更新時看到新的檔案清單與版本號

### Requirement: 分享 session 生命週期可控

系統 MUST 支援停止分享動作，停止後不得再提供新下載初始化資料，且既有資料路由必須回應已失效狀態。

#### Scenario: 停止分享後拒絕初始化

- **WHEN** 分享者按下停止分享
- **THEN** 後續對 metadata API 的請求回應 session 已失效，且不再建立新下載連線

### Requirement: HTTP web seed 採用「Seeder 培養」名額制（取代均勻限速）

系統 MUST 維護固定數量的「全速下載名額」（fast slots，預設 2 個），只有持有名額的 client 可全速透過 HTTP 下載。未持有名額的 client 的 HTTP range request MUST 被大幅延遲（每 chunk ≥ 2000ms），迫使其依賴 P2P 取得資料。當持有名額的 client 完成下載（回報 is_seeding=true）後，名額 MUST 自動釋放給下一位 client。

#### Scenario: 前 N 位 client 自動獲得 fast slot

- **WHEN** 前兩位 client 開始發送 HTTP range request 下載檔案
- **THEN** 這兩位 client 自動獲得 fast slot，HTTP 回應無延遲，以全速下載

#### Scenario: fast slot 已滿時新 client 被大幅延遲

- **WHEN** 兩個 fast slot 已被占用，第三位 client 發送 HTTP range request
- **THEN** 該 client 的每個 chunk 回應被延遲至少 2000ms，迫使 WebTorrent 排程器優先使用 P2P 來源

#### Scenario: fast slot client 完成下載後名額釋放

- **WHEN** 持有 fast slot 的 client 透過 client-stats API 回報 `isSeeding: true`
- **THEN** 該 client 的 fast slot 立即釋放，下一位發送 range request 的 non-slot client 自動獲得名額

#### Scenario: fast slot 超時保護

- **WHEN** 持有 fast slot 的 client 超過 60 秒未發送任何 range request
- **THEN** 該 slot 自動釋放，防止 slot 被無活動 client 永久占用

#### Scenario: non-slot client 仍有 HTTP fallback

- **WHEN** 未持有 fast slot 的 client 無法從 P2P peer 取得資料
- **THEN** 該 client 仍可透過延遲後的 HTTP range request 取得資料（極慢但不會完全中斷）

### Requirement: 內建 tracker WebSocket endpoint 可用

系統 MUST 在分享伺服器的 `/announce` 路徑提供 WebSocket upgrade 支援，作為 `builtin-websocket-tracker` capability 的路由入口。

#### Scenario: WebSocket upgrade 請求

- **WHEN** 下載端對 `wss://{host}/announce` 發送 WebSocket upgrade 請求
- **THEN** 伺服器接受 upgrade 並建立 WebSocket 長連線
