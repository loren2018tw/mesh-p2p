<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import QRCode from "qrcode";
import UiMessageAlert from "./components/UiMessageAlert.vue";
import UiProgressBar from "./components/UiProgressBar.vue";
import UiStatusChip from "./components/UiStatusChip.vue";

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

type ShareMetrics = {
  activeClientCount: number;
  httpUploadedBytes: number;
  metadataRevision: number;
  lastActivityUnixMs: number;
};

type ShareInsights = {
  shareState: string;
  reachability: string;
  activeDownloads: number;
  recentError: string | null;
  recentActivityLabel: string;
  nextActionHint: string;
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

type ShareStatus = {
  isSharing: boolean;
  serverUrl: string;
  fallbackHttpEnabled: boolean;
  session: ShareSession | null;
  metrics: ShareMetrics;
  insights: ShareInsights;
  processingProgress: ProcessingProgress | null;
};

const selectedFilePaths = ref<string[]>([]);
const manualFilePath = ref("");
const shareStatus = ref<ShareStatus | null>(null);
const message = ref("尚未開始分享");
const messageKind = ref<"success" | "warning" | "error" | "info">("info");
const isPickingFiles = ref(false);
const appVersion = __APP_VERSION__;
const qrDialogOpen = ref(false);
const shareQrDataUrl = ref("");
const isGeneratingShareQr = ref(false);

let refreshTimer: number | null = null;

const isSharing = computed(() => !!shareStatus.value?.isSharing);
const activeFiles = computed(() => shareStatus.value?.session?.files ?? []);
const shareUrl = computed(() => shareStatus.value?.serverUrl ?? "");

const metrics = computed(
  () =>
    shareStatus.value?.metrics ?? {
      activeClientCount: 0,
      httpUploadedBytes: 0,
      metadataRevision: 0,
      lastActivityUnixMs: 0,
    },
);

const insights = computed(
  () =>
    shareStatus.value?.insights ?? {
      shareState: "未啟動",
      reachability: "尚未啟動",
      activeDownloads: 0,
      recentError: null,
      recentActivityLabel: "暫無活動",
      nextActionHint: "先選擇檔案並啟動分享",
    },
);

const processingProgress = computed(
  () => shareStatus.value?.processingProgress ?? null,
);

const isProcessing = computed(() => !!processingProgress.value?.isProcessing);

const currentStatusKind = computed(() => {
  if (insights.value.recentError) {
    return "error" as const;
  }
  if (isSharing.value) {
    return "success" as const;
  }
  return "neutral" as const;
});

watch(
  shareUrl,
  (url) => {
    void syncShareQr(url);
  },
  { immediate: true },
);

async function refreshStatus() {
  try {
    shareStatus.value = await invoke<ShareStatus>("get_share_status");
  } catch (error) {
    messageKind.value = "error";
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
    messageKind.value = "error";
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
      await invoke<ShareSession>("add_share_files", {
        filePaths: uniquePaths,
      });
      await refreshStatus();
      messageKind.value = "success";
      message.value = `已加入 ${uniquePaths.length} 個新檔案到分享中`;
    } catch (error) {
      messageKind.value = "error";
      message.value = `加入分享檔案失敗：${String(error)}`;
    }
    return;
  }

  selectedFilePaths.value = Array.from(
    new Set([...selectedFilePaths.value, ...uniquePaths]),
  );
  messageKind.value = "info";
  message.value = `已加入 ${uniquePaths.length} 個檔案`;
}

function removeFile(path: string) {
  selectedFilePaths.value = selectedFilePaths.value.filter(
    (item) => item !== path,
  );
}

async function startShare() {
  if (!selectedFilePaths.value.length) {
    messageKind.value = "warning";
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
    messageKind.value = "success";
    message.value = `分享已啟動：${result.serverUrl}`;
    selectedFilePaths.value = [];
    manualFilePath.value = "";
    await refreshStatus();
  } catch (error) {
    messageKind.value = "error";
    message.value = `啟動分享失敗：${String(error)}`;
  }
}

async function stopShare() {
  try {
    await invoke("stop_share");
    messageKind.value = "warning";
    message.value = "分享已停止";
    await refreshStatus();
  } catch (error) {
    messageKind.value = "error";
    message.value = `停止分享失敗：${String(error)}`;
  }
}

async function syncShareQr(url: string) {
  if (!url) {
    shareQrDataUrl.value = "";
    qrDialogOpen.value = false;
    return;
  }

  isGeneratingShareQr.value = true;

  try {
    shareQrDataUrl.value = await QRCode.toDataURL(url, {
      errorCorrectionLevel: "M",
      margin: 1,
      width: 320,
      color: {
        dark: "#0f172a",
        light: "#ffffffff",
      },
    });
  } finally {
    isGeneratingShareQr.value = false;
  }
}

