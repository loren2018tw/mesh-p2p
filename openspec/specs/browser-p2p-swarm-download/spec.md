## Purpose

定義瀏覽器端下載器在混合 HTTP 與 P2P swarm 模式下的行為，確保下載流程在多人協作與異常情境中仍可持續且正確。

## Requirements

### Requirement: 瀏覽器端可啟動混合下載

系統 MUST 讓瀏覽器端下載器可同時使用分享者 HTTP 來源與 P2P peers，並在初始化後自動開始下載；初始化 metadata MUST 透過程式化 API 取得，不得依賴一般瀏覽器檔案下載流程。

#### Scenario: 首次連線啟動下載

- **WHEN** 使用者在瀏覽器開啟分享頁並點擊下載
- **THEN** 下載器先透過 metadata API 取得初始化資料，再啟動 HTTP 與 P2P 連線並開始接收片段

#### Scenario: metadata API 暫時失敗

- **WHEN** 使用者點擊下載時 metadata API 回應錯誤或逾時
- **THEN** 系統顯示可理解錯誤訊息並提供重試，且不觸發瀏覽器原生檔案下載行為

### Requirement: 下載進度必須可視化

系統 MUST 即時顯示下載進度資訊，至少包含百分比、已下載大小、總大小、目前速度與預估剩餘時間。

#### Scenario: 下載進行中顯示進度

- **WHEN** 下載器持續接收片段
- **THEN** 使用者端 UI 以固定節奏更新進度資訊，且進度值與實際接收量一致

#### Scenario: 下載來源切換時維持可讀狀態

- **WHEN** 下載流程在 P2P 與 HTTP 回源間切換
- **THEN** UI 持續顯示目前可用來源狀態與進度，不得中斷或倒退到未知狀態

### Requirement: 下載清單項目必須可直接操作並標示狀態

系統 MUST 在下載清單中為每個可下載檔案提供獨立下載按鈕；當項目下載進行中時，該項目後方 MUST 顯示對應進度列；下載完成後該項目 MUST 明確標示「分享中」狀態。系統 MUST NOT 顯示手動儲存按鈕。

#### Scenario: 使用者從下載清單啟動單一檔案下載

- **WHEN** 使用者在下載清單點擊某檔案的下載按鈕
- **THEN** 系統僅啟動該項目下載流程，並將該項目狀態更新為下載中

#### Scenario: 下載中在清單後方顯示進度列

- **WHEN** 清單中的檔案項目處於下載中
- **THEN** 該項目後方顯示可更新的進度列，且進度值隨實際接收量更新

#### Scenario: 下載完成後標示分享中

- **WHEN** 清單中的檔案項目下載完成且完整性驗證成功
- **THEN** 該項目狀態顯示為「分享中」，且不再顯示進行中進度列，亦不顯示儲存按鈕

### Requirement: 多下載者間可交換片段

系統 MUST 允許多個下載者在同一 swarm 中交換已取得片段，以減少對單一分享者的重複請求。

#### Scenario: 第二位下載者加入 swarm

- **WHEN** 第二位使用者加入相同分享 session 並開始下載
- **THEN** 兩位下載者可互相提供可用片段，且分享者上傳負載相對單純 HTTP 模式下降

### Requirement: 檔案完整性必須驗證

系統 MUST 在下載完成前驗證所有片段 hash，任何驗證失敗片段都必須重新抓取直到通過。

#### Scenario: 發生損毀片段

- **WHEN** 下載器收到 hash 不一致的片段
- **THEN** 系統丟棄該片段並重新從 peer 或 HTTP 來源抓取，直到完整檔案驗證成功

### Requirement: P2P 不可用時保持可下載

系統 MUST 在無可用 peers 或 tracker 暫時失效時，仍可透過 HTTP 來源持續下載。

#### Scenario: tracker 暫時不可用

- **WHEN** 下載期間無法取得可用 peer 清單
- **THEN** 系統自動切換或維持 HTTP 回源下載，且下載流程不中斷

### Requirement: 瀏覽器必須支援 File System Access API 才可使用下載功能

系統 MUST 在頁面載入時檢查瀏覽器是否支援 `window.showDirectoryPicker`。若不支援，MUST 顯示明確的不支援提示訊息，且 MUST NOT 提供任何下載功能或降級 fallback。

#### Scenario: 不支援的瀏覽器載入下載頁面

- **WHEN** 使用者以不支援 File System Access API 的瀏覽器（如 Firefox、Safari）開啟下載頁面
- **THEN** 系統顯示「本系統僅支援 File System Access API 之瀏覽器（如 Chrome、Edge）」提示，且不顯示下載按鈕或檔案清單操作項

#### Scenario: 支援的瀏覽器載入下載頁面

- **WHEN** 使用者以支援 File System Access API 的瀏覽器（如 Chrome、Edge）開啟下載頁面
- **THEN** 系統正常顯示檔案清單與下載功能

### Requirement: 連線後必須先授權下載目錄

系統 MUST 在使用者可開始下載前，要求透過 `showDirectoryPicker()` 授權一個下載目錄。未授權前 MUST NOT 允許任何下載操作。

#### Scenario: 首次載入需要授權目錄

- **WHEN** 使用者首次載入下載頁面且尚無已授權目錄
- **THEN** 系統顯示提示並提供「選擇下載資料夾」按鈕，使用者點擊後觸發目錄選擇器

#### Scenario: 使用者取消目錄選擇

- **WHEN** 使用者在目錄選擇器中按取消
- **THEN** 系統維持「未授權」狀態，下載按鈕保持不可用，並顯示提示要求授權

#### Scenario: 使用者已有持久化的目錄授權

- **WHEN** 使用者重新載入頁面且先前已授權的目錄 handle 仍有效
- **THEN** 系統自動恢復該目錄授權，無須再次手動選擇

### Requirement: 下載完成後自動可用無須手動儲存

系統 MUST 在 WebTorrent 下載完成後直接將檔案視為已存在於使用者授權目錄中，MUST NOT 另行開啟寫入串流或要求使用者點擊「儲存」按鈕。下載完成即代表檔案已寫入完畢。

#### Scenario: 下載完成後檔案自動存在

- **WHEN** WebTorrent torrent 觸發 `done` 事件
- **THEN** 目標檔案已完整存在於使用者授權的下載目錄中，使用者可直接在檔案系統中存取

#### Scenario: 無須手動儲存步驟

- **WHEN** 下載完成
- **THEN** UI 不顯示「儲存」按鈕，直接從「下載中」轉為「分享中」狀態

### Requirement: 下載完成後自動進入 seeding 分享

系統 MUST 在下載完成後自動從檔案系統讀回已下載的 piece 參與 P2P seeding，持續為其他下載者提供 piece 交換。

#### Scenario: 下載完成後持續 seeding

- **WHEN** WebTorrent torrent 觸發 `done` 事件
- **THEN** torrent 繼續運行，chunk store 從檔案系統讀回 piece 提供給其他 peer，UI 顯示「分享中」
