# mesh-p2p

mesh-p2p 是一個使用 Vue 3、Vite、TypeScript 與 Tauri 2 建構的桌面應用程式。

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
- 目前未實作一般瀏覽器自動掃描本地下載資料夾；本地快取重用將在後續 change 處理。

### WebTorrent Replay Script

啟動分享後，可用下列命令重播 WebTorrent 下載/做種流程：

```bash
pnpm replay:webtorrent -- <share-url> [file-id]
```

範例：

```bash
pnpm replay:webtorrent -- http://192.168.1.10:38451
```

此腳本會：

1. 讀取 metadata API。
2. 下載對應 torrent descriptor。
3. 啟動兩個 WebTorrent client。
4. 讓第一個 client 先完成下載並保持 seeding。
5. 驗證第二個 client 是否能以相同 torrent lifecycle 完成下載，並輸出是否偵測到 peer 連線。

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
