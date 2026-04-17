# mesh-p2p

mesh-p2p 是一個使用 Vue 3、Vite、TypeScript 與 Tauri 2 建構的桌面應用程式，專為區域網路內的大檔案傳輸設計。

### 核心特色

- **P2P 協同分享**：當多位使用者同時下載同一檔案時，已完成下載的使用者會自動成為 seeder，與分享者共同提供 piece 給其他下載者。下載人數越多，整體傳輸效率越高，分享者不再是唯一瓶頸。
- **混合傳輸模式**：結合 WebTorrent P2P swarm 與 HTTP web seed fallback。P2P 可用時走 peer 交換；P2P 不可達時自動回退到 HTTP 直傳，確保任何網路環境下都能完成傳輸。
- **大檔案友善**：依檔案大小動態調整 piece size（256KB–2MB），5GB 檔案僅產生約 2,560 pieces，降低 metadata 開銷與記憶體壓力。下載端透過 File System Access API 直接寫入磁碟，不經記憶體緩衝，避免大檔案 OOM。
- **零操作完成**：下載完成後檔案直接存在於使用者選擇的資料夾中，無須手動儲存。下載期間使用暫存檔，中途停止或瀏覽器關閉不會留下損壞檔案。
- **免安裝下載端**：分享者啟動桌面應用後，下載者只需用 Chrome/Edge 開啟分享連結即可下載，無須安裝任何軟體。

## 開發

安裝前端依賴：

```bash
pnpm install
```

啟動開發模式：

```bash
pnpm tauri dev
```

## 本機建置

建立 release 執行檔：

```bash
pnpm tauri build
```

目前 Tauri 設定已將 bundle 關閉，建置完成後只會產生可攜式執行檔：

- Linux: src-tauri/target/release/mesh-p2p
- Windows: src-tauri/target/release/mesh-p2p.exe

## WebTorrent 分享行為

- 分享端會為每個檔案建立可重用的種子描述檔，儲存在原檔案同目錄，檔名格式為 `<原檔名>.mesh.seed.json`。
- 再次分享同一路徑檔案時，若檔案大小與修改時間符合既有描述檔，系統會重用既有 piece hashes，避免重新計算。
- 下載頁會透過 WebTorrent 啟動 torrent lifecycle，並使用分享端提供的 HTTP web seed 作為 fallback。
- 檔案下載完成後，瀏覽器頁面會在存活期間持續 seeding；關閉頁面或按下停止後會回收該 seeding session。

## GitHub Actions 發版

專案提供手動觸發的 workflow：.github/workflows/release-draft.yml

此 workflow 會：

1. 驗證輸入的 tag 格式。
2. 在 Linux 與 Windows runner 上建置 release 執行檔。
3. Linux 產出 tar.gz，Windows 產出 zip。
4. 建立或更新 GitHub draft release，並附上壓縮檔。

### 觸發方式

在 GitHub Actions 手動執行 Build Draft Release，填入：

- tag: 必填，格式必須是 v<major>.<minor>.<patch>，例如 v0.1.0
- release_name: 選填，若留空則直接使用 tag

### 產物命名

- Linux: mesh-p2p-<tag>-linux-x86_64.tar.gz
- Windows: mesh-p2p-<tag>-windows-x86_64.zip

## 建議開發環境

- VS Code
- Vue - Official
- Tauri
- rust-analyzer

## Windows 分享端連線注意事項

- 若 Windows 主機有多網卡（例如 VPN 或虛擬網卡），系統自動偵測的分享 IP 可能不是區網可達位址，造成客戶端連不到分享頁。
- 可在啟動前設定環境變數 `MESH_P2P_HOST`，強制指定分享主機位址（建議填入實際區網 IPv4）。

PowerShell 範例：

```powershell
$env:MESH_P2P_HOST = "192.168.0.2"
pnpm tauri dev
```

- 設定後，分享 URL 會使用你指定的 host，例如 `https://192.168.0.2:<port>`。
