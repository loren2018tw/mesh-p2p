## Why

目前下載架構使用 IndexedDB (`IdbChunkStore`) 作為 WebTorrent 的 piece 暫存，下載完成後再由使用者手動觸發「儲存」按鈕將資料從 IDB 串流寫入檔案系統。這造成：

1. **雙重儲存浪費**：資料先寫進 IDB 再複製到檔案系統，大檔案佔用兩倍儲存空間。
2. **手動儲存步驟不直覺**：使用者下載完還要按「儲存」，且無法在下載過程中直接取用部分檔案。
3. **IDB 容量與效能限制**：瀏覽器對 IndexedDB 有 quota 限制，大量 piece 讀寫也造成 GC 壓力。

改用 File System Access API 直接寫入檔案系統，可消除中間層、自動完成儲存、並讓 WebTorrent 直接從檔案系統讀回已下載的 piece 參與 P2P seeding，降低記憶體壓力。

## What Changes

- **移除 `IdbChunkStore`**：不再使用 IndexedDB 暫存 WebTorrent piece。
- **新增 `FileSystemChunkStore`**：實作 WebTorrent chunk store 介面，透過 File System Access API 直接讀寫目標檔案。
- **強制要求 File System Access API**：客戶端連線後立即檢查瀏覽器支援度，不支援則顯示提示訊息，不提供降級 fallback 下載。
- **連線時主動要求授權下載目錄**：載入下載頁面後第一步即透過 `showDirectoryPicker()` 要求使用者授權儲存位置。
- **移除手動儲存按鈕與流程**：下載完成即代表檔案已在磁碟上，無須額外儲存步驟。
- **下載完成後自動轉為 seeding 狀態**：UI 直接從「下載中」變成「分享中」，檔案清單行內顯示進度。
- **移除 blob fallback 與 `showSaveFilePicker` 單檔降級路徑**。

## Capabilities

### New Capabilities

- `filesystem-chunk-store`: 基於 File System Access API 的 WebTorrent chunk store 實作，取代 IdbChunkStore，直接在授權目錄中建立檔案並進行 piece 級別的隨機讀寫。

### Modified Capabilities

- `browser-p2p-swarm-download`: 下載流程不再使用 IDB 暫存，改為 File System Access API 直寫；移除手動儲存步驟與 blob fallback；強制要求瀏覽器支援 File System Access API。

## Impact

- **前端下載頁面（`share.rs` 內嵌 HTML/JS）**：重寫 chunk store、downloadFile、saveCompletedFile 等函式；移除 IdbChunkStore 類別與相關 IDB 工具函式；新增瀏覽器支援度檢查 gate。
- **UI 互動**：移除「儲存」按鈕；下載完成自動切換「分享中」狀態；檔案清單每行直接內嵌進度條。
- **瀏覽器相容性**：僅支援 Chromium 系（Chrome、Edge、Opera）和部份支援的瀏覽器；Firefox、Safari 使用者將看到不支援提示。
- **現有 spec**：`browser-p2p-swarm-download` spec 需更新下載流程及儲存相關 requirement。
