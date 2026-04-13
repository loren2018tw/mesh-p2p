## Why

目前 app 與使用者端網頁介面在一致性、可理解性與下載體驗上存在落差：UI 缺少統一元件系統、使用者難以掌握下載進度，且 P2P metadata 下載若走一般瀏覽器下載流程可能被封鎖。這些問題直接影響可用性與成功率，因此需要優先優化。

## What Changes

- 在 app 與使用者端 UI 導入 Vuetify，建立一致且可維護的 Vue + Vuetify 元件化介面。
- 在使用者端下載流程加入可視化傳輸進度（例如百分比、速度、剩餘時間、狀態）。
- 調整 P2P metadata 取得機制，避免使用一般網頁下載流程，改用可控且不易被瀏覽器封鎖的程式化流程。
- 重新設計 app 右側資訊區，改為可理解、可行動的資訊呈現（例如連線狀態、分享摘要、下載狀態與錯誤提示）。

## Capabilities

### New Capabilities

- `vuetify-ui-foundation`: 建立 app 與使用者端共用的 Vuetify-based UI foundation 與核心版型/元件規範。

### Modified Capabilities

- `browser-p2p-swarm-download`: 擴充下載狀態可視化與 metadata 取得流程，避免依賴一般瀏覽器下載行為。
- `file-seed-publisher`: 補強可供前端資訊面板使用的分享與傳輸摘要資料輸出。
- `local-share-web-server`: 調整本機分享頁面的資料呈現與狀態資訊輸出契約，支援更有意義的 UI 顯示。

## Impact

- 前端依賴：新增或擴大使用 Vuetify 相關套件與樣式設定。
- 前端程式：`src/` 下 Vue 元件、狀態管理與下載流程 UI 需重構。
- Tauri/Rust：`src-tauri/` 可能需調整回傳資訊與事件資料，支援右側資訊區與下載狀態顯示。
- 規格層：需新增 1 個 capability spec，並更新既有下載/分享/本機服務 capability 規格。
