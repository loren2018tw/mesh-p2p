## MODIFIED Requirements

### Requirement: 分享 API 提供必要下載描述

系統 MUST 提供下載頁初始化所需的 metadata API，至少包含 session id、檔案長度、piece 大小與 info hash 或 magnet 資訊，並包含 metadata 版本欄位以支援相容性；下載頁初始化流程 MUST 使用此 API，而非一般瀏覽器檔案下載流程。metadata API MUST 額外提供 torrent 初始化所需的版本修訂與相容性欄位。

#### Scenario: 下載頁初始化

- **WHEN** 瀏覽器載入分享頁並請求 metadata API
- **THEN** 系統回傳完整初始化資料與 metadata 版本，足以啟動後續 HTTP 與 P2P 下載流程

#### Scenario: API 版本不相容

- **WHEN** 下載頁使用不支援的 metadata 版本
- **THEN** 系統回應明確錯誤碼與升級提示，避免靜默失敗

#### Scenario: 下載頁依 metadata 啟動 WebTorrent session

- **WHEN** 下載頁讀取 metadata 並準備建立 torrent session
- **THEN** 系統提供的欄位足以讓下載頁完成版本檢查與 torrent 初始化

### Requirement: 分享狀態資訊必須可被人類理解

系統 MUST 提供可直接呈現在 app 右側資訊面板的狀態資訊 API，至少包含分享狀態、可達性、活躍連線數與最近錯誤摘要。狀態資訊 MUST 額外提供目前 seeding 端點數或等價指標，讓使用者可判讀協同上傳是否生效。

#### Scenario: app 更新右側資訊面板

- **WHEN** app 定期請求狀態資訊 API
- **THEN** 系統回傳語意化欄位，前端可直接呈現「可分享」「傳輸中」「異常待處理」等狀態

#### Scenario: 顯示協同分享是否生效

- **WHEN** 下載端已有至少一位完成且持續 seeding 的 peer
- **THEN** 狀態資訊 API 回傳可辨識欄位，前端可顯示協同分享已啟用
