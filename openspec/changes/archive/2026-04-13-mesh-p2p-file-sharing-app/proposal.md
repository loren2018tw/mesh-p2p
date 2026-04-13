## Why

目前在區域網路或小型社群中分享大型檔案時，常依賴單一 HTTP 來源，下載速度受分享者上傳頻寬限制，且多人同時下載時體驗快速惡化。需要一個桌面應用程式，讓使用者可快速挑選檔案並發佈下載頁面，同時利用 P2P 讓下載者彼此交換片段，以提升整體下載效率與可用性。

## What Changes

- 新增內建 Web Server，啟動後提供可由瀏覽器連線的下載頁面與分享資訊。
- 新增檔案分享流程，支援在程式中挑選欲分享檔案並自動建立可供 P2P 使用的種子中繼資料。
- 新增瀏覽器端下載體驗，讓使用者可從分享者下載，也可與其他下載者互相交換檔案片段（swarm）。
- 新增多使用者併發下載下的連線管理與基本狀態呈現，確保分享可持續且速度可擴展。

## Capabilities

### New Capabilities

- `local-share-web-server`: 提供本機可存取的下載入口頁，展示檔案資訊、連線狀態與下載啟動流程。
- `file-seed-publisher`: 將使用者挑選檔案轉換為可供 P2P 分發的種子中繼資料，並對外提供必要下載描述。
- `browser-p2p-swarm-download`: 在瀏覽器端建立 P2P 下載流程，支援 peer 片段交換以加速多人下載。

### Modified Capabilities

- 無。

## Impact

- 前端：`src/` 需新增下載頁與狀態 UI，處理瀏覽器端 P2P 初始化與事件顯示。
- 後端：`src-tauri/src/` 需新增檔案選取、種子建立、分享 session 管理與內建 Web Server 啟停邏輯。
- 設定：可能需調整 `src-tauri/tauri.conf.json` 與 capability 權限，開放必要網路/檔案存取。
- 依賴：可能新增 P2P/WebRTC 或 torrent 相關 Rust 與前端套件，並引入 tracker/announce 設定管理。
