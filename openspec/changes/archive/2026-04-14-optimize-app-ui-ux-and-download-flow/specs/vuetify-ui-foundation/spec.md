## ADDED Requirements

### Requirement: App 與使用者端必須使用一致的 Vuetify 元件基礎

系統 MUST 在桌面 app 與使用者端頁面使用一致的 Vue + Vuetify 元件層，且關鍵互動（下載、分享、狀態提示）必須採用 Vuetify 元件與共用樣式 token。

#### Scenario: 新頁面建立時套用共用元件

- **WHEN** 開發者新增下載或分享相關頁面
- **THEN** 頁面使用 Vuetify 元件與專案定義的共用 theme/token，而非臨時自製樣式

#### Scenario: 既有關鍵流程頁面完成重構

- **WHEN** 使用者開啟 app 主畫面與瀏覽器下載頁
- **THEN** 兩者呈現一致的狀態標籤、進度元件與錯誤提示樣式，且互動行為一致
