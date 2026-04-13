<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

type ShareSession = {
  sessionId: string;
  files: SharedFile[];
  fileCount: number;
  totalSize: number;
  revision: number;
  lastUpdatedUnixMs: number;
  trackerUrls: string[];
  startedAtUnixMs: number;
};

type SharedFile = {
  fileId: string;
  fileName: string;
  filePath: string;
  fileSize: number;
  infoHash: string;
  pieceSize: number;
  pieceCount: number;
  magnetUri: string;
};

type ShareStatus = {
  isSharing: boolean;
  serverUrl: string;
  fallbackHttpEnabled: boolean;
  session: ShareSession | null;
  metrics: ShareMetrics;
  processingProgress: ProcessingProgress | null;
};

type ProcessingProgress = {
  isProcessing: boolean;
  currentFileName: string;
  currentFileIndex: number;
  totalFiles: number;
  bytesProcessed: number;
  totalBytes: number;
  percentage: number;
};

type ShareMetrics = {
  activeClientCount: number;
  httpUploadedBytes: number;
  metadataRevision: number;
  lastActivityUnixMs: number;
};

const selectedFilePaths = ref<string[]>([]);
const manualFilePath = ref("");
const shareStatus = ref<ShareStatus | null>(null);
const message = ref("尚未開始分享");
const isPickingFiles = ref(false);

const isSharing = computed(() => !!shareStatus.value?.isSharing);

const activeFiles = computed(() => shareStatus.value?.session?.files ?? []);

const metrics = computed(
  () =>
    shareStatus.value?.metrics ?? {
      activeClientCount: 0,
      httpUploadedBytes: 0,
      metadataRevision: 0,
      lastActivityUnixMs: 0,
    },
);

const processingProgress = computed(
  () => shareStatus.value?.processingProgress ?? null,
);

const isProcessing = computed(() => !!processingProgress.value?.isProcessing);

async function refreshStatus() {
  try {
    shareStatus.value = await invoke<ShareStatus>("get_share_status");
  } catch (error) {
    message.value = `取得狀態失敗：${String(error)}`;
  }
}

async function pickFile() {
  if (isPickingFiles.value) {
    return;
  }

  isPickingFiles.value = true;

  try {
    const chosen = await invoke<string[]>("pick_share_files");
    if (chosen.length) {
      await mergeFilesIntoShare(chosen);
    }
  } catch (error) {
    message.value = `檔案選取失敗：${String(error)}`;
  } finally {
    isPickingFiles.value = false;
  }
}

async function addManualPath() {
  const trimmed = manualFilePath.value.trim();
  if (!trimmed) {
    return;
  }

  await mergeFilesIntoShare([trimmed]);
  manualFilePath.value = "";
}

async function mergeFilesIntoShare(paths: string[]) {
  const uniquePaths = Array.from(new Set(paths));

  if (isSharing.value) {
    try {
      const session = await invoke<ShareSession>("add_share_files", {
        filePaths: uniquePaths,
      });
      if (shareStatus.value) {
        shareStatus.value = {
          ...shareStatus.value,
          session,
        };
      }
      await refreshStatus();
      message.value = `已加入 ${uniquePaths.length} 個新檔案到分享中`;
    } catch (error) {
      message.value = `加入分享檔案失敗：${String(error)}`;
    }
    return;
  }

  selectedFilePaths.value = Array.from(
    new Set([...selectedFilePaths.value, ...uniquePaths]),
  );
  message.value = `已加入 ${uniquePaths.length} 個檔案`;
}

function removeFile(path: string) {
  selectedFilePaths.value = selectedFilePaths.value.filter(
    (item) => item !== path,
  );
}

async function startShare() {
  if (!selectedFilePaths.value.length) {
    message.value = "請先加入至少一個要分享的檔案";
    return;
  }

  try {
    const result = await invoke<{ serverUrl: string; session: ShareSession }>(
      "start_share",
      {
        filePaths: selectedFilePaths.value,
      },
    );
    message.value = `分享已啟動：${result.serverUrl}`;
    selectedFilePaths.value = [];
    manualFilePath.value = "";
    await refreshStatus();
  } catch (error) {
    message.value = `啟動分享失敗：${String(error)}`;
  }
}

async function stopShare() {
  try {
    await invoke("stop_share");
    message.value = "分享已停止";
    await refreshStatus();
  } catch (error) {
    message.value = `停止分享失敗：${String(error)}`;
  }
}

onMounted(() => {
  refreshStatus();
  window.setInterval(refreshStatus, 5000);
});

function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB"];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return Math.round((bytes / Math.pow(k, i)) * 100) / 100 + " " + sizes[i];
}
</script>

