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

系統 MUST 提供下載頁初始化所需的 metadata API，至少包含 session id、檔案長度、piece 大小與 info hash 或 magnet 資訊，並包含 metadata 版本欄位以支援相容性；下載頁初始化流程 MUST 使用此 API，而非一般瀏覽器檔案下載流程。

#### Scenario: 下載頁初始化

- **WHEN** 瀏覽器載入分享頁並請求 metadata API
- **THEN** 系統回傳完整初始化資料與 metadata 版本，足以啟動後續 HTTP 與 P2P 下載流程

#### Scenario: API 版本不相容

- **WHEN** 下載頁使用不支援的 metadata 版本
- **THEN** 系統回應明確錯誤碼與升級提示，避免靜默失敗

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
