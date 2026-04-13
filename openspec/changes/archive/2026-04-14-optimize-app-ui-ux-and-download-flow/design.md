## Context

目前專案前端以 Vue + Tauri 為基礎，但 UI 元件體系尚未統一，導致 app 與使用者端頁面的視覺與互動不一致。下載流程雖已具備混合 HTTP/P2P 能力，但使用者端缺乏可解讀的進度與狀態，且 metadata 若以傳統下載動作觸發，容易受到瀏覽器安全策略或下載保護機制影響。另有 app 右側資訊區目前語意不足，使用者難以判斷其對應的系統狀態與下一步操作。

此變更跨越 Vue UI、下載流程、metadata API 與 Tauri 資訊面板，屬於跨模組調整，需要先明確技術決策與資料契約。

## Goals / Non-Goals

**Goals:**

- 導入 Vuetify 作為 app 與使用者端共享 UI foundation，統一元件語彙與互動樣式。
- 將下載流程的可觀測資訊（進度、速度、狀態、錯誤）標準化並顯示於使用者端。
- 將 metadata 取得改為程式化 API 流程（fetch/XHR），避免依賴一般瀏覽器下載行為。
- 重新定義 app 右側資訊區的資訊模型，呈現可理解且可行動的狀態摘要。

**Non-Goals:**

- 不在此變更中改寫底層 BitTorrent 演算法或 piece 排程策略。
- 不在此變更中引入帳號系統、雲端同步或跨裝置身份管理。
- 不在此變更中重做整體導覽 IA（Information Architecture）或品牌視覺重塑。

## Decisions

1. 採用 Vuetify 作為唯一主要 GUI 元件層

- Decision: app 與使用者端前端全面以 Vue + Vuetify 實作核心互動元件（按鈕、表格、進度、狀態標籤、對話框）。
- Rationale: 降低自製元件維護成本，確保可及性與一致性，並加速 UI 重構。
- Alternatives considered:
  - 延用現有散裝 CSS + 原生元件：短期成本低，但一致性與可維護性差。
  - 改用其他 UI framework（如 Element Plus）：可行，但與目前生態及 Material 風格適配度較弱。

2. metadata 改用 API 請求而非瀏覽器檔案下載觸發

- Decision: 使用者端僅透過 JSON API 取得初始化 metadata（session id、files、info hash/magnet、piece 參數與版本資訊），再由前端下載器初始化。
- Rationale: 可避免被瀏覽器下載保護流程誤判，並改善錯誤處理與重試控制。
- Alternatives considered:
  - 透過 `<a download>` 或直接導向檔案 URL：易受瀏覽器策略影響且難以精準控制重試。
  - 以 blob 轉存再觸發下載：仍屬下載流程變形，非必要且增加複雜度。

3. 建立下載狀態資料模型與 UI 對映

- Decision: 下載器維護標準化狀態欄位（phase、progressPercent、bytesReceived、totalBytes、speedBps、etaSeconds、sourceMix、errorCode）。
- Rationale: 讓 UI 可穩定顯示進度與診斷資訊，也利於後續遙測與測試。
- Alternatives considered:
  - 僅顯示百分比：資訊不足，無法協助排錯與理解來源切換。
  - 顯示底層 debug 原始欄位：過於技術導向，不利一般使用者。

4. app 右側資訊區改為「狀態摘要面板」

- Decision: 以任務導向分區呈現：分享狀態、連線可達性、目前活躍下載、最近錯誤/警示、建議下一步。
- Rationale: 提升可理解性，讓資訊可直接支援決策與操作。
- Alternatives considered:
  - 保留現況文字堆疊：實作成本低，但可讀性與行動性不足。
  - 全改為圖表儀表板：視覺華麗但初期資料品質與維護成本較高。

## Risks / Trade-offs

- [Risk] Vuetify 導入造成既有樣式衝突與 bundle 增長 → Mitigation: 分階段替換、啟用 tree-shaking、優先改造關鍵頁面。
- [Risk] metadata 契約變更導致舊版頁面不相容 → Mitigation: metadata API 增加版本欄位與向後相容處理。
- [Risk] 進度計算在多來源切換下可能抖動 → Mitigation: 使用平滑速度估算與最小刷新間隔，避免 UI 閃爍。
- [Risk] 右側面板資訊過多反而造成負擔 → Mitigation: 以摘要優先，細節採可展開式資訊。

## Migration Plan

1. 新增 Vuetify 基礎設定與全域 theme/token，先在 app 主框架接入。
2. 將下載頁 metadata 入口改為 API-based 初始化，同步更新回應欄位文件。
3. 實作下載進度資料模型與前端顯示元件，完成基本狀態與錯誤提示。
4. 重構 app 右側資訊區為狀態摘要面板，對接實際 session/傳輸資料。
5. 以 feature flag 或漸進切換方式替換舊 UI，確認相容後移除舊邏輯。

## Open Questions

- 是否需要在右側面板加入「peer 數量與來源比例」作為預設可見欄位，或放入進階資訊區？
- metadata API 是否需提供簽章或完整性欄位，以強化中繼資料可信度？
- Vuetify 主題是否需支援高對比模式作為首發需求？