<template>
  <main class="page">
    <section class="panel">
      <h1>Mesh P2P File Share</h1>
      <p class="sub">內建 Web Server + P2P-ready 下載入口</p>

      <div class="field-row">
        <button :disabled="isPickingFiles" @click="pickFile">
          {{ isPickingFiles ? "選擇中..." : "選擇檔案" }}
        </button>
        <input
          v-model="manualFilePath"
          placeholder="手動輸入檔案路徑後按 Enter"
          @keydown.enter.prevent="addManualPath"
        />
      </div>

      <div v-if="selectedFilePaths.length && !isSharing" class="meta">
        <p><strong>待分享檔案：</strong>{{ selectedFilePaths.length }} 個</p>
        <ul class="file-list">
          <li v-for="filePath in selectedFilePaths" :key="filePath">
            <span>{{ filePath }}</span>
            <button class="ghost small" @click="removeFile(filePath)">
              移除
            </button>
          </li>
        </ul>
      </div>

      <div class="field-row">
        <button @click="startShare">啟動分享</button>
        <button class="ghost" :disabled="!isSharing" @click="stopShare">
          停止分享
        </button>
      </div>

      <div class="status">
        <p><strong>狀態：</strong>{{ message }}</p>
        <p v-if="shareStatus?.serverUrl">
          <strong>分享 URL（主機 IP）：</strong>{{ shareStatus.serverUrl }}
        </p>
        <p v-if="shareStatus?.fallbackHttpEnabled" class="warn">
          目前啟用 HTTP fallback 模式
        </p>
      </div>

      <div v-if="isProcessing && processingProgress" class="processing">
        <p><strong>正在處理檔案...</strong></p>
        <p class="progress-info">
          檔案 {{ processingProgress.currentFileIndex }} /
          {{ processingProgress.totalFiles }} ({{
            formatBytes(processingProgress.bytesProcessed)
          }}
          / {{ formatBytes(processingProgress.totalBytes) }})
        </p>
        <div class="progress-bar-container">
          <div
            class="progress-bar"
            :style="{ width: processingProgress.percentage + '%' }"
          >
            <span class="progress-text"
              >{{ processingProgress.percentage }}%</span
            >
          </div>
        </div>
      </div>

      <div v-if="shareStatus?.session" class="meta">
        <p><strong>檔案數：</strong>{{ shareStatus.session.fileCount }}</p>
        <p>
          <strong>總大小：</strong>{{ shareStatus.session.totalSize }} bytes
        </p>
        <p><strong>清單版本：</strong>{{ shareStatus.session.revision }}</p>
        <ul class="file-list compact">
          <li v-for="file in activeFiles" :key="file.fileId">
            <span>{{ file.fileName }}</span>
            <span>{{ file.fileSize }} bytes</span>
          </li>
        </ul>
      </div>
    </section>

    <section class="panel">
      <h2>分享統計</h2>
      <p class="sub">下載端數量、HTTP 上傳量與清單同步狀態</p>

      <div class="meta">
        <p><strong>目前下載端數量：</strong>{{ metrics.activeClientCount }}</p>
        <p>
          <strong>累計 HTTP 上傳量：</strong
          >{{ metrics.httpUploadedBytes }} bytes
        </p>
        <p><strong>目前清單版本：</strong>{{ metrics.metadataRevision }}</p>
        <p><strong>最近活動：</strong>{{ metrics.lastActivityUnixMs || 0 }}</p>
      </div>

      <p class="warn">
        使用者端下載頁會每 5
        秒主動更新檔案清單；分享中新增檔案後，下載頁可自動同步新清單。
      </p>
    </section>
  </main>
</template>

<style scoped>
.page {
  min-height: 100vh;
  margin: 0;
  padding: 24px;
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
  gap: 16px;
  background: linear-gradient(135deg, #f0f7ff 0%, #eafbea 45%, #fff5dd 100%);
  color: #17324d;
}

.panel {
  background: #ffffffd9;
  border: 1px solid #b8cfe0;
  border-radius: 14px;
  padding: 18px;
  box-shadow: 0 8px 22px rgba(25, 44, 68, 0.1);
}

h1,
h2 {
  margin: 0 0 8px;
  font-family: "IBM Plex Sans", "Noto Sans TC", sans-serif;
}

.sub {
  margin-top: 0;
  color: #446581;
}

.field-row {
  display: flex;
  gap: 8px;
  margin: 10px 0;
}

input,
button {
  border-radius: 10px;
  border: 1px solid #8cb2cb;
  padding: 10px 12px;
  font-size: 14px;
}

input {
  flex: 1;
}

button {
  cursor: pointer;
  background: #1e6fb9;
  color: #fff;
  border-color: #1e6fb9;
}

button.ghost {
  background: #fff;
  color: #1e6fb9;
}

.status,
.meta {
  background: #f7fbff;
  border: 1px dashed #acc8dd;
  border-radius: 10px;
  padding: 10px;
}

.file-list {
  list-style: none;
  padding: 0;
  margin: 8px 0 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.file-list li {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 10px;
  background: #fff;
  border: 1px solid #d7e6f0;
  border-radius: 8px;
}

.file-list.compact li {
  font-size: 13px;
}

.small {
  padding: 6px 10px;
  font-size: 12px;
}

.warn {
  color: #915900;
}

@media (max-width: 720px) {
  .field-row {
    flex-direction: column;
  }
}

.processing {
  background: #f0f8ff;
  border: 1px solid #7eb3d4;
  border-radius: 10px;
  padding: 16px;
  margin: 12px 0 0 0;
  animation: pulse 1.5s ease-in-out infinite;
}

.processing p {
  margin: 0 0 8px 0;
  font-weight: 600;
  color: #0d47a1;
}

.progress-info {
  font-size: 12px;
  color: #446581;
  font-weight: normal;
  margin-bottom: 10px;
}

.progress-bar-container {
  width: 100%;
  height: 24px;
  background: #e0eef8;
  border-radius: 12px;
  overflow: hidden;
  border: 1px solid #7eb3d4;
  position: relative;
}

.progress-bar {
  height: 100%;
  background: linear-gradient(90deg, #1e6fb9 0%, #2e8fc9 100%);
  width: 0%;
  transition: width 0.3s ease;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 11px;
}

.progress-text {
  color: #fff;
  font-size: 12px;
  font-weight: 600;
  text-shadow: 0 1px 2px rgba(0, 0, 0, 0.2);
}

@keyframes pulse {
  0%,
  100% {
    box-shadow: 0 0 0 0 rgba(30, 111, 185, 0.3);
  }
  50% {
    box-shadow: 0 0 0 8px rgba(30, 111, 185, 0);
  }
}
</style>
