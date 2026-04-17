## MODIFIED Requirements

### Requirement: 多下載者間可交換片段

系統 MUST 允許多個下載者在同一 swarm 中交換已取得片段，以減少對單一分享者的重複請求。系統 MUST 透過 piece priority offset 策略確保不同下載端優先取得不同片段，最大化 P2P 交換效率。

#### Scenario: 第二位下載者加入 swarm

- **WHEN** 第二位使用者加入相同分享 session 並開始下載
- **THEN** 兩位下載者可互相提供可用片段，且分享者上傳負載相對單純 HTTP 模式下降

#### Scenario: 不同 offset 的下載者互相交換片段

- **WHEN** 兩位下載者分別從不同的 piece offset 開始下載
- **THEN** 在下載初期兩者即持有互補的 piece 子集，可透過 P2P 交換彼此缺少的片段，減少對 HTTP web seed 的依賴

### Requirement: 瀏覽器端可啟動混合下載

系統 MUST 讓瀏覽器端下載器可同時使用分享者 HTTP 來源與 P2P peers，並在初始化後自動開始下載；初始化 metadata MUST 透過程式化 API 取得，不得依賴一般瀏覽器檔案下載流程。下載端 MUST 優先使用內建 WebSocket tracker 建立 peer 連線，並根據 server 回傳的 `piecePriorityOffset` 調整 piece 下載順序。

#### Scenario: 首次連線啟動下載

- **WHEN** 使用者在瀏覽器開啟分享頁並點擊下載
- **THEN** 下載器先透過 metadata API 取得初始化資料（含 piecePriorityOffset 與 builtinTrackerUrl），再啟動 HTTP 與 P2P 連線並開始接收片段

#### Scenario: 優先使用內建 tracker

- **WHEN** 下載端初始化 WebTorrent client
- **THEN** client 最先連線至內建 tracker 進行 peer 發現，公共外部 tracker 作為備援

#### Scenario: metadata API 暫時失敗

- **WHEN** 使用者點擊下載時 metadata API 回應錯誤或逾時
- **THEN** 系統顯示可理解錯誤訊息並提供重試，且不觸發瀏覽器原生檔案下載行為
