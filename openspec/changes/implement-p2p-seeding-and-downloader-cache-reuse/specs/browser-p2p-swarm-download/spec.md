## MODIFIED Requirements

### Requirement: 瀏覽器端可啟動混合下載

系統 MUST 讓瀏覽器端下載器可同時使用分享者 HTTP 來源與 P2P peers，並在初始化後自動開始下載；初始化 metadata MUST 透過程式化 API 取得，不得依賴一般瀏覽器檔案下載流程。下載流程 MUST 以 WebTorrent 相容 client 啟動 torrent session，並在 P2P 不可用時維持 HTTP fallback。

#### Scenario: 首次連線啟動下載

- **WHEN** 使用者在瀏覽器開啟分享頁並點擊下載
- **THEN** 下載器先透過 metadata API 取得初始化資料，再以 WebTorrent session 啟動 HTTP 與 P2P 混合下載

#### Scenario: metadata API 暫時失敗

- **WHEN** 使用者點擊下載時 metadata API 回應錯誤或逾時
- **THEN** 系統顯示可理解錯誤訊息並提供重試，且不觸發瀏覽器原生檔案下載行為

### Requirement: 下載清單項目必須可直接操作並標示狀態

系統 MUST 在下載清單中為每個可下載檔案提供獨立下載按鈕；當項目下載進行中時，該項目後方 MUST 顯示對應進度列；下載完成後該項目 MUST 明確標示「已下載」狀態。當檔案已完整且已加入 seeding 時，狀態 MUST 顯示為「已下載並分享中」。

#### Scenario: 使用者從下載清單啟動單一檔案下載

- **WHEN** 使用者在下載清單點擊某檔案的下載按鈕
- **THEN** 系統僅啟動該項目下載流程，並將該項目狀態更新為下載中

#### Scenario: 下載中在清單後方顯示進度列

- **WHEN** 清單中的檔案項目處於下載中
- **THEN** 該項目後方顯示可更新的進度列，且進度值隨實際接收量更新

#### Scenario: 下載完成後標示已下載

- **WHEN** 清單中的檔案項目下載完成且完整性驗證成功
- **THEN** 該項目狀態先顯示為「已下載」，並於加入 seeding 後更新為「已下載並分享中」

### Requirement: 多下載者間可交換片段

系統 MUST 允許多個下載者在同一 swarm 中交換已取得片段，以減少對單一分享者的重複請求。下載者在檔案完整後 MUST 持續提供片段直到 session 結束、使用者手動停止或頁面卸載。

#### Scenario: 第二位下載者加入 swarm

- **WHEN** 第二位使用者加入相同分享 session 並開始下載
- **THEN** 兩位下載者可互相提供可用片段，且分享者上傳負載相對單純 HTTP 模式下降

#### Scenario: 下載完成後持續協同分享

- **WHEN** 第一位下載者完成檔案且頁面仍維持開啟
- **THEN** 第一位下載者維持 seeding，後續加入的下載者可自其取得可用片段
