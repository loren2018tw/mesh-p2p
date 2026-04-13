## Purpose

定義瀏覽器端下載器在混合 HTTP 與 P2P swarm 模式下的行為，確保下載流程在多人協作與異常情境中仍可持續且正確。

## Requirements

### Requirement: 瀏覽器端可啟動混合下載

系統 MUST 讓瀏覽器端下載器可同時使用分享者 HTTP 來源與 P2P peers，並在初始化後自動開始下載。

#### Scenario: 首次連線啟動下載

- **WHEN** 使用者在瀏覽器開啟分享頁並點擊下載
- **THEN** 下載器根據 metadata 啟動 HTTP 與 P2P 連線並開始接收片段

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
