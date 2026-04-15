## 1. 瀏覽器支援檢查與目錄授權

- [x] 1.1 在 `onMounted` 中新增 `browserSupported` ref，檢查 `window.showDirectoryPicker` 是否存在；不支援時設為 false 並將 `statusText` 設為不支援提示訊息
- [x] 1.2 新增 `directoryReady` ref 與 `dirHandle` ref；瀏覽器支援時嘗試從 IndexedDB 恢復已持久化的目錄 handle 並驗證權限
- [x] 1.3 在 UI template 中新增不支援瀏覽器的全頁提示（`v-alert type="error"`），當 `browserSupported` 為 false 時隱藏整個檔案清單與下載功能區域
- [x] 1.4 新增「選擇下載資料夾」按鈕，點擊後呼叫 `showDirectoryPicker({ mode: 'readwrite' })`，成功後將 handle 持久化到 IndexedDB 並設 `directoryReady = true`
- [x] 1.5 未授權目錄時，下載按鈕顯示為 disabled 並於上方顯示提示文字引導使用者先選擇資料夾

## 2. 實作 FileSystemChunkStore

- [x] 2.1 在下載頁面 JavaScript 中新增 `FileSystemChunkStore` class，constructor 接收 `chunkLength` 與 `opts`（含 `length`、`fileHandle`）
- [x] 2.2 實作 `put(index, buf, cb)` 方法：開啟 `createWritable({ keepExistingData: true })`，seek 到 `index * chunkLength`，寫入 buf，close writable，呼叫 `cb(null)`
- [x] 2.3 實作 write queue 序列化機制，確保所有 `put` 操作依序執行，避免同一檔案多個 writable 同時開啟衝突
- [x] 2.4 實作 `get(index, opts, cb)` 方法：透過 `fileHandle.getFile()` 取得 File 物件，用 `slice(offset, offset + length).arrayBuffer()` 讀取對應區段
- [x] 2.5 實作 `close(cb)` 方法：清空內部狀態並呼叫 `cb(null)`
- [x] 2.6 實作 `destroy(cb)` 方法：嘗試透過 `directoryHandle.removeEntry(fileName)` 刪除檔案，成功或失敗皆呼叫 cb
- [x] 2.7 在 `put` 與 `get` 中加入錯誤處理，磁碟空間不足或權限遺失時透過 `cb(error)` 回報

## 3. 整合 WebTorrent 下載流程

- [x] 3.1 修改 `downloadFile()` 函式：移除 `showDirectoryPicker` / `showSaveFilePicker` 前置檢查，改為直接使用已授權的 `dirHandle`
- [x] 3.2 在 `downloadFile()` 中透過 `dirHandle.getFileHandle(file.fileName, { create: true })` 取得 fileHandle，傳入 `FileSystemChunkStore` 作為 WebTorrent `store` 選項
- [x] 3.3 修改 `torrent.on('done')` handler：移除所有儲存相關呼叫，直接設 `phase = 'seeding'`，不再呼叫 `saveFromIdbStore` 或 `persistTorrentFile`
- [x] 3.4 確保 `destroySession()` 呼叫 torrent destroy 時使用 `{ destroyStore: false }` 以保留已下載的檔案

## 4. 移除舊有 IDB 與 Blob fallback 程式碼

- [x] 4.1 移除 `IdbChunkStore` class 整個實作
- [x] 4.2 移除 `saveCompletedFile()`、`saveFromIdbStore()`、`persistTorrentFile()`、`saveBlob()` 函式
- [x] 4.3 移除 `saveBusy` ref 與相關 UI 狀態
- [x] 4.4 移除 `MAX_BLOB_SAVE_BYTES` 常數與所有 blob 大小限制邏輯
- [x] 4.5 移除 `ensureSaveDirectoryReady()` 函式（其功能已在步驟 1 中整合至頁面初始化流程）
- [x] 4.6 移除 `openSaveDirDb()`、`loadSavedDirectoryHandle()`、`persistDirectoryHandle()`、`hasDirectoryWritePermission()` 中與舊流程重複的部分，保留目錄 handle 持久化邏輯但簡化整合至新流程

## 5. 更新 UI Template

- [x] 5.1 移除檔案清單中的「儲存」按鈕（`v-btn` with `saveCompletedFile`）
- [x] 5.2 移除 `downloaded` phase 相關標示，將 `phaseLabel` 中 `seeding` 的文字改為「分享中」
- [x] 5.3 在檔案清單上方或卡片區域顯示已授權目錄名稱，並提供「變更資料夾」按鈕
- [x] 5.4 下載按鈕在 `directoryReady` 為 false 時顯示 disabled 狀態
- [x] 5.5 調整 `phaseColor` 與 `phaseLabel`：移除 `downloaded` phase，`seeding` 對應「分享中」(success color)

## 6. 驗證與邊界情境

- [ ] 6.1 測試大檔案（>1 GB）下載是否能直接寫入檔案系統而不 OOM
- [ ] 6.2 測試多檔案同時下載時 `FileSystemChunkStore` 各自獨立 write queue 不相互衝突
- [ ] 6.3 測試不支援瀏覽器（Firefox）載入頁面時是否正確顯示不支援提示
- [ ] 6.4 測試頁面重新載入後能否自動恢復先前授權的目錄 handle
- [ ] 6.5 測試下載過程中停止（`stopTransfer`）後檔案是否被正確清理
