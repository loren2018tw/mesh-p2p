## Context

目前系統在規格上定義了 HTTP + P2P 的混合下載與 swarm 片段交換，但實作仍以 HTTP 檔案串流為主，缺少可持續運作的 WebTorrent client lifecycle。另一方面，分享端每次啟動分享都重新計算 piece/hash，下載端也無法重用既有檔案加入協同上傳，造成重複計算與重複下載。

此變更橫跨 Rust 分享服務、嵌入下載頁前端邏輯與 metadata schema，且牽涉狀態相容性與生命週期管理，屬於跨模組設計。

## Goals / Non-Goals

**Goals:**

- 建立可運作的 WebTorrent 下載/上傳路徑，讓下載端在下載中與下載完成後都能提供可用片段給其他 peers。
- 分享端在首次建立 torrent metadata 後，將種子描述（torrent metadata 或等價資料）落地於原檔案目錄，後續分享優先重用。
- 擴充 metadata/state API，讓 UI 與下載邏輯可以明確判定「需下載」「已快取可直接分享」「下載完成且持續分享」。

**Non-Goals:**

- 不在本次引入跨 session、跨裝置的全域內容索引服務。
- 不處理加密私有 tracker 管理介面與帳號權限系統。
- 不將下載頁改造成完整檔案管理器（例如批次移動、重命名、版本回溯）。
- 不在本次於一般瀏覽器模式實作本地下載資料夾自動掃描；此能力延後至後續 change。

## Decisions

1. 下載端改為真正使用 WebTorrent client，HTTP 僅作 fallback data source

- 決策：下載頁以 metadata API 提供的 magnet/infoHash/piece 資訊建立 torrent；可用 peers 時優先走 P2P，無 peers 或 tracker 故障時回退 HTTP。
- 原因：符合既有規格方向，並可在不破壞可下載性的前提下提升 swarm 效益。
- 替代方案：維持純 HTTP 並以分段請求模擬 peer。此方案無法形成去中心化片段交換，故不採用。

2. 分享端採用「檔案指紋 + 種子檔」重用策略

- 決策：分享端於檔案同目錄存放種子描述檔（命名規則可為 `<filename>.mesh.torrent` 或等價副檔名），並保存必要驗證欄位（檔案大小、mtime、快速 hash 或完整 hash 摘要）。
- 原因：可避免每次分享都重做昂貴 piece/hash 建置，降低啟動延遲與 CPU 負擔。
- 替代方案：僅依檔名判定可重用。此方案碰撞風險高，容易造成錯誤 torrent 對映，故不採用。

3. 下載完成後預設持續 seeding，並提供明確回收條件

- 決策：項目狀態改為「已下載且分享中」，在頁面存活期間持續 seeding；以下情境回收：使用者手動停止、頁面卸載、session 失效、完整性重驗失敗。
- 原因：讓 swarm 在首位下載完成後仍可擴散上傳來源，降低分享端瓶頸。
- 替代方案：下載完成立即停止。此方案會使 swarm 在尖峰時段退化為單點來源，故不採用。

4. metadata 版本策略採向後相容優先、破壞變更升版

- 決策：新增欄位盡量可選，若初始化流程必需欄位改變則提升 metadataVersion 並提供明確錯誤碼與升級提示。
- 原因：降低舊下載頁與新分享端互通失敗的風險。
- 替代方案：不做版本管理直接覆寫欄位。此方案會造成靜默錯誤，故不採用。

## Risks / Trade-offs

- [Risk] 瀏覽器環境中 WebRTC/Tracker 可達性受網路政策影響，可能出現 peer 探索不穩定。
  → Mitigation: 保留 HTTP fallback、暴露 tracker 連線狀態與錯誤摘要、支援多 tracker 清單。

- [Risk] 本地快取比對若採樣不足可能誤判可重用。
  → Mitigation: 使用完整 piece/hash 驗證才標示為可 seeding，採樣只作初步候選。

- [Risk] 種子檔與原始檔不同步（檔案被外部程式覆寫）會導致錯誤分享。
  → Mitigation: 每次啟動分享先做一致性驗證，失敗即重建種子並覆蓋舊描述。

- [Trade-off] 新增 persistent metadata 與本地掃描會增加實作複雜度與初始化流程時間。
  → Mitigation: 將掃描與驗證分階段進行，先可下載再逐步提升可分享狀態，並在 UI 呈現處理中狀態。
