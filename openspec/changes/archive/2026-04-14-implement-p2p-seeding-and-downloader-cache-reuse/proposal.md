## Why

目前下載頁雖有混合 HTTP/P2P 的規格目標，但實際流程仍以 HTTP 下載為主，尚未形成穩定的 WebTorrent 片段交換路徑。這導致分享者負載集中、重複分享需要重建種子、下載者重開或重下時無法重用已存在資料，降低整體傳輸效率與使用體驗。

## What Changes

- 導入可運作的 WebTorrent 傳輸流程：下載端使用 metadata API 初始化 torrent，並與分享端及其他下載端交換片段。
- 分享端新增種子檔持久化與重用：建立分享時將 torrent metadata（或等價種子描述）儲存在原始檔案同目錄，下次分享先驗證檔案指紋與種子一致，若一致則略過重建流程。
- 建立下載完成後持續 seeding 的生命週期與控制策略（例如：頁面關閉停止、手動停止、session 失效時回收）。
- 補齊前後端狀態欄位與錯誤語意，讓 UI 能區分「下載中」「驗證中」「已完成且分享中」「需重新校驗」等狀態。

## Capabilities

### New Capabilities

- 無。

### Modified Capabilities

- `browser-p2p-swarm-download`: 將規格從目標性描述補強為可執行的 WebTorrent 流程，明確定義完成後持續 seeding、peer 交換與回收條件。
- `file-seed-publisher`: 新增分享端種子檔落地、驗證與重用規則，降低重複分享時的 CPU/hash 建置成本。
- `local-share-web-server`: 補充 metadata/API 欄位與狀態語意，支援 torrent 初始化一致性與版本相容處理。

## Impact

- 前端下載頁（嵌入於 `src-tauri/src/share.rs` 的 HTML/JS）將加入 WebTorrent client lifecycle 與完成後 seeding 控制。
- Rust 後端分享服務需新增種子索引/指紋驗證與 metadata 擴充欄位，並維持舊版相容或提供明確版本錯誤。
- 可能新增前端依賴（如 `webtorrent`）與相關型別/封裝層。
- 狀態面板與 metrics/API 需擴充，以支援觀測 active peers、上傳來源與 seeding 狀態。
