## Context

此變更需要同時調整 Tauri Rust backend 與 Vue frontend，並引入瀏覽器可用的 P2P 傳輸機制。現況專案尚未提供可對外分享的下載頁面，也沒有分享 session、種子中繼資料管理與 peer 交換流程。

主要限制如下：

- 分享端需在桌面程式內完成檔案選取與分享啟動，不依賴外部 CLI。
- 接收端僅使用瀏覽器即可下載，避免要求安裝額外客戶端。
- 多人同時下載時，系統需支援 swarm 傳輸，避免分享者成為單點頻寬瓶頸。
- 需符合 Tauri 能力權限與本機檔案安全存取邊界。

## Goals / Non-Goals

**Goals:**

- 建立內建 Web Server，提供單一分享頁 URL 與檔案描述 API。
- 建立檔案到種子中繼資料（torrent metadata / magnet）生成與發布流程。
- 建立瀏覽器端 HTTP + P2P 混合下載能力，優先確保檔案完整性與下載成功率。
- 提供最小可用觀測資訊（peer 數量、下載進度、分享 session 狀態）。

**Non-Goals:**

- 不支援帳號系統、登入或跨網際網路的 NAT 打洞保證。
- 不建立長期歷史資料分析平台（僅保留執行期狀態）。
- 不在本次變更處理 DRM、端對端內容加密或權限細分分享名單。

## Decisions

1. 以「控制面」與「資料面」分離的架構實作。

- 控制面：Tauri backend 管理分享 session、檔案資訊、tracker 設定與 Web Server 路由。
- 資料面：瀏覽器使用 WebRTC DataChannel + BitTorrent 協議片段交換，並保留 HTTP 回源。
- 理由：可在不中斷下載體驗下，以 P2P 提升吞吐，且控制邏輯維持在桌面端。
- 替代方案：純 HTTP（實作簡單但無 swarm 加速）、純 P2P（首次可用性差，啟動成本高）。

2. 使用混合下載策略（HTTP seed + P2P peers）。

- 瀏覽器端先透過分享頁取得 torrent metadata 或 magnet URI。
- 若 P2P peer 數量不足，回退或並行使用分享者 HTTP 來源。
- 理由：提高冷啟動成功率並降低 tracker 短暫不穩造成的失敗率。
- 替代方案：強制僅 P2P，會在初始 peer 稀少時導致體驗不穩。

3. 後端維持單機短生命週期分享 session。

- 每次分享建立 session id，含檔案路徑、檔案長度、info hash、啟動時間與狀態。
- 關閉分享後撤銷對應 HTTP 與 metadata 暴露。
- 理由：簡化生命週期管理，降低敏感檔案長期暴露風險。

4. 前端採事件驅動狀態模型。

- 透過 polling 或 WebSocket/SSE（視實作可行性）回報 peer、速度、完成率。
- 將下載狀態拆分為：初始化、尋找 peers、下載中、驗證中、完成、失敗。
- 理由：便於 UX 呈現與後續測試，狀態可對應驗收情境。

## Risks / Trade-offs

- [WebRTC/BitTorrent 相容性差異] → Mitigation: 選用成熟瀏覽器端 library，先鎖定 Chrome/Chromium 與 Firefox 驗收矩陣。
- [內建 Web Server 增加攻擊面] → Mitigation: 僅綁定可設定網卡、限制路由、關閉目錄索引、加上基本 rate limit。
- [大檔案導致記憶體壓力] → Mitigation: 採串流與分塊 I/O，避免整檔載入記憶體。
- [Tracker 依賴不穩定] → Mitigation: 支援多 announce URL 與 HTTP 回源策略。
- [使用者對分享 URL 安全認知不足] → Mitigation: UI 明確顯示公開範圍與一鍵停止分享。

## Migration Plan

1. 在 backend 新增分享 session 與 Web Server 模組，先以本機測試檔案直連下載。
2. 加入種子中繼資料產生流程，提供 metadata API 與分享頁整合。
3. 前端導入 P2P library，完成瀏覽器端下載與進度顯示。
4. 增加多人下載情境測試（1 分享者 + N 下載者），驗證 swarm 加速與完整性。
5. 發布時保留功能旗標，可在異常時回退為純 HTTP 模式。

## Open Questions

- tracker 由應用內建暫時節點、固定公共節點，或兩者混合？
- 是否需要在第一版加入分享連結有效期限與一次性 token？
- 下載完成檔案在瀏覽器端的落地策略（直接儲存 vs File System Access API）目標支援範圍為何？
