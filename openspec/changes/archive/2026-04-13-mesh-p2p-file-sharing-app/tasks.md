## 1. Backend 分享 session 與 Web Server 基礎

- [x] 1.1 在 `src-tauri/src/` 新增 share session domain model（session id、檔案資訊、狀態、info hash）
- [x] 1.2 實作分享啟動/停止 command，含檔案存在與可讀性驗證
- [x] 1.3 建立內建 Web Server 路由，提供分享頁與 metadata API
- [x] 1.4 停止分享時撤銷 session 與對應路由可用性

## 2. 種子中繼資料與 tracker 管理

- [x] 2.1 建立檔案 piece/hash 計算流程，輸出 torrent metadata 或 magnet
- [x] 2.2 加入 announce/tracker 清單設定讀取與驗證
- [x] 2.3 在 metadata API 回傳 session 初始化所需欄位（檔案長度、piece size、info hash）
- [x] 2.4 為種子建立流程補上單元測試（有效檔案、無效檔案、多 tracker）

## 3. 前端分享頁與下載器初始化

- [x] 3.1 新增下載頁 UI，顯示檔案資訊、session 狀態與下載按鈕
- [x] 3.2 串接 metadata API 並建立前端下載狀態機（初始化、尋找 peers、下載中、驗證中、完成、失敗）
- [x] 3.3 導入瀏覽器端 P2P library，完成 torrent/magnet 初始化
- [x] 3.4 建立 HTTP 回源下載通道，確保無 peers 時仍可下載

## 4. Swarm 交換與完整性保證

- [x] 4.1 實作 peer 事件處理（加入、離線、片段可用度變更）與 UI 更新
- [x] 4.2 實作片段 hash 驗證與損毀片段重抓邏輯

## 5. 安全性、可觀測性與發佈準備

- [x] 5.1 限制 Web Server 綁定介面與路由曝露範圍，加入基本 rate limit
- [x] 5.2 在前端顯示安全提示與一鍵停止分享操作
- [ ] 5.3 補齊端到端測試與手動測試腳本（Chrome/Chromium、Firefox）
- [x] 5.4 加入功能旗標以支援故障時回退純 HTTP 模式