async function copyText(value: string) {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(value);
    return;
  }

  const textarea = document.createElement("textarea");
  textarea.value = value;
  textarea.setAttribute("readonly", "true");
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();

  const copied = document.execCommand("copy");
  textarea.remove();

  if (!copied) {
    throw new Error("無法複製分享 URL");
  }
}

async function openShareQrDialog() {
  if (!shareUrl.value) {
    return;
  }

  try {
    if (!shareQrDataUrl.value) {
      await syncShareQr(shareUrl.value);
    }

    await copyText(shareUrl.value);
    messageKind.value = "success";
    message.value = "分享 URL 已複製，請讓對方掃描 QR Code 或直接貼上連結";
  } catch (error) {
    messageKind.value = "warning";
    message.value = `已開啟 QR Code，但複製失敗：${String(error)}`;
  }

  qrDialogOpen.value = true;
}

async function copyShareUrl() {
  if (!shareUrl.value) {
    return;
  }

  try {
    await copyText(shareUrl.value);
    messageKind.value = "success";
    message.value = "分享 URL 已複製";
  } catch (error) {
    messageKind.value = "error";
    message.value = `複製分享 URL 失敗：${String(error)}`;
  }
}

function formatBytes(bytes: number): string {
  if (!bytes) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(
    Math.floor(Math.log(bytes) / Math.log(k)),
    sizes.length - 1,
  );
  return `${(bytes / Math.pow(k, i)).toFixed(i === 0 ? 0 : 2)} ${sizes[i]}`;
}

function formatTime(unixMs: number): string {
  if (!unixMs) return "尚無";
  return new Date(unixMs).toLocaleString("zh-TW", { hour12: false });
}

onMounted(() => {
  refreshStatus();
  refreshTimer = window.setInterval(refreshStatus, 5000);
});

onUnmounted(() => {
  if (refreshTimer) {
    window.clearInterval(refreshTimer);
  }
});
</script>

