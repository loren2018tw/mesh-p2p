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
