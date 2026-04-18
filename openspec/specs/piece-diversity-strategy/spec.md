## Purpose

定義 piece 下載順序多樣化策略，確保不同下載端優先取得不同片段，最大化 P2P 交換效率。

## Requirements

### Requirement: Server 為每個 client 分配 piece priority offset

系統 MUST 在 metadata API 回應中為每個請求回傳 `piecePriorityOffset` 數值，該值基於目前已分配的 client 數量輪轉遞增，確保不同 client 取得不同的 offset。

#### Scenario: 連續兩個 client 取得不同 offset

- **WHEN** 第一個 client 請求 metadata API，隨後第二個 client 也請求 metadata API
- **THEN** 兩個 client 收到的 `piecePriorityOffset` 值不同，且差距至少為 `totalPieces / 預期最大 client 數`

#### Scenario: offset 在 piece 總數範圍內循環

- **WHEN** 已有超過預期 client 數量的 client 請求 metadata
- **THEN** offset 值以 modulo 方式循環回到起點，不會超出 piece 總數範圍

### Requirement: 下載端根據 offset 調整 piece 下載優先順序

系統 MUST 讓下載端在 torrent 初始化完成後，根據 server 回傳的 `piecePriorityOffset` 將 pieces 分群並優先下載對應群組的 pieces。

#### Scenario: 下載端套用 piece priority

- **WHEN** 下載端收到 `piecePriorityOffset` 為 N，且 torrent 含 1000 個 pieces
- **THEN** 下載端優先下載從 piece index N 開始的一段 pieces，其餘 pieces 設為較低優先級但仍會下載

#### Scenario: 不同 offset 的 client 產生互補 pieces

- **WHEN** 兩個 client 分別取得 offset 0 和 offset 500（假設 1000 pieces）
- **THEN** 在下載初期，Client A 優先持有 piece 0-499 區間的片段，Client B 優先持有 piece 500-999 區間的片段，兩者可透過 P2P 互相交換對方缺少的片段

### Requirement: piece priority 不得阻止最終完整下載

系統 MUST 確保所有 pieces 最終都會被下載，priority offset 只影響下載順序，不得導致任何 piece 永遠不被下載。

#### Scenario: 優先片段下載完成後繼續其餘片段

- **WHEN** 下載端已完成高優先級群組的所有 pieces
- **THEN** 系統自動繼續下載其餘低優先級 pieces 直到全部完成
