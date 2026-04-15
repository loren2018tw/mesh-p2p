## MODIFIED Requirements

### Requirement: 系統自動產生 P2P 種子中繼資料

系統 MUST 對已選取檔案自動計算 piece 與 hash，產生可供 BitTorrent 相容客戶端使用的種子中繼資料。系統 MUST 將種子描述資料儲存於原始檔案同目錄，並在下次分享前先驗證檔案與既有種子描述的一致性；若一致 MUST 重用既有種子描述，若不一致 MUST 重新建立。

#### Scenario: 種子建立成功

- **WHEN** 新分享 session 建立完成
- **THEN** 系統產生對應 torrent metadata 或 magnet，並可被下載頁初始化流程取得

#### Scenario: 重複分享時重用種子描述

- **WHEN** 使用者再次分享同一檔案且檔案內容與既有種子描述一致
- **THEN** 系統直接重用既有種子描述並略過完整重算流程

#### Scenario: 種子描述不一致時重建

- **WHEN** 使用者再次分享同一路徑檔案但一致性驗證失敗
- **THEN** 系統重新計算 piece/hash 並更新該目錄下的種子描述資料
