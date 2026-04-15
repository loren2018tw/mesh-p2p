## Purpose

定義基於 File System Access API 的 WebTorrent chunk store 實作，取代 IndexedDB 暫存，直接在使用者授權的檔案系統目錄中建立檔案並進行 piece 級別的隨機讀寫。

## Requirements

### Requirement: chunk store 必須符合 WebTorrent chunk store 介面

`FileSystemChunkStore` MUST 實作 WebTorrent chunk store 所需的 `put(index, buf, cb)`、`get(index, opts, cb)`、`close(cb)`、`destroy(cb)` 方法，使 WebTorrent 可直接作為 `store` 選項使用。

#### Scenario: WebTorrent 初始化時指定 FileSystemChunkStore

- **WHEN** WebTorrent client 透過 `client.add(torrent, { store: FileSystemChunkStore })` 啟動下載
- **THEN** WebTorrent MUST 成功初始化 torrent 並開始接收 piece

#### Scenario: put 寫入 piece 到檔案系統

- **WHEN** WebTorrent 呼叫 `put(index, buf, cb)` 寫入一個 piece
- **THEN** chunk store MUST 將 `buf` 寫入目標檔案的 `index * chunkLength` 偏移位置，且寫入完成後呼叫 `cb(null)`

#### Scenario: get 讀取已寫入的 piece

- **WHEN** WebTorrent 呼叫 `get(index, opts, cb)` 讀取先前已 put 的 piece
- **THEN** chunk store MUST 從目標檔案的對應偏移位置讀取資料並透過 `cb(null, buf)` 回傳

### Requirement: chunk store 必須使用 File System Access API 操作檔案

`FileSystemChunkStore` MUST 透過 `FileSystemDirectoryHandle` 與 `FileSystemFileHandle` 進行所有檔案操作，MUST NOT 使用 IndexedDB、localStorage 或記憶體 buffer 作為主要儲存。

#### Scenario: 建立新檔案

- **WHEN** chunk store 初始化且目標檔案尚不存在
- **THEN** chunk store MUST 透過 `directoryHandle.getFileHandle(fileName, { create: true })` 建立檔案

#### Scenario: 寫入使用 createWritable

- **WHEN** chunk store 需要寫入 piece
- **THEN** chunk store MUST 使用 `fileHandle.createWritable({ keepExistingData: true })` 開啟寫入串流，seek 到正確偏移並寫入後關閉

#### Scenario: 讀取使用 File.slice

- **WHEN** chunk store 需要讀取 piece
- **THEN** chunk store MUST 透過 `fileHandle.getFile()` 取得 File 物件並使用 `slice()` + `arrayBuffer()` 讀取對應區段

### Requirement: 並行寫入必須序列化

chunk store MUST 確保對同一檔案的寫入操作序列化執行，避免多個 `createWritable` 同時開啟導致衝突或資料損毀。

#### Scenario: 多個 piece 同時寫入

- **WHEN** WebTorrent 在短時間內對不同 index 連續呼叫多次 `put`
- **THEN** chunk store MUST 將所有寫入排入佇列依序執行，每次寫入完成後才開始下一次寫入

#### Scenario: 寫入佇列不阻塞讀取

- **WHEN** 寫入佇列有待處理項目時，WebTorrent 呼叫 `get` 讀取
- **THEN** 讀取操作 MUST 可在寫入佇列之外獨立執行，不被寫入佇列阻塞

### Requirement: 檔案寫入失敗必須回報錯誤

chunk store MUST 在檔案系統操作失敗時（如磁碟空間不足、權限遺失）透過 callback 回報錯誤，使 WebTorrent 與上層 UI 可顯示適當錯誤訊息。

#### Scenario: 磁碟空間不足

- **WHEN** `put` 操作因磁碟空間不足而失敗
- **THEN** chunk store MUST 透過 `cb(error)` 回報錯誤，不得靜默忽略

#### Scenario: 權限遺失

- **WHEN** 使用者在下載過程中撤銷目錄存取權限
- **THEN** chunk store MUST 透過 `cb(error)` 回報權限錯誤

### Requirement: destroy 必須清理已建立的檔案

呼叫 `destroy(cb)` 時，chunk store MUST 嘗試刪除先前建立的目標檔案，並在完成後呼叫 `cb(null)`。若刪除失敗（如檔案已被其他程式開啟），MUST 透過 `cb(error)` 回報但不影響其他操作。

#### Scenario: 使用者停止下載後清理

- **WHEN** 使用者停止一個進行中的下載，WebTorrent 呼叫 `destroy`
- **THEN** chunk store MUST 刪除目標檔案並釋放所有 handle