<template>
  <v-app>
    <v-main class="main-bg">
      <v-container class="py-6" fluid>
        <v-row>
          <v-col cols="12" lg="8">
            <v-card rounded="xl" class="mb-4">
              <v-card-title
                class="d-flex align-center justify-space-between ga-3"
              >
                <span class="text-h5 font-weight-bold"
                  >Mesh P2P File Share</span
                >
                <v-btn
                  icon="mdi-github"
                  variant="text"
                  color="primary"
                  href="https://github.com/loren2018tw/mesh-p2p"
                  target="_blank"
                  rel="noopener noreferrer"
                  aria-label="GitHub Repository"
                />
              </v-card-title>
              <v-card-subtitle
                >版本： v{{ appVersion }} By
                Loren(loren.tw@gmail.com)</v-card-subtitle
              >
              <v-card-text>
                <UiMessageAlert :text="message" :kind="messageKind" />

                <div class="d-flex align-center ga-2 flex-wrap mb-3">
                  <UiStatusChip
                    :label="insights.shareState"
                    :kind="currentStatusKind"
                  />
                  <UiStatusChip
                    :label="`可達性：${insights.reachability}`"
                    kind="info"
                  />
                  <UiStatusChip
                    :label="`活躍下載：${insights.activeDownloads}`"
                    kind="warning"
                  />
                </div>

                <v-row>
                  <v-col cols="12" md="4">
                    <v-btn
                      color="primary"
                      block
                      prepend-icon="mdi-file-plus"
                      :loading="isPickingFiles"
                      @click="pickFile"
                    >
                      選擇檔案
                    </v-btn>
                  </v-col>
                  <v-col cols="12" md="8">
                    <v-text-field
                      v-model="manualFilePath"
                      label="手動輸入檔案路徑，按 Enter 加入"
                      variant="outlined"
                      density="comfortable"
                      hide-details
                      @keydown.enter.prevent="addManualPath"
                    />
                  </v-col>
                </v-row>

                <v-card
                  v-if="selectedFilePaths.length && !isSharing"
                  variant="tonal"
                  class="mt-4"
                >
                  <v-card-title class="text-subtitle-1"
                    >待分享檔案（{{ selectedFilePaths.length }}）</v-card-title
                  >
                  <v-list density="comfortable">
                    <v-list-item
                      v-for="filePath in selectedFilePaths"
                      :key="filePath"
                      :title="filePath"
                    >
                      <template #append>
                        <v-btn
                          size="small"
                          variant="text"
                          color="error"
                          prepend-icon="mdi-close"
                          @click="removeFile(filePath)"
                        >
                          移除
                        </v-btn>
                      </template>
                    </v-list-item>
                  </v-list>
                </v-card>

                <div class="d-flex ga-2 mt-4 flex-wrap">
                  <v-btn
                    color="secondary"
                    prepend-icon="mdi-play"
                    :disabled="isSharing"
                    @click="startShare"
                  >
                    啟動分享
                  </v-btn>
                  <v-btn
                    color="warning"
                    prepend-icon="mdi-stop"
                    :disabled="!isSharing"
                    @click="stopShare"
                  >
                    暫停分享
                  </v-btn>
                </div>

                <v-card
                  v-if="shareStatus?.serverUrl"
                  class="mt-4"
                  variant="outlined"
                >
                  <v-card-text
                    class="d-flex align-center justify-space-between ga-4 flex-wrap"
                  >
                    <div class="share-url-block">
                      <div class="text-body-2 mb-1">分享 URL（主機 IP）</div>
                      <div class="text-subtitle-2 text-primary share-url-text">
                        {{ shareStatus.serverUrl }}
                      </div>
                      <div class="d-flex ga-2 mt-3 flex-wrap">
                        <v-btn
                          size="small"
                          color="primary"
                          variant="tonal"
                          prepend-icon="mdi-content-copy"
                          @click="copyShareUrl"
                        >
                          複製連結
                        </v-btn>
                        <v-btn
                          size="small"
                          color="secondary"
                          variant="text"
                          prepend-icon="mdi-qrcode"
                          @click="openShareQrDialog"
                        >
                          顯示 QR Code
                        </v-btn>
                      </div>
                      <div
                        v-if="shareStatus?.fallbackHttpEnabled"
                        class="text-warning mt-2"
                      >
                        目前啟用 HTTP fallback 模式
                      </div>
                    </div>

                    <button
                      type="button"
                      class="qr-trigger"
                      :disabled="isGeneratingShareQr"
                      @click="openShareQrDialog"
                    >
                      <img
                        v-if="shareQrDataUrl"
                        :src="shareQrDataUrl"
                        alt="分享 QR Code"
                        class="qr-thumb"
                      />
                      <div v-else class="qr-thumb qr-thumb--placeholder">
                        <v-progress-circular
                          v-if="isGeneratingShareQr"
                          indeterminate
                          size="22"
                          width="2"
                          color="primary"
                        />
                        <v-icon v-else icon="mdi-qrcode" size="30" />
                      </div>
                      <span class="qr-trigger__label">掃描分享</span>
                    </button>
                  </v-card-text>
                </v-card>

                <v-dialog v-model="qrDialogOpen" max-width="460">
                  <v-card rounded="xl">
                    <v-card-title
                      class="d-flex align-center justify-space-between"
                    >
                      <span>分享 QR Code</span>
                      <v-btn
                        icon="mdi-close"
                        variant="text"
                        @click="qrDialogOpen = false"
                      />
                    </v-card-title>
                    <v-card-text class="text-center">
                      <div class="text-body-2 text-medium-emphasis mb-4">
                        已自動複製分享 URL，對方可掃描 QR Code 或直接貼上連結。
                      </div>
                      <img
                        v-if="shareQrDataUrl"
                        :src="shareQrDataUrl"
                        alt="大型分享 QR Code"
                        class="qr-dialog-image"
                      />
                      <div v-else class="py-8">
                        <v-progress-circular indeterminate color="primary" />
                      </div>
                      <v-sheet
                        border
                        rounded="lg"
                        color="surface-variant"
                        class="mt-4 pa-3 text-left"
                      >
                        <div class="text-caption text-medium-emphasis mb-1">
                          分享 URL
                        </div>
                        <div class="text-body-2 share-url-text">
                          {{ shareStatus?.serverUrl }}
                        </div>
                      </v-sheet>
                    </v-card-text>
                  </v-card>
                </v-dialog>

                <v-card
                  v-if="isProcessing && processingProgress"
                  class="mt-4"
                  variant="tonal"
                >
                  <v-card-title class="text-subtitle-1"
                    >正在處理檔案</v-card-title
                  >
                  <v-card-text>
                    <div class="text-body-2 mb-2">
                      {{ processingProgress.currentFileIndex }} /
                      {{ processingProgress.totalFiles }} ・
                      {{ processingProgress.currentFileName }}
                    </div>
                    <UiProgressBar
                      :value="processingProgress.percentage"
                      color="info"
                      :caption="`${formatBytes(processingProgress.bytesProcessed)} / ${formatBytes(processingProgress.totalBytes)}`"
                    />
                  </v-card-text>
                </v-card>

                <v-card
                  v-if="shareStatus?.session"
                  class="mt-4"
                  variant="outlined"
                >
                  <v-card-title class="text-subtitle-1"
                    >目前分享檔案</v-card-title
                  >
                  <v-card-subtitle>
                    {{ shareStatus.session.fileCount }} 個檔案 ・
                    {{ formatBytes(shareStatus.session.totalSize) }} ・ 版本
                    {{ shareStatus.session.revision }}
                  </v-card-subtitle>
                  <v-data-table
                    :items="activeFiles"
                    :headers="[
                      { title: '檔名', value: 'fileName' },
                      { title: '大小', value: 'fileSize' },
                      { title: 'Piece', value: 'pieceCount' },
                    ]"
                    :items-per-page="5"
                    density="comfortable"
                  >
                    <template #item.fileSize="{ item }">
                      {{ formatBytes(item.fileSize) }}
                    </template>
                  </v-data-table>
                </v-card>
              </v-card-text>
            </v-card>
          </v-col>

          <v-col cols="12" lg="4">
            <v-card rounded="xl" class="mb-4" variant="elevated">
              <v-card-title class="text-h6">狀態摘要</v-card-title>
              <v-card-subtitle>即時資訊</v-card-subtitle>
              <v-list lines="two" density="comfortable">
                <v-list-item
                  title="分享狀態"
                  :subtitle="insights.shareState"
                  prepend-icon="mdi-broadcast"
                />
                <v-list-item
                  title="可達性"
                  :subtitle="insights.reachability"
                  prepend-icon="mdi-lan-connect"
                />
                <v-list-item
                  title="活躍下載"
                  :subtitle="`${insights.activeDownloads} 個`"
                  prepend-icon="mdi-download-network"
                />
                <v-list-item
                  title="最近活動"
                  :subtitle="insights.recentActivityLabel"
                  prepend-icon="mdi-timeline-clock"
                />
                <v-list-item
                  title="建議下一步"
                  :subtitle="insights.nextActionHint"
                  prepend-icon="mdi-lightbulb-on-outline"
                />
              </v-list>
              <v-divider />
              <v-card-text>
                <div class="text-body-2 mb-1">
                  目前下載端數量：<strong>{{
                    metrics.activeClientCount
                  }}</strong>
                </div>
                <div class="text-body-2 mb-1">
                  累計 HTTP 上傳：<strong>{{
                    formatBytes(metrics.httpUploadedBytes)
                  }}</strong>
                </div>
                <div class="text-body-2 mb-1">
                  清單版本：<strong>{{ metrics.metadataRevision }}</strong>
                </div>
                <div class="text-body-2">
                  最近活動時間：<strong>{{
                    formatTime(metrics.lastActivityUnixMs)
                  }}</strong>
                </div>
                <v-alert
                  v-if="insights.recentError"
                  type="error"
                  variant="tonal"
                  density="compact"
                  class="mt-3"
                >
                  近期錯誤：{{ insights.recentError }}
                </v-alert>
              </v-card-text>
            </v-card>
          </v-col>
        </v-row>
      </v-container>
    </v-main>
  </v-app>
