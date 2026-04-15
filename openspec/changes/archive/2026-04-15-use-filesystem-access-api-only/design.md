## Context

目前 Mesh P2P 下載頁面使用 `IdbChunkStore`（IndexedDB）暫存 WebTorrent 下載的 piece，下載完成後需由使用者手動按「儲存」按鈕，透過 `saveFromIdbStore()` 或 `persistTorrentFile()` 將資料從 IDB 串流寫入檔案系統。此架構存在雙重儲存浪費、手動操作摩擦、以及 IDB quota 限制等問題。

瀏覽器端下載頁面嵌入在 `src-tauri/src/share.rs` 的 `download_page_handler` 函式中，包含整個 Vue 3 + Vuetify 應用程式的 HTML/JS。目前已部分使用 File System Access API（`showDirectoryPicker`、目錄 handle 持久化），但僅用於儲存階段，未用於 WebTorrent 的 chunk store。

## Goals / Non-Goals

**Goals:**

- WebTorrent 的 piece 直接寫入使用者授權的檔案系統目錄，不經過 IndexedDB。
- 下載完成即代表檔案已存在於磁碟，無須任何手動儲存步驟。
- 已下載的 piece 直接從檔案系統讀回，參與 P2P seeding 分享。
- 不支援 File System Access API 的瀏覽器在頁面載入時即顯示明確提示，不提供降級路徑。
- 頁面載入後主動觸發目錄選擇器，取得下載目錄授權後才顯示檔案清單與下載功能。

**Non-Goals:**

- 不在此變更中支援 Firefox/Safari 等不支援 File System Access API 的瀏覽器降級下載。
- 不變更 Rust 後端（`share.rs`）的 HTTP API 行為或 torrent 生成邏輯。
- 不變更桌面端（Tauri）的分享功能或 UI。
- 不處理斷點續傳跨 session 的持久化（關閉瀏覽器後重開繼續下載）。

## Decisions

### Decision 1: 實作 `FileSystemChunkStore` 取代 `IdbChunkStore`

**選擇**: 在下載頁面內嵌 JavaScript 中實作新的 `FileSystemChunkStore` class，符合 WebTorrent 的 chunk store 介面（`put(index, buf, cb)`、`get(index, opts, cb)`、`close(cb)`、`destroy(cb)`）。

**做法**:

- 透過 `FileSystemDirectoryHandle.getFileHandle(fileName, { create: true })` 在授權目錄中建立目標檔案。
- 使用 `createSyncAccessHandle()` 或 `createWritable()` 進行隨機位置寫入／讀取。
- 因 `createSyncAccessHandle()` 僅在 Web Worker 中可用，且 WebTorrent 在主執行緒運作，故使用 `FileSystemFileHandle` 搭配 seek + write 操作。
- 具體做法：每次 `put` 時開啟 `createWritable({ keepExistingData: true })`，seek 到 `index * chunkLength` 位置寫入 chunk，然後 close writable。每次 `get` 時透過 `fileHandle.getFile()` 取得 File 物件，再使用 `slice()` + `arrayBuffer()` 讀取對應區段。

**替代方案考慮**:

- 繼續使用 IDB 但自動儲存：仍有雙重寫入問題，未根本解決。
- 使用 Origin Private File System (OPFS)：效能更好但檔案對使用者不可見，違背「下載完成即可取用」目標。

### Decision 2: 頁面載入時強制檢查與授權

**選擇**: 在 `onMounted` 階段先檢查 `window.showDirectoryPicker` 是否存在，不存在則顯示不支援提示並阻止所有下載功能。通過檢查後立即呼叫 `showDirectoryPicker()` 要求使用者授權下載目錄。

**做法**:

- 新增 `browserSupported` ref，`onMounted` 時設置。
- 新增 `directoryReady` ref，授權成功後設為 true。
- metadata 仍然持續載入以顯示檔案清單，但下載按鈕在 `directoryReady` 為 false 時不可用。
- 提供「選擇下載資料夾」按鈕讓使用者可手動重新授權。

### Decision 3: 移除所有 IDB / Blob fallback 程式碼

**選擇**: 完全移除 `IdbChunkStore` class、`saveCompletedFile()`、`saveFromIdbStore()`、`persistTorrentFile()`、`saveBlob()`、`saveBusy` ref、`openSaveDirDb()`/`loadSavedDirectoryHandle()`/`persistDirectoryHandle()` 等 IDB 持久化工具函式、以及 UI 中的「儲存」按鈕。

**理由**: 不提供降級路徑，保持程式碼簡潔。不支援的瀏覽器在入口就被攔截。

### Decision 4: 下載完成後自動轉 seeding 狀態

**選擇**: WebTorrent `done` 事件觸發後，直接將 phase 設為 `seeding`，因為檔案已在磁碟上。UI 從「下載中」（顯示進度條）直接轉為「分享中」chip。

**做法**:

- `done` handler 中：設 `phase = 'seeding'`，不再呼叫任何儲存函式。
- 移除 `downloaded` phase（不再需要區分「已下載未儲存」狀態）。
- UI 的 `phaseLabel` 更新：`seeding` → 「分享中」。

### Decision 5: 目錄 handle 記憶機制

**選擇**: 保留使用 IndexedDB 持久化 `FileSystemDirectoryHandle` 的機制（`mesh-p2p-app` 資料庫），以便使用者重新載入頁面時不必重新選擇目錄。但簡化為只在檢查權限失敗時才觸發 picker。

## Risks / Trade-offs

- **[瀏覽器相容性受限]** → File System Access API 目前僅 Chromium 系瀏覽器完整支援。Firefox 與 Safari 使用者將無法使用下載功能。此為有意的 trade-off，以換取更簡潔的架構與更好的大檔案支援。透過明確的不支援提示降低使用者困惑。

- **[`createWritable` 效能]** → 每次 `put` 都呼叫 `createWritable({ keepExistingData: true })` + close 可能有開銷。若效能不佳，可考慮批次寫入或維持單一 writable 的方式最佳化，但初期先以正確性為主。

- **[並行寫入衝突]** → WebTorrent 可能同時對不同 piece index 呼叫 `put`。`createWritable({ keepExistingData: true })` 在單一檔案上同時開啟多個 writable 可能失敗。需在 `FileSystemChunkStore` 中實作寫入佇列 (write queue) 確保序列化。

- **[磁碟空間不足]** → 寫入檔案系統可能因空間不足而失敗。需在 `put` 的錯誤回呼中妥善處理並顯示錯誤訊息。

## Open Questions

- `createWritable({ keepExistingData: true })` 的並行操作行為是否穩定？可能需要實測後決定是否改用 write queue 或 OPFS + `createSyncAccessHandle`（Web Worker 方案）。
