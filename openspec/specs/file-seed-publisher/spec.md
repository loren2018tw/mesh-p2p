## Purpose

定義分享端如何將使用者選取的檔案轉換為可發佈於 BitTorrent 相容網路的種子資料，並支援分享內容持續更新。

## Requirements

### Requirement: 分享端可選取檔案並建立分享 session

系統 MUST 允許分享者在程式內選取一個或多個檔案並建立分享 session，且必須驗證每個檔案存在、可讀與大小資訊。

#### Scenario: 選取有效檔案建立 session

- **WHEN** 使用者選取一個或多個存在且可讀的檔案並確認分享
- **THEN** 系統建立新 session 並回傳所有檔案的基本資訊與 session id

#### Scenario: 分享中追加檔案

- **WHEN** 分享已啟動且分享者再加入新的有效檔案
- **THEN** 系統將新檔案加入既有 session，並更新可供下載端取得的分享清單

### Requirement: 系統自動產生 P2P 種子中繼資料

系統 MUST 對已選取檔案自動計算 piece 與 hash，產生可供 BitTorrent 相容客戶端使用的種子中繼資料。

#### Scenario: 種子建立成功

- **WHEN** 新分享 session 建立完成
- **THEN** 系統產生對應 torrent metadata 或 magnet，並可被下載頁初始化流程取得

### Requirement: 種子公告資訊可配置

系統 MUST 支援一組 announce/tracker 清單設定，並在產生種子資料時帶入公告資訊。

#### Scenario: 使用多個 tracker

- **WHEN** 系統設定包含多個 tracker URL
- **THEN** 產生的種子資料包含全部有效 tracker 項目，供 peer 探索使用