</template>

<style scoped>
.main-bg {
  min-height: 100vh;
  background:
    radial-gradient(
      circle at 12% 18%,
      rgba(16, 185, 129, 0.16),
      transparent 36%
    ),
    radial-gradient(
      circle at 88% 12%,
      rgba(249, 115, 22, 0.16),
      transparent 30%
    ),
    linear-gradient(135deg, #f5f7f2 0%, #f4faf9 42%, #f8f5ef 100%);
}

.share-url-block {
  flex: 1 1 360px;
  min-width: 0;
}

.share-url-text {
  overflow-wrap: anywhere;
  word-break: break-word;
}

.qr-trigger {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  border: 0;
  border-radius: 18px;
  padding: 10px;
  background: linear-gradient(
    180deg,
    rgba(255, 255, 255, 0.95),
    rgba(232, 245, 241, 0.95)
  );
  box-shadow: inset 0 0 0 1px rgba(15, 118, 110, 0.16);
  cursor: pointer;
  transition:
    transform 0.18s ease,
    box-shadow 0.18s ease;
}

.qr-trigger:hover:not(:disabled) {
  transform: translateY(-1px);
  box-shadow:
    inset 0 0 0 1px rgba(15, 118, 110, 0.24),
    0 10px 24px rgba(15, 23, 42, 0.1);
}

.qr-trigger:disabled {
  cursor: wait;
}

.qr-trigger__label {
  font-size: 12px;
  font-weight: 600;
  color: rgb(15, 118, 110);
}

.qr-thumb,
.qr-thumb--placeholder {
  width: 78px;
  height: 78px;
  border-radius: 14px;
  background: white;
}

.qr-thumb {
  display: block;
  object-fit: cover;
}

.qr-thumb--placeholder {
  display: grid;
  place-items: center;
  color: rgb(15, 118, 110);
}

.qr-dialog-image {
  display: block;
  width: min(100%, 280px);
  margin: 0 auto;
  border-radius: 20px;
  background: white;
  box-shadow: 0 18px 48px rgba(15, 23, 42, 0.12);
}
</style>
