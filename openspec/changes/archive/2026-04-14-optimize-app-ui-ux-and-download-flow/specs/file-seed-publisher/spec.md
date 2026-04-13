## MODIFIED Requirements

### Requirement: 分享端可選取檔案並建立分享 session

系統 MUST 允許分享者在程式內選取一個或多個檔案並建立分享 session，且必須驗證每個檔案存在、可讀與大小資訊；建立後 MUST 提供可供 UI 面板顯示的 session 摘要資料（包含檔案數、總大小、建立時間與當前狀態）。

#### Scenario: 選取有效檔案建立 session

- **WHEN** 使用者選取一個或多個存在且可讀的檔案並確認分享
- **THEN** 系統建立新 session 並回傳所有檔案基本資訊、session id 與摘要資料

#### Scenario: 分享中追加檔案

- **WHEN** 分享已啟動且分享者再加入新的有效檔案
- **THEN** 系統將新檔案加入既有 session，更新可供下載端取得的分享清單，並同步更新摘要資料

### Requirement: 系統必須提供傳輸摘要供 UI 顯示

系統 MUST 提供可供 app 右側資訊面板使用的傳輸摘要資料，至少包含活躍下載數、近期錯誤狀態與最近活動時間。

#### Scenario: UI 請求傳輸摘要

- **WHEN** app UI 讀取當前分享 session 的狀態摘要
- **THEN** 系統回傳完整且可顯示的人類可讀狀態欄位，不需由前端推測底層狀態
