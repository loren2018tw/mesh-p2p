use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::{SocketAddr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::ConnectInfo,
    extract::Path as AxumPath,
    extract::Query,
    extract::State as AxumState,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tauri::{State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tokio::{
    fs,
    io::AsyncReadExt,
    sync::{oneshot, Semaphore},
};

const DEFAULT_PIECE_SIZE: usize = 256 * 1024;
const RATE_LIMIT_WINDOW_MS: u64 = 1000;
const RATE_LIMIT_MAX: usize = 5000;
const CLIENT_ACTIVITY_WINDOW_SECS: u64 = 300;
const METADATA_VERSION: u16 = 1;
const MIN_SUPPORTED_METADATA_VERSION: u16 = 1;
const MAX_SUPPORTED_METADATA_VERSION: u16 = METADATA_VERSION;
const APP_VERSION_PLACEHOLDER: &str = "__APP_VERSION__";
const MAX_CHUNK_SIZE: usize = 50 * 1024 * 1024; // 改為 50 MB (原 200 MB)
const MAX_CONCURRENT_RANGES: usize = 100; // 最多 100 並行 range requests

static APP_VERSION: OnceLock<String> = OnceLock::new();

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SharedFile {
    file_id: String,
    file_name: String,
    file_path: String,
    file_size: u64,
    info_hash: String,
    piece_size: usize,
    piece_count: usize,
    magnet_uri: String,
    content_signature: String,
    seed_reused: bool,
    #[serde(skip_serializing)]
    torrent_bytes: Vec<u8>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ShareSession {
    session_id: String,
    files: Vec<SharedFile>,
    file_count: usize,
    total_size: u64,
    revision: u64,
    last_updated_unix_ms: u128,
    tracker_urls: Vec<String>,
    started_at_unix_ms: u128,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ShareMetrics {
    active_client_count: usize,
    http_uploaded_bytes: u64,
    p2p_uploaded_bytes: u64,
    active_p2p_peer_count: usize,
    fallback_transfer_count: u64,
    seeding_peer_count: usize,
    metadata_revision: u64,
    last_activity_unix_ms: u128,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ShareStatus {
    is_sharing: bool,
    server_url: String,
    fallback_http_enabled: bool,
    session: Option<ShareSession>,
    metrics: ShareMetrics,
    insights: ShareInsights,
    processing_progress: Option<ProcessingProgress>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ShareInsights {
    share_state: String,
    reachability: String,
    active_downloads: usize,
    seeding_peers: usize,
    recent_error: Option<String>,
    recent_activity_label: String,
    next_action_hint: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingProgress {
    is_processing: bool,
    current_file_name: String,
    current_file_index: usize,
    total_files: usize,
    bytes_processed: u64,
    total_bytes: u64,
    percentage: u8, // 0-100
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartShareResponse {
    server_url: String,
    session: ShareSession,
}

#[derive(Clone)]
pub struct ShareState {
    inner: Arc<Mutex<ShareRuntime>>,
}

#[derive(Debug)]
struct ShareRuntime {
    server: Option<ServerRuntime>,
    session: Option<ShareSession>,
    tracker_urls: Vec<String>,
    fallback_http_enabled: bool,
    http_uploaded_bytes: u64,
    p2p_uploaded_bytes: u64,
    active_p2p_peer_count: usize,
    fallback_transfer_count: u64,
    seeding_peer_count: usize,
    metadata_revision: u64,
    last_activity_unix_ms: u128,
    client_activity: VecDeque<ClientActivity>,
    client_reports: HashMap<String, ClientReportSnapshot>,
    last_error: Option<String>,
    processing_progress: Option<ProcessingProgress>,
    processing_cancel_requested: bool,
}

#[derive(Debug)]
struct ServerRuntime {
    base_url: String,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

#[derive(Clone)]
struct HttpState {
    runtime: Arc<Mutex<ShareRuntime>>,
    limiter: Arc<Mutex<VecDeque<Instant>>>,
    range_semaphore: Arc<Semaphore>, // 限制並行 range requests
}

#[derive(Debug)]
struct ClientActivity {
    ip: String,
    seen_at: Instant,
}

#[derive(Debug, Clone)]
struct ClientReportSnapshot {
    p2p_uploaded_bytes: u64,
    active_peers: usize,
    is_seeding: bool,
    last_seen: Instant,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    error: String,
    error_code: Option<String>,
    upgrade_hint: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataQuery {
    metadata_version: Option<u16>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientStatsReport {
    client_id: String,
    file_id: String,
    p2p_uploaded_bytes: u64,
    active_peers: usize,
    is_seeding: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientStatsAck {
    accepted: bool,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MetadataResponse {
    metadata_version: u16,
    min_supported_metadata_version: u16,
    max_supported_metadata_version: u16,
    session_id: String,
    files: Vec<SharedFile>,
    file_count: usize,
    total_size: u64,
    revision: u64,
    last_updated_unix_ms: u128,
    tracker_urls: Vec<String>,
    started_at_unix_ms: u128,
    fallback_http_enabled: bool,
}

impl ShareState {
    pub fn new() -> Self {
        let tracker_urls = load_trackers_from_env();
        let fallback_http_enabled = load_fallback_flag();

        Self {
            inner: Arc::new(Mutex::new(ShareRuntime {
                server: None,
                session: None,
                tracker_urls,
                fallback_http_enabled,
                http_uploaded_bytes: 0,
                p2p_uploaded_bytes: 0,
                active_p2p_peer_count: 0,
                fallback_transfer_count: 0,
                seeding_peer_count: 0,
                metadata_revision: 0,
                last_activity_unix_ms: unix_time_ms(),
                client_activity: VecDeque::new(),
                client_reports: HashMap::new(),
                last_error: None,
                processing_progress: None,
                processing_cancel_requested: false,
            })),
        }
    }
}

#[tauri::command]
pub fn pick_share_file(window: WebviewWindow) -> Result<Option<String>, String> {
    let selected = window
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("選擇要分享的檔案")
        .blocking_pick_file();

    Ok(selected.and_then(|file| {
        file.into_path()
            .ok()
            .map(|path| path.to_string_lossy().to_string())
    }))
}

#[tauri::command]
pub async fn pick_share_files(window: WebviewWindow) -> Result<Vec<String>, String> {
    let selected = tokio::task::spawn_blocking(move || {
        window
            .dialog()
            .file()
            .set_parent(&window)
            .set_title("選擇要分享的檔案")
            .blocking_pick_files()
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();

    Ok(selected
        .into_iter()
        .filter_map(|file| file.into_path().ok())
        .map(|path| path.to_string_lossy().to_string())
        .collect())
}

#[tauri::command]
pub async fn add_share_files(
    state: State<'_, ShareState>,
    file_paths: Vec<String>,
) -> Result<ShareSession, String> {
    if file_paths.is_empty() {
        return Err("Please select at least one file to add".to_string());
    }

    let (tracker_urls, base_url, existing_paths, next_index, file_count) = {
        let guard = state
            .inner
            .lock()
            .map_err(|_| "State lock poisoned".to_string())?;
        let session = guard
            .session
            .as_ref()
            .ok_or_else(|| "Share session is not active".to_string())?;

        (
            guard.tracker_urls.clone(),
            guard
                .server
                .as_ref()
                .map(|server| server.base_url.clone())
                .ok_or_else(|| "Share server is not running".to_string())?,
            session
                .files
                .iter()
                .map(|file| file.file_path.clone())
                .collect::<HashSet<_>>(),
            session.files.len(),
            file_paths.len(),
        )
    };

    // Initialize progress tracking
    {
        let mut guard = state
            .inner
            .lock()
            .map_err(|_| "State lock poisoned".to_string())?;
        guard.processing_cancel_requested = false;
        guard.processing_progress = Some(ProcessingProgress {
            is_processing: true,
            current_file_name: String::new(),
            current_file_index: 0,
            total_files: file_count,
            bytes_processed: 0,
            total_bytes: 0,
            percentage: 0,
        });
    }

    let state_clone = state.inner.clone();
    let new_files = match build_shared_files(
        file_paths,
        &tracker_urls,
        &base_url,
        &existing_paths,
        next_index,
        {
            let state = state_clone.clone();
            move |file_name, file_idx, file_total_size, bytes_processed| {
                let mut guard = state
                    .lock()
                    .map_err(|_| "State lock poisoned".to_string())?;

                if guard.processing_cancel_requested {
                    return Err("分享已停止".to_string());
                }

                if let Some(ref mut progress) = guard.processing_progress {
                    progress.current_file_name = file_name.clone();
                    progress.current_file_index = file_idx;
                    progress.total_bytes = file_total_size;
                    progress.bytes_processed = bytes_processed;
                    if file_total_size > 0 {
                        progress.percentage =
                            ((bytes_processed as f64 / file_total_size as f64) * 100.0) as u8;
                    }
                }

                Ok(())
            }
        },
    )
    .await
    {
        Ok(files) => files,
        Err(err) => {
            if let Ok(mut guard) = state.inner.lock() {
                guard.last_error = Some(err.clone());
                guard.processing_progress = None;
            }
            return Err(err);
        }
    };

    let mut guard = state
        .inner
        .lock()
        .map_err(|_| "State lock poisoned".to_string())?;
    guard.metadata_revision += 1;
    guard.last_activity_unix_ms = unix_time_ms();
    guard.last_error = None;
    guard.processing_progress = None; // Clear progress after completion
    guard.processing_cancel_requested = false;
    let revision = guard.metadata_revision;
    let last_updated_unix_ms = guard.last_activity_unix_ms;

    let session = guard
        .session
        .as_mut()
        .ok_or_else(|| "Share session is not active".to_string())?;

    session.files.extend(new_files);
    refresh_session_summary(session, revision, last_updated_unix_ms);

    Ok(session.clone())
}

#[tauri::command]
pub async fn start_share(
    state: State<'_, ShareState>,
    file_paths: Vec<String>,
) -> Result<StartShareResponse, String> {
    if file_paths.is_empty() {
        return Err("Please select at least one file to share".to_string());
    }

    let server_url = ensure_server_running(state.inner.clone()).await?;

    let (tracker_urls, file_count) = {
        let guard = state
            .inner
            .lock()
            .map_err(|_| "State lock poisoned".to_string())?;
        (guard.tracker_urls.clone(), file_paths.len())
    };

    // Initialize progress tracking
    {
        let mut guard = state
            .inner
            .lock()
            .map_err(|_| "State lock poisoned".to_string())?;
        guard.processing_cancel_requested = false;
        guard.processing_progress = Some(ProcessingProgress {
            is_processing: true,
            current_file_name: String::new(),
            current_file_index: 0,
            total_files: file_count,
            bytes_processed: 0,
            total_bytes: 0,
            percentage: 0,
        });
    }

    let state_clone = state.inner.clone();
    let files = match build_shared_files(
        file_paths,
        &tracker_urls,
        &server_url,
        &HashSet::new(),
        0,
        {
            let state = state_clone.clone();
            move |file_name, file_idx, file_total_size, bytes_processed| {
                let mut guard = state
                    .lock()
                    .map_err(|_| "State lock poisoned".to_string())?;

                if guard.processing_cancel_requested {
                    return Err("分享已停止".to_string());
                }

                if let Some(ref mut progress) = guard.processing_progress {
                    progress.current_file_name = file_name.clone();
                    progress.current_file_index = file_idx;
                    progress.total_bytes = file_total_size;
                    progress.bytes_processed = bytes_processed;
                    if file_total_size > 0 {
                        progress.percentage =
                            ((bytes_processed as f64 / file_total_size as f64) * 100.0) as u8;
                    }
                }

                Ok(())
            }
        },
    )
    .await
    {
        Ok(files) => files,
        Err(err) => {
            if let Ok(mut guard) = state.inner.lock() {
                guard.last_error = Some(err.clone());
                guard.processing_progress = None;
            }
            return Err(err);
        }
    };
    let started_at = unix_time_ms();
    let total_size = files.iter().map(|file| file.file_size).sum();

    let session = ShareSession {
        session_id: generate_session_id(),
        file_count: files.len(),
        total_size,
        revision: 1,
        last_updated_unix_ms: started_at,
        files,
        tracker_urls: tracker_urls.to_vec(),
        started_at_unix_ms: started_at,
    };

    {
        let mut guard = state
            .inner
            .lock()
            .map_err(|_| "State lock poisoned".to_string())?;
        guard.http_uploaded_bytes = 0;
        guard.p2p_uploaded_bytes = 0;
        guard.active_p2p_peer_count = 0;
        guard.fallback_transfer_count = 0;
        guard.seeding_peer_count = 0;
        guard.metadata_revision = 1;
        guard.last_activity_unix_ms = started_at;
        guard.last_error = None;
        guard.client_activity.clear();
        guard.client_reports.clear();
        guard.processing_progress = None; // Clear progress after completion
        guard.processing_cancel_requested = false;
        guard.session = Some(session.clone());
    }

    Ok(StartShareResponse {
        server_url,
        session,
    })
}

#[tauri::command]
pub async fn stop_share(state: State<'_, ShareState>) -> Result<bool, String> {
    let mut guard = state
        .inner
        .lock()
        .map_err(|_| "State lock poisoned".to_string())?;
    let existed = guard.session.is_some() || guard.processing_progress.is_some();
    guard.processing_cancel_requested = true;
    guard.processing_progress = None;
    guard.client_reports.clear();
    guard.seeding_peer_count = 0;
    guard.p2p_uploaded_bytes = 0;
    guard.active_p2p_peer_count = 0;
    guard.session = None;
    Ok(existed)
}

#[tauri::command]
pub fn get_share_status(state: State<'_, ShareState>) -> Result<ShareStatus, String> {
    let guard = state
        .inner
        .lock()
        .map_err(|_| "State lock poisoned".to_string())?;

    Ok(ShareStatus {
        is_sharing: guard.session.is_some(),
        server_url: guard
            .server
            .as_ref()
            .map(|s| s.base_url.clone())
            .unwrap_or_default(),
        fallback_http_enabled: guard.fallback_http_enabled,
        session: guard.session.clone(),
        metrics: build_metrics(&guard),
        insights: build_insights(&guard),
        processing_progress: guard.processing_progress.clone(),
    })
}

async fn ensure_server_running(runtime: Arc<Mutex<ShareRuntime>>) -> Result<String, String> {
    {
        let guard = runtime
            .lock()
            .map_err(|_| "State lock poisoned".to_string())?;
        if let Some(server) = &guard.server {
            return Ok(server.base_url.clone());
        }
    }

    let listener = TcpListener::bind("0.0.0.0:0")
        .map_err(|e| format!("Failed to bind local web server: {e}"))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("Failed to read local address: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("Failed to set listener non-blocking: {e}"))?;

    let host_ip = detect_host_ip();
    let base_url = format!("http://{}:{}", host_ip, addr.port());

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let http_state = HttpState {
        runtime: runtime.clone(),
        limiter: Arc::new(Mutex::new(VecDeque::new())),
        range_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_RANGES)),
    };

    let router = Router::new()
        .route("/", get(download_page_handler))
        .route("/webtorrent.min.js", get(webtorrent_js_handler))
        .route("/mesh.png", get(mesh_png_handler))
        .route("/api/metadata", get(metadata_handler))
        .route("/api/client-stats", post(client_stats_handler))
        .route("/api/torrent/{file_id}", get(torrent_handler))
        .route("/api/file/{file_id}", get(file_handler))
        .route("/api/health", get(health_handler))
        .with_state(http_state)
        .layer(tower_http::cors::CorsLayer::permissive());

    let tokio_listener = tokio::net::TcpListener::from_std(listener)
        .map_err(|e| format!("Failed to convert listener: {e}"))?;

    tauri::async_runtime::spawn(async move {
        let server = axum::serve(
            tokio_listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });

        if let Err(err) = server.await {
            eprintln!("Share server failed: {err}");
        }
    });

    {
        let mut guard = runtime
            .lock()
            .map_err(|_| "State lock poisoned".to_string())?;
        guard.server = Some(ServerRuntime {
            base_url: base_url.clone(),
            shutdown_tx: Some(shutdown_tx),
        });
    }

    Ok(base_url)
}

fn enforce_rate_limit(state: &HttpState) -> Result<(), StatusCode> {
    let now = Instant::now();

    let mut limiter = state
        .limiter
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    while let Some(oldest) = limiter.front() {
        if now.duration_since(*oldest) > Duration::from_millis(RATE_LIMIT_WINDOW_MS) {
            let _ = limiter.pop_front();
        } else {
            break;
        }
    }

    if limiter.len() >= RATE_LIMIT_MAX {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    limiter.push_back(now);
    Ok(())
}

static WEBTORRENT_JS_RAW: &[u8] =
    include_bytes!("../../node_modules/webtorrent/dist/webtorrent.min.js");
static WEBTORRENT_JS_PATCHED: OnceLock<String> = OnceLock::new();
static MESH_PNG: &[u8] = include_bytes!("../icons/mesh.png");

fn get_webtorrent_js() -> &'static str {
    WEBTORRENT_JS_PATCHED.get_or_init(|| {
        let src = std::str::from_utf8(WEBTORRENT_JS_RAW).expect("webtorrent.min.js is valid UTF-8");
        // The file ends with ES module `export{X as default}` — patch to a global assignment
        // so it can be used as a classic <script> without type="module"
        if let Some(pos) = src.rfind("export{") {
            if let Some(close_offset) = src[pos..].find('}') {
                let inner = &src[pos + 7..pos + close_offset]; // e.g. "Kt as default"
                let var_name = inner.split_whitespace().next().unwrap_or("_WT");
                return format!("{}globalThis.WebTorrent={};", &src[..pos], var_name);
            }
        }
        src.to_string()
    })
}

async fn webtorrent_js_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "application/javascript"),
            (axum::http::header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        get_webtorrent_js(),
    )
}

async fn mesh_png_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "image/png")],
        MESH_PNG,
    )
}

async fn download_page_handler() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="zh-Hant">
    <head>
        <meta charset="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <title>Mesh P2P Download</title>
        <link rel="icon" type="image/png" href="/mesh.png" />
        <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/@mdi/font@7.4.47/css/materialdesignicons.min.css" />
        <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/vuetify@3.10.8/dist/vuetify.min.css" />
        <style>
            html, body, #app { height: 100%; margin: 0; }
            body {
                background:
                    radial-gradient(circle at 10% 14%, rgba(16, 185, 129, 0.2), transparent 30%),
                    radial-gradient(circle at 92% 10%, rgba(249, 115, 22, 0.2), transparent 28%),
                    linear-gradient(135deg, #f5f7f2 0%, #eef8f6 50%, #f9f4ea 100%);
            }
            .tiny { font-size: 12px; color: #475569; }
        </style>
    </head>
    <body>
        <div id="app"></div>

        <script src="https://unpkg.com/vue@3/dist/vue.global.prod.js"></script>
        <script src="https://cdn.jsdelivr.net/npm/vuetify@3.10.8/dist/vuetify.min.js"></script>
        <script src="https://cdn.jsdelivr.net/npm/js-sha1@0.6.0/src/sha1.min.js"></script>
        <script>
            // Polyfill Web Crypto API for insecure contexts (LAN HTTP)
            if (!window.crypto) window.crypto = {};
            if (!window.crypto.getRandomValues) {
                window.crypto.getRandomValues = function(buf) {
                    for (let i = 0; i < buf.length; i++) buf[i] = Math.floor(Math.random() * 256);
                    return buf;
                };
            }
            if (!window.crypto.subtle) window.crypto.subtle = {};
            if (!window.crypto.subtle.digest) {
                window.crypto.subtle.digest = function(algo, data) {
                    return new Promise((resolve, reject) => {
                        try {
                            const isSha1 = typeof algo === 'string' 
                                ? algo.toUpperCase() === 'SHA-1' 
                                : (algo && algo.name && algo.name.toUpperCase() === 'SHA-1');
                            if (isSha1) {
                                resolve(sha1.arrayBuffer(data));
                            } else {
                                reject(new Error('Polyfill only supports SHA-1'));
                            }
                        } catch (e) {
                            reject(e);
                        }
                    });
                };
            }
        </script>
        <script src="/webtorrent.min.js"></script>
        <script>
            const { createApp, ref, computed, onMounted, onUnmounted } = Vue;
            const { createVuetify } = Vuetify;

            const vuetify = createVuetify({
                theme: {
                    defaultTheme: 'mesh',
                    themes: {
                        mesh: {
                            dark: false,
                            colors: {
                                primary: '#0f766e',
                                secondary: '#14532d',
                                accent: '#c2410c',
                                warning: '#a16207',
                                error: '#b91c1c'
                            }
                        }
                    }
                }
            });

            function formatBytes(bytes) {
                if (!bytes) return '0 B';
                const k = 1024;
                const units = ['B', 'KB', 'MB', 'GB', 'TB'];
                const idx = Math.min(Math.floor(Math.log(bytes) / Math.log(k)), units.length - 1);
                const val = bytes / Math.pow(k, idx);
                return `${val.toFixed(idx === 0 ? 0 : 2)} ${units[idx]}`;
            }

            function nowMs() {
                return Date.now();
            }

            const SUPPORTED_METADATA_VERSION = 1;
            const CLIENT_ID = `dl-${Math.random().toString(36).slice(2)}-${Date.now()}`;

            createApp({
                setup() {
                    const statusText = ref('載入中...');
                    const warningText = ref('');
                    const metadata = ref(null);
                    const metadataError = ref('');
                    const lastRevision = ref(-1);
                    const timerId = ref(null);
                    const downloads = ref({});
                    let torrentClient = null;
                    const torrentSessions = {};

                    const files = computed(() => metadata.value?.files ?? []);

                    function ensureClient() {
                        if (!torrentClient) {
                            if (!globalThis.WebTorrent) {
                                throw new Error('WebTorrent client unavailable');
                            }
                            torrentClient = new globalThis.WebTorrent();
                        }
                        return torrentClient;
                    }

                    function ensureDownloadState(fileId) {
                        if (!downloads.value[fileId]) {
                            downloads.value[fileId] = {
                                phase: 'idle',
                                progressPercent: 0,
                                bytesReceived: 0,
                                totalBytes: 0,
                                speedBps: 0,
                                etaSeconds: 0,
                                sourceMix: 'P2P + HTTP metadata 初始化',
                                errorCode: null,
                                startedAt: 0,
                                lastTickAt: 0,
                                smoothedSpeedBps: 0,
                            };
                        }
                        return downloads.value[fileId];
                    }

                    async function reportClientStats(file, torrent, isSeeding) {
                        try {
                            const resp = await fetch('/api/client-stats', {
                                method: 'POST',
                                headers: {
                                    'content-type': 'application/json',
                                },
                                body: JSON.stringify({
                                    clientId: CLIENT_ID,
                                    fileId: file.fileId,
                                    p2pUploadedBytes: Math.round(torrent.uploaded || 0),
                                    activePeers: Number(torrent.numPeers || 0),
                                    isSeeding,
                                }),
                                keepalive: true,
                            });

                            if (resp.status === 410) {
                                warningText.value = '分享已停止，正在回收既有 P2P session';
                                destroyAllSessions();
                            }
                        } catch (error) {
                            warningText.value = String(error);
                        }
                    }

                    async function loadMetadata() {
                        metadataError.value = '';
                        warningText.value = '';
                        try {
                            const resp = await fetch(`/api/metadata?metadataVersion=${SUPPORTED_METADATA_VERSION}`, { cache: 'no-store' });
                            if (!resp.ok) {
                                let errMsg = '目前沒有可用分享。';
                                try {
                                    const errData = await resp.json();
                                    if (errData?.errorCode === 'METADATA_VERSION_UNSUPPORTED') {
                                        errMsg = `${errData.error} ${errData.upgradeHint || ''}`.trim();
                                    }
                                } catch (_) {
                                }
                                statusText.value = '目前沒有可用分享。';
                                metadataError.value = errMsg;
                                metadata.value = null;
                                destroyAllSessions();
                                return;
                            }

                            const data = await resp.json();
                            if (typeof data.metadataVersion !== 'number') {
                                metadataError.value = 'metadata 版本資訊缺失';
                                statusText.value = 'metadata 格式不支援';
                                return;
                            }

                            metadata.value = data;
                            statusText.value = `分享中，已同步 ${data.fileCount} 個檔案`;

                            if (lastRevision.value !== data.revision) {
                                lastRevision.value = data.revision;
                            }

                            if (data.fallbackHttpEnabled) {
                                warningText.value = '目前啟用 HTTP fallback 模式';
                            }
                        } catch (error) {
                            metadataError.value = String(error);
                            statusText.value = '無法連線到分享服務。';
                        }
                    }

                    function destroySession(fileId, nextPhase = 'idle') {
                        const session = torrentSessions[fileId];
                        if (!session) {
                            return;
                        }
                        if (session.tickId) {
                            clearInterval(session.tickId);
                        }
                        if (session.torrent) {
                            void reportClientStats(session.file, session.torrent, false);
                            session.torrent.destroy({ destroyStore: false }, () => {});
                        }
                        delete torrentSessions[fileId];

                        const state = ensureDownloadState(fileId);
                        state.phase = nextPhase;
                        state.speedBps = 0;
                        state.etaSeconds = 0;
                        state.sourceMix = '已停止';
                    }

                    function destroyAllSessions() {
                        Object.keys(torrentSessions).forEach((fileId) => {
                            destroySession(fileId, 'idle');
                        });
                        if (torrentClient) {
                            torrentClient.destroy(() => {});
                            torrentClient = null;
                        }
                    }

                    async function fetchTorrentBytes(fileId) {
                        const resp = await fetch(`/api/torrent/${fileId}`);
                        if (!resp.ok) {
                            throw new Error(`取得 torrent 描述失敗：HTTP ${resp.status}`);
                        }
                        const buffer = await resp.arrayBuffer();
                        // WebTorrent 在瀏覽器中只接受 Blob、字串(URL/Magnet) 等；
                        // Uint8Array 會被誤認為無效的識別碼而拋出 "Invalid torrent identifier"。
                        if (typeof Buffer !== 'undefined') {
                            return Buffer.from(buffer);
                        }
                        return new Blob([buffer], { type: 'application/x-bittorrent' });
                    }

                    function updateStateFromTorrent(file, torrent) {
                        const state = ensureDownloadState(file.fileId);
                        state.totalBytes = torrent.length || file.fileSize || 0;
                        state.bytesReceived = torrent.downloaded || 0;
                        state.progressPercent = state.totalBytes > 0
                            ? Math.min(100, Math.round((state.bytesReceived / state.totalBytes) * 100))
                            : 0;
                        state.speedBps = Math.round(torrent.downloadSpeed || 0);
                        state.etaSeconds = state.speedBps > 0 && state.totalBytes > state.bytesReceived
                            ? Math.ceil((state.totalBytes - state.bytesReceived) / state.speedBps)
                            : 0;
                        state.sourceMix = torrent.numPeers > 0 ? 'P2P + HTTP web seed' : 'HTTP web seed fallback';
                        state.lastTickAt = nowMs();
                        void reportClientStats(file, torrent, state.phase === 'seeding' || torrent.progress === 1);
                    }

                    function persistTorrentFile(file, torrent) {
                        return new Promise((resolve, reject) => {
                            const target = torrent.files?.find((entry) => entry.name === file.fileName) || torrent.files?.[0];
                            if (!target) {
                                reject(new Error('torrent 內容為空'));
                                return;
                            }
                            
                            if (typeof target.blob === 'function') {
                                target.blob().then(blob => {
                                    saveBlob(file.fileName, blob);
                                    resolve();
                                }).catch(reject);
                            } else if (typeof target.getBlob === 'function') {
                                target.getBlob((error, blob) => {
                                    if (error) {
                                        reject(error);
                                        return;
                                    }
                                    saveBlob(file.fileName, blob);
                                    resolve();
                                });
                            } else if (typeof target.getBlobURL === 'function') {
                                target.getBlobURL((error, url) => {
                                    if (error) {
                                        reject(error);
                                    } else {
                                        const link = document.createElement('a');
                                        link.href = url;
                                        link.download = file.fileName;
                                        document.body.appendChild(link);
                                        link.click();
                                        link.remove();
                                        resolve();
                                    }
                                });
                            } else {
                                reject(new Error('無法獲取下載檔案 (WebTorrent API 不支援)'));
                            }
                        });
                    }

                    function saveBlob(fileName, blob) {
                        const url = URL.createObjectURL(blob);
                        const link = document.createElement('a');
                        link.href = url;
                        link.download = fileName;
                        document.body.appendChild(link);
                        link.click();
                        link.remove();
                        URL.revokeObjectURL(url);
                    }

                    async function downloadFile(file) {
                        const state = ensureDownloadState(file.fileId);
                        if (state.phase === 'downloading' || state.phase === 'seeding') {
                            return;
                        }

                        state.phase = 'downloading';
                        state.progressPercent = 0;
                        state.bytesReceived = 0;
                        state.totalBytes = file.fileSize || 0;
                        state.speedBps = 0;
                        state.etaSeconds = 0;
                        state.errorCode = null;
                        state.startedAt = nowMs();
                        state.lastTickAt = state.startedAt;
                        state.smoothedSpeedBps = 0;

                        try {
                            const client = ensureClient();
                            // 永遠使用 .torrent bytes：包含完整 metadata（pieces 雜湊值），
                            // web seed 才能在無 P2P peer 的 LAN 環境下正常下載。
                            // 若改用 magnet URI，WebTorrent 需要先向 peer 取 metadata，
                            // 在沒有外部 peer 的情況下永遠卡在 0%。
                            const torrentBytes = await fetchTorrentBytes(file.fileId);

                            const torrent = client.add(torrentBytes, { destroyStoreOnDestroy: false });
                            
                            // 限制並行 peer 連線數 (改進 1)
                            const MAX_CONCURRENT_PEERS = 15;
                            if (typeof torrent.setMaxConns === 'function') {
                                torrent.setMaxConns(MAX_CONCURRENT_PEERS);
                            }
                            
                            // error handler 必須最先附上，避免 EventEmitter 在 handler 尚未
                            // 附上前發出 'error' 事件而變成 Uncaught 錯誤。
                            torrent.on('error', (error) => {
                                destroySession(file.fileId, 'error');
                                state.errorCode = String(error);
                            });

                            const tickId = setInterval(() => updateStateFromTorrent(file, torrent), 1000);
                            torrentSessions[file.fileId] = { file, torrent, tickId };

                            torrent.on('download', () => updateStateFromTorrent(file, torrent));
                            torrent.on('wire', () => updateStateFromTorrent(file, torrent));
                            torrent.on('warning', (warning) => {
                                const msg = String(warning);
                                // 過濾 tracker WebSocket 連線失敗（LAN 環境下外部 tracker 必然失敗，非真正錯誤）
                                if (!msg.includes('WebSocket') && !msg.includes('tracker') && !msg.includes('wss://')) {
                                    warningText.value = msg;
                                }
                                updateStateFromTorrent(file, torrent);
                            });
                            torrent.on('done', async () => {
                                updateStateFromTorrent(file, torrent);
                                try {
                                    await persistTorrentFile(file, torrent);
                                    state.phase = 'seeding';
                                    state.progressPercent = 100;
                                    state.etaSeconds = 0;
                                    state.sourceMix = torrent.numPeers > 0 ? 'P2P seeding' : 'HTTP web seed 完成';
                                    void reportClientStats(file, torrent, true);
                                } catch (error) {
                                    state.phase = 'error';
                                    state.errorCode = String(error);
                                }
                            });
                        } catch (error) {
                            state.phase = 'error';
                            state.errorCode = String(error);
                        }
                    }

                    function stopTransfer(fileId) {
                        const state = ensureDownloadState(fileId);
                        const nextPhase = (state.bytesReceived >= state.totalBytes && state.totalBytes > 0) ? 'downloaded' : 'idle';
                        destroySession(fileId, nextPhase);
                    }

                    function phaseColor(phase) {
                        if (phase === 'seeding') return 'success';
                        if (phase === 'downloaded') return 'secondary';
                        if (phase === 'downloading') return 'info';
                        if (phase === 'error') return 'error';
                        return 'grey';
                    }

                    function phaseLabel(phase) {
                        if (phase === 'seeding') return '已下載並分享中';
                        if (phase === 'downloaded') return '已下載';
                        if (phase === 'downloading') return '下載中';
                        if (phase === 'error') return '失敗';
                        return '待下載';
                    }

                    onMounted(() => {
                        loadMetadata();
                        timerId.value = setInterval(loadMetadata, 5000);
                    });

                    onUnmounted(() => {
                        if (timerId.value) {
                            clearInterval(timerId.value);
                        }
                        destroyAllSessions();
                    });

                    return {
                        statusText,
                        warningText,
                        metadata,
                        metadataError,
                        files,
                        downloads,
                        loadMetadata,
                        downloadFile,
                        stopTransfer,
                        phaseColor,
                        phaseLabel,
                        formatBytes,
                    };
                },
                template: `
                    <v-app>
                        <v-main>
                            <v-container class="py-8" style="max-width: 980px">
                                <v-card rounded="xl">
                                    <v-card-title class="d-flex align-center justify-space-between ga-3">
                                        <span class="text-h5 font-weight-bold">Mesh P2P 分享下載頁</span>
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
                                    <v-card-subtitle>版本： v__APP_VERSION__ By Loren(loren.tw@gmail.com)</v-card-subtitle>
                                    <v-card-text>
                                        <v-alert type="info" variant="tonal" class="mb-3">{{ statusText }}</v-alert>
                                        <v-alert v-if="warningText" type="warning" variant="tonal" class="mb-3">{{ warningText }}</v-alert>
                                        <v-alert v-if="metadataError" type="error" variant="tonal" class="mb-3">
                                            讀取 metadata 失敗：{{ metadataError }}
                                            <template #append>
                                                <v-btn size="small" color="error" variant="text" @click="loadMetadata">重試</v-btn>
                                            </template>
                                        </v-alert>

                                        <v-card v-if="metadata" variant="outlined" class="mb-4">
                                            <v-card-text class="d-flex flex-wrap ga-4">
                                                <div>檔案數：<strong>{{ metadata.fileCount }}</strong></div>
                                                <div>總大小：<strong>{{ formatBytes(metadata.totalSize) }}</strong></div>
                                            </v-card-text>
                                        </v-card>

                                        <v-list v-if="files.length" lines="three" class="rounded-lg border">
                                            <v-list-item
                                                v-for="file in files"
                                                :key="file.fileId"
                                                :title="file.fileName"
                                                :subtitle="formatBytes(file.fileSize)"
                                            >
                                                <template #append>
                                                    <div style="min-width: 340px" class="d-flex align-center ga-2">
                                                        <v-chip
                                                            size="small"
                                                            :color="phaseColor(downloads[file.fileId]?.phase || 'idle')"
                                                            variant="flat"
                                                        >
                                                            {{ phaseLabel(downloads[file.fileId]?.phase || 'idle') }}
                                                        </v-chip>

                                                        <v-btn
                                                            size="small"
                                                            :color="downloads[file.fileId]?.phase === 'seeding' || downloads[file.fileId]?.phase === 'downloading' ? 'warning' : 'primary'"
                                                            @click="downloads[file.fileId]?.phase === 'seeding' || downloads[file.fileId]?.phase === 'downloading' ? stopTransfer(file.fileId) : downloadFile(file)"
                                                        >
                                                            {{ downloads[file.fileId]?.phase === 'seeding' || downloads[file.fileId]?.phase === 'downloading' ? '停止' : '下載' }}
                                                        </v-btn>
                                                    </div>
                                                </template>

                                                <template #subtitle>
                                                    <div>
                                                        <div class="mb-1">{{ formatBytes(file.fileSize) }}</div>
                                                        <v-progress-linear
                                                            v-if="downloads[file.fileId]?.phase === 'downloading'"
                                                            :model-value="downloads[file.fileId]?.progressPercent || 0"
                                                            color="info"
                                                            rounded
                                                            height="10"
                                                        />
                                                        <div v-if="downloads[file.fileId]?.phase === 'downloading'" class="tiny mt-1">
                                                            {{ downloads[file.fileId]?.progressPercent || 0 }}% ・
                                                            {{ formatBytes(downloads[file.fileId]?.bytesReceived || 0) }} /
                                                            {{ formatBytes(downloads[file.fileId]?.totalBytes || file.fileSize) }} ・
                                                            {{ formatBytes(downloads[file.fileId]?.speedBps || 0) }}/s ・
                                                            ETA {{ downloads[file.fileId]?.etaSeconds || 0 }}s
                                                        </div>
                                                        <div v-if="downloads[file.fileId]?.phase === 'downloading' || downloads[file.fileId]?.phase === 'seeding'" class="tiny mt-1">
                                                            {{ downloads[file.fileId]?.sourceMix || 'P2P + HTTP web seed' }}
                                                        </div>
                                                        <div v-if="downloads[file.fileId]?.phase === 'error'" class="tiny" style="color:#b91c1c">
                                                            {{ downloads[file.fileId]?.errorCode }}
                                                        </div>
                                                    </div>
                                                </template>
                                            </v-list-item>
                                        </v-list>

                                        <v-alert v-else type="warning" variant="tonal">目前沒有可下載檔案。</v-alert>
                                    </v-card-text>
                                </v-card>
                            </v-container>
                        </v-main>
                    </v-app>
                `
            }).use(vuetify).mount('#app');
        </script>
    </body>
</html>"#
            .replace(APP_VERSION_PLACEHOLDER, current_app_version()),
    )
}

async fn metadata_handler(
    Query(query): Query<MetadataQuery>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumState(state): AxumState<HttpState>,
) -> impl IntoResponse {
    if let Err(status) = enforce_rate_limit(&state) {
        return (
            status,
            Json(ErrorResponse {
                error: "Rate limit exceeded".to_string(),
                error_code: None,
                upgrade_hint: None,
            }),
        )
            .into_response();
    }

    if let Some(client_version) = query.metadata_version {
        if !(MIN_SUPPORTED_METADATA_VERSION..=MAX_SUPPORTED_METADATA_VERSION)
            .contains(&client_version)
        {
            return (
                StatusCode::UPGRADE_REQUIRED,
                Json(ErrorResponse {
                    error: format!(
                        "Unsupported metadata version: {client_version}. Supported range: {MIN_SUPPORTED_METADATA_VERSION}..={MAX_SUPPORTED_METADATA_VERSION}"
                    ),
                    error_code: Some("METADATA_VERSION_UNSUPPORTED".to_string()),
                    upgrade_hint: Some(format!(
                        "Please upgrade download page to metadataVersion={MAX_SUPPORTED_METADATA_VERSION}"
                    )),
                }),
            )
                .into_response();
        }
    }

    let mut guard = match state.runtime.lock() {
        Ok(g) => g,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "State lock poisoned".to_string(),
                    error_code: None,
                    upgrade_hint: None,
                }),
            )
                .into_response();
        }
    };

    record_client_activity(&mut guard, addr);

    match &guard.session {
        Some(session) => (
            StatusCode::OK,
            Json(build_metadata_response(
                session,
                guard.fallback_http_enabled,
            )),
        )
            .into_response(),
        None => (
            StatusCode::GONE,
            Json(ErrorResponse {
                error: "Share session is not active".to_string(),
                error_code: None,
                upgrade_hint: None,
            }),
        )
            .into_response(),
    }
}

async fn file_handler(
    headers: axum::http::HeaderMap,
    AxumPath(file_id): AxumPath<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumState(state): AxumState<HttpState>,
) -> impl IntoResponse {
    if let Err(status) = enforce_rate_limit(&state) {
        return (
            status,
            Json(ErrorResponse {
                error: "Rate limit exceeded".to_string(),
                error_code: None,
                upgrade_hint: None,
            }),
        )
            .into_response();
    }

    let file_path = {
        let mut guard = match state.runtime.lock() {
            Ok(g) => g,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "State lock poisoned".to_string(),
                        error_code: None,
                        upgrade_hint: None,
                    }),
                )
                    .into_response();
            }
        };

        record_client_activity(&mut guard, addr);

        match &guard.session {
            Some(session) => match session.files.iter().find(|file| file.file_id == file_id) {
                Some(file) => PathBuf::from(file.file_path.clone()),
                None => {
                    return (
                        StatusCode::NOT_FOUND,
                        Json(ErrorResponse {
                            error: "Shared file not found".to_string(),
                            error_code: None,
                            upgrade_hint: None,
                        }),
                    )
                        .into_response();
                }
            },
            None => {
                return (
                    StatusCode::GONE,
                    Json(ErrorResponse {
                        error: "Share session is not active".to_string(),
                        error_code: None,
                        upgrade_hint: None,
                    }),
                )
                    .into_response();
            }
        }
    };

    let mut file = match tokio::fs::File::open(&file_path).await {
        Ok(f) => f,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to open shared file: {err}"),
                    error_code: None,
                    upgrade_hint: None,
                }),
            )
                .into_response();
        }
    };

    let metadata = match file.metadata().await {
        Ok(m) => m,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let file_size = metadata.len();

    let mut start = 0;
    let mut end = file_size.saturating_sub(1);
    let mut is_partial = false;

    if let Some(range_value) = headers
        .get(axum::http::header::RANGE)
        .and_then(|h| h.to_str().ok())
    {
        if range_value.starts_with("bytes=") {
            let parts: Vec<&str> = range_value["bytes=".len()..].split('-').collect();
            if !parts.is_empty() {
                if let Ok(s) = parts[0].parse::<u64>() {
                    start = s;
                    is_partial = true;
                }
                if parts.len() > 1 && !parts[1].is_empty() {
                    if let Ok(e) = parts[1].parse::<u64>() {
                        end = e;
                    }
                }
            }
        }
    }

    if start >= file_size && file_size > 0 {
        let mut res_headers = axum::http::HeaderMap::new();
        res_headers.insert(
            axum::http::header::CONTENT_RANGE,
            format!("bytes */{file_size}").parse().unwrap(),
        );
        return (StatusCode::RANGE_NOT_SATISFIABLE, res_headers, "").into_response();
    }
    if end >= file_size {
        end = file_size.saturating_sub(1);
    }
    if start > end {
        return (StatusCode::RANGE_NOT_SATISFIABLE, "").into_response();
    }

    let chunk_size = (end - start + 1) as usize;
    if chunk_size > MAX_CHUNK_SIZE {
        let max_mb = MAX_CHUNK_SIZE / 1024 / 1024;
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "Chunk too large (max {} MB), please use smaller Range requests",
                max_mb
            ),
        )
            .into_response();
    }

    // 取得 range request 信號量許可證
    let _permit = match state.range_semaphore.acquire().await {
        Ok(permit) => permit,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Too many concurrent downloads, please retry",
            )
                .into_response()
        }
    };

    use std::io::SeekFrom;
    use tokio::io::AsyncSeekExt;
    if file.seek(SeekFrom::Start(start)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let mut buffer = vec![0; chunk_size];
    if tokio::io::AsyncReadExt::read_exact(&mut file, &mut buffer)
        .await
        .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    if let Ok(mut guard) = state.runtime.lock() {
        guard.http_uploaded_bytes += chunk_size as u64;
        guard.fallback_transfer_count += 1;
        guard.last_activity_unix_ms = unix_time_ms();
    }

    let file_name = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download.bin")
        .to_string();

    let mut res_headers = axum::http::HeaderMap::new();
    res_headers.insert(axum::http::header::ACCEPT_RANGES, "bytes".parse().unwrap());
    res_headers.insert(
        axum::http::header::CONTENT_TYPE,
        "application/octet-stream".parse().unwrap(),
    );

    if is_partial {
        res_headers.insert(
            axum::http::header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{file_size}").parse().unwrap(),
        );
        (StatusCode::PARTIAL_CONTENT, res_headers, buffer).into_response()
    } else {
        res_headers.insert(
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{file_name}\"")
                .parse()
                .unwrap(),
        );
        (StatusCode::OK, res_headers, buffer).into_response()
    }
}

async fn health_handler() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true }))
}

async fn client_stats_handler(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumState(state): AxumState<HttpState>,
    Json(report): Json<ClientStatsReport>,
) -> impl IntoResponse {
    if let Err(status) = enforce_rate_limit(&state) {
        return (
            status,
            Json(ErrorResponse {
                error: "Rate limit exceeded".to_string(),
                error_code: None,
                upgrade_hint: None,
            }),
        )
            .into_response();
    }

    let mut guard = match state.runtime.lock() {
        Ok(g) => g,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "State lock poisoned".to_string(),
                    error_code: None,
                    upgrade_hint: None,
                }),
            )
                .into_response();
        }
    };

    record_client_activity(&mut guard, addr);

    let session = match &guard.session {
        Some(session) => session,
        None => {
            return (
                StatusCode::GONE,
                Json(ErrorResponse {
                    error: "Share session is not active".to_string(),
                    error_code: Some("SESSION_INACTIVE".to_string()),
                    upgrade_hint: None,
                }),
            )
                .into_response();
        }
    };

    if !session
        .files
        .iter()
        .any(|file| file.file_id == report.file_id)
    {
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Shared file not found".to_string(),
                error_code: Some("FILE_NOT_FOUND".to_string()),
                upgrade_hint: None,
            }),
        )
            .into_response();
    }

    let now = Instant::now();
    guard.client_reports.insert(
        report.client_id.clone(),
        ClientReportSnapshot {
            p2p_uploaded_bytes: report.p2p_uploaded_bytes,
            active_peers: report.active_peers,
            is_seeding: report.is_seeding,
            last_seen: now,
        },
    );
    rebuild_reported_metrics(&mut guard, now);
    guard.last_activity_unix_ms = unix_time_ms();

    eprintln!(
        "client-stats accepted client={} file={} p2p_uploaded_bytes={} active_peers={} is_seeding={}",
        report.client_id,
        report.file_id,
        report.p2p_uploaded_bytes,
        report.active_peers,
        report.is_seeding,
    );

    Json(ClientStatsAck { accepted: true }).into_response()
}

async fn torrent_handler(
    AxumPath(file_id): AxumPath<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumState(state): AxumState<HttpState>,
) -> impl IntoResponse {
    if let Err(status) = enforce_rate_limit(&state) {
        return (
            status,
            Json(ErrorResponse {
                error: "Rate limit exceeded".to_string(),
                error_code: None,
                upgrade_hint: None,
            }),
        )
            .into_response();
    }

    let torrent_bytes = {
        let mut guard = match state.runtime.lock() {
            Ok(g) => g,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: "State lock poisoned".to_string(),
                        error_code: None,
                        upgrade_hint: None,
                    }),
                )
                    .into_response();
            }
        };

        record_client_activity(&mut guard, addr);

        match &guard.session {
            Some(session) => match session.files.iter().find(|file| file.file_id == file_id) {
                Some(file) => file.torrent_bytes.clone(),
                None => {
                    return (
                        StatusCode::NOT_FOUND,
                        Json(ErrorResponse {
                            error: "Shared torrent descriptor not found".to_string(),
                            error_code: None,
                            upgrade_hint: None,
                        }),
                    )
                        .into_response();
                }
            },
            None => {
                return (
                    StatusCode::GONE,
                    Json(ErrorResponse {
                        error: "Share session is not active".to_string(),
                        error_code: None,
                        upgrade_hint: None,
                    }),
                )
                    .into_response();
            }
        }
    };

    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/x-bittorrent".to_string(),
        )],
        torrent_bytes,
    )
        .into_response()
}

async fn validate_file_path(path: &Path) -> Result<(), String> {
    let meta = fs::metadata(path)
        .await
        .map_err(|_| "Selected file does not exist or is not readable".to_string())?;

    if !meta.is_file() {
        return Err("Selected path is not a file".to_string());
    }

    let mut file = fs::File::open(path)
        .await
        .map_err(|_| "Selected file does not have read permission".to_string())?;

    let mut sample = [0u8; 1];
    let _ = file
        .read(&mut sample)
        .await
        .map_err(|_| "Selected file cannot be read".to_string())?;

    Ok(())
}

async fn build_shared_files(
    file_paths: Vec<String>,
    trackers: &[String],
    base_url: &str,
    existing_paths: &HashSet<String>,
    next_index: usize,
    progress_callback: impl Fn(String, usize, u64, u64) -> Result<(), String> + Send + Clone,
) -> Result<Vec<SharedFile>, String> {
    let mut files = Vec::new();
    let mut seen = existing_paths.clone();

    for (offset, file_path) in file_paths.into_iter().enumerate() {
        let path = PathBuf::from(&file_path);
        validate_file_path(&path).await?;

        let normalized = path.to_string_lossy().to_string();
        if seen.contains(&normalized) {
            continue;
        }

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("shared-file")
            .to_string();
        let file_id = format!("file-{}", next_index + offset + 1);
        let web_seed_url = format!("{base_url}/api/file/{file_id}");

        let file_idx = offset + 1;
        let file_total_size = fs::metadata(&path)
            .await
            .map_err(|e| format!("Failed to stat file: {e}"))?
            .len();

        progress_callback(file_name.clone(), file_idx, file_total_size, 0)?;

        let metadata = build_seed_metadata(&path, trackers, &web_seed_url, {
            let cb = progress_callback.clone();
            let file_name = file_name.clone();
            move |chunk_bytes| {
                cb(
                    file_name.clone(),
                    file_idx,
                    file_total_size,
                    chunk_bytes as u64,
                )
            }
        })
        .await?;

        files.push(SharedFile {
            file_id,
            file_name,
            file_path: normalized.clone(),
            file_size: metadata.file_size,
            info_hash: metadata.info_hash,
            piece_size: metadata.piece_size,
            piece_count: metadata.piece_count,
            magnet_uri: metadata.magnet_uri,
            content_signature: metadata.content_signature,
            seed_reused: metadata.seed_reused,
            torrent_bytes: metadata.torrent_bytes,
        });
        seen.insert(normalized);
    }

    if files.is_empty() {
        return Err("No new files were added to the share session".to_string());
    }

    Ok(files)
}

fn refresh_session_summary(session: &mut ShareSession, revision: u64, last_updated_unix_ms: u128) {
    session.file_count = session.files.len();
    session.total_size = session.files.iter().map(|file| file.file_size).sum();
    session.revision = revision;
    session.last_updated_unix_ms = last_updated_unix_ms;
}

fn build_metadata_response(
    session: &ShareSession,
    fallback_http_enabled: bool,
) -> MetadataResponse {
    MetadataResponse {
        metadata_version: METADATA_VERSION,
        min_supported_metadata_version: MIN_SUPPORTED_METADATA_VERSION,
        max_supported_metadata_version: MAX_SUPPORTED_METADATA_VERSION,
        session_id: session.session_id.clone(),
        files: session.files.clone(),
        file_count: session.file_count,
        total_size: session.total_size,
        revision: session.revision,
        last_updated_unix_ms: session.last_updated_unix_ms,
        tracker_urls: session.tracker_urls.clone(),
        started_at_unix_ms: session.started_at_unix_ms,
        fallback_http_enabled,
    }
}

fn record_client_activity(runtime: &mut ShareRuntime, addr: SocketAddr) {
    let now = Instant::now();
    runtime.client_activity.push_back(ClientActivity {
        ip: addr.ip().to_string(),
        seen_at: now,
    });

    while let Some(oldest) = runtime.client_activity.front() {
        if now.duration_since(oldest.seen_at) > Duration::from_secs(CLIENT_ACTIVITY_WINDOW_SECS) {
            let _ = runtime.client_activity.pop_front();
        } else {
            break;
        }
    }

    runtime.last_activity_unix_ms = unix_time_ms();
}

fn prune_client_reports(reports: &mut HashMap<String, ClientReportSnapshot>, now: Instant) {
    reports.retain(|_, report| {
        now.duration_since(report.last_seen) <= Duration::from_secs(CLIENT_ACTIVITY_WINDOW_SECS)
    });
}

fn rebuild_reported_metrics(runtime: &mut ShareRuntime, now: Instant) {
    prune_client_reports(&mut runtime.client_reports, now);
    runtime.p2p_uploaded_bytes = runtime
        .client_reports
        .values()
        .map(|entry| entry.p2p_uploaded_bytes)
        .sum();
    runtime.active_p2p_peer_count = runtime
        .client_reports
        .values()
        .map(|entry| entry.active_peers)
        .sum();
    runtime.seeding_peer_count = runtime
        .client_reports
        .values()
        .filter(|entry| entry.is_seeding)
        .count();
}

fn build_metrics(runtime: &ShareRuntime) -> ShareMetrics {
    let now = Instant::now();
    let active_client_count = runtime
        .client_activity
        .iter()
        .filter(|activity| {
            now.duration_since(activity.seen_at) <= Duration::from_secs(CLIENT_ACTIVITY_WINDOW_SECS)
        })
        .map(|activity| activity.ip.clone())
        .collect::<HashSet<_>>()
        .len();

    ShareMetrics {
        active_client_count,
        http_uploaded_bytes: runtime.http_uploaded_bytes,
        p2p_uploaded_bytes: runtime.p2p_uploaded_bytes,
        active_p2p_peer_count: runtime.active_p2p_peer_count,
        fallback_transfer_count: runtime.fallback_transfer_count,
        seeding_peer_count: runtime.seeding_peer_count,
        metadata_revision: runtime.metadata_revision,
        last_activity_unix_ms: runtime.last_activity_unix_ms,
    }
}

fn build_insights(runtime: &ShareRuntime) -> ShareInsights {
    let is_sharing = runtime.session.is_some();
    let metrics = build_metrics(runtime);
    let active_downloads = metrics.active_client_count;
    let seeding_peers = metrics.seeding_peer_count;
    let reachability = if is_sharing {
        "LAN 可連線".to_string()
    } else {
        "尚未啟動".to_string()
    };
    let share_state = if runtime.processing_progress.is_some() {
        "正在處理檔案".to_string()
    } else if is_sharing {
        "分享中".to_string()
    } else {
        "未啟動".to_string()
    };

    let next_action_hint = if runtime.processing_progress.is_some() {
        "等待種子資料處理完成，系統會自動更新狀態".to_string()
    } else if is_sharing {
        "可複製分享 URL 給下載者，或追加新檔案".to_string()
    } else {
        "先加入檔案後按「啟動分享」".to_string()
    };

    ShareInsights {
        share_state,
        reachability,
        active_downloads,
        seeding_peers,
        recent_error: runtime.last_error.clone(),
        recent_activity_label: format_recent_activity(runtime.last_activity_unix_ms),
        next_action_hint,
    }
}

fn format_recent_activity(unix_ms: u128) -> String {
    if unix_ms == 0 {
        return "暫無活動".to_string();
    }

    let now = unix_time_ms();
    let delta_ms = now.saturating_sub(unix_ms);
    if delta_ms < 1_000 {
        return "剛剛".to_string();
    }
    if delta_ms < 60_000 {
        return format!("{} 秒前", delta_ms / 1_000);
    }
    if delta_ms < 3_600_000 {
        return format!("{} 分鐘前", delta_ms / 60_000);
    }
    format!("{} 小時前", delta_ms / 3_600_000)
}

#[derive(Debug)]
struct SeedMetadata {
    file_size: u64,
    info_hash: String,
    piece_size: usize,
    piece_count: usize,
    magnet_uri: String,
    content_signature: String,
    seed_reused: bool,
    torrent_bytes: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeedDescriptor {
    schema_version: u16,
    file_size: u64,
    modified_unix_ms: u128,
    piece_size: usize,
    piece_count: usize,
    info_hash: String,
    pieces_base64: String,
}

async fn build_seed_metadata(
    path: &Path,
    trackers: &[String],
    web_seed_url: &str,
    progress_callback: impl Fn(u64) -> Result<(), String> + Send,
) -> Result<SeedMetadata, String> {
    let file_meta = fs::metadata(path)
        .await
        .map_err(|e| format!("Failed to stat file: {e}"))?;
    let file_size = file_meta.len();
    let modified_unix_ms = file_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let piece_size = DEFAULT_PIECE_SIZE;
    let expected_piece_count = if file_size == 0 {
        1
    } else {
        (file_size as usize).div_ceil(piece_size)
    };

    let display_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("shared-file");

    if let Some(reused) = load_seed_descriptor(path).await {
        if reused.schema_version == METADATA_VERSION
            && reused.file_size == file_size
            && reused.modified_unix_ms == modified_unix_ms
            && reused.piece_size == piece_size
            && reused.piece_count == expected_piece_count
            && !reused.info_hash.is_empty()
            && !reused.pieces_base64.is_empty()
        {
            progress_callback(file_size)?;

            let piece_hashes = base64::engine::general_purpose::STANDARD
                .decode(reused.pieces_base64.as_bytes())
                .map_err(|e| format!("Failed to decode seed descriptor pieces: {e}"))?;
            let (torrent_bytes, info_hash) = build_torrent_bytes(
                display_name,
                file_size,
                piece_size,
                &piece_hashes,
                trackers,
                web_seed_url,
            );
            let magnet_uri =
                build_magnet_uri(&info_hash, display_name, file_size, trackers, web_seed_url);

            return Ok(SeedMetadata {
                file_size,
                info_hash,
                piece_size,
                piece_count: reused.piece_count,
                magnet_uri,
                content_signature: format!(
                    "{}:{}:{}",
                    reused.info_hash, reused.file_size, reused.piece_count
                ),
                seed_reused: true,
                torrent_bytes,
            });
        }
    }

    let piece_count = expected_piece_count;

    let mut file = fs::File::open(path)
        .await
        .map_err(|e| format!("Failed to open file: {e}"))?;

    let mut buffer = vec![0; piece_size.max(1)];
    let mut bytes_processed = 0u64;
    let mut piece_hashes = Vec::with_capacity(piece_count * 20);

    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|e| format!("Failed to read file: {e}"))?;
        if n == 0 {
            break;
        }

        let mut piece_hasher = Sha1::new();
        piece_hasher.update(&buffer[..n]);
        piece_hashes.extend_from_slice(&piece_hasher.finalize());
        bytes_processed += n as u64;
        progress_callback(bytes_processed)?;
    }

    let (torrent_bytes, info_hash) = build_torrent_bytes(
        display_name,
        file_size,
        piece_size,
        &piece_hashes,
        trackers,
        web_seed_url,
    );
    let magnet_uri = build_magnet_uri(&info_hash, display_name, file_size, trackers, web_seed_url);

    let descriptor = SeedDescriptor {
        schema_version: METADATA_VERSION,
        file_size,
        modified_unix_ms,
        piece_size,
        piece_count,
        info_hash: info_hash.clone(),
        pieces_base64: base64::engine::general_purpose::STANDARD.encode(piece_hashes),
    };
    save_seed_descriptor(path, &descriptor).await;

    let content_signature = format!("{}:{}:{}", info_hash, file_size, piece_count);

    Ok(SeedMetadata {
        file_size,
        info_hash,
        piece_size,
        piece_count,
        magnet_uri,
        content_signature,
        seed_reused: false,
        torrent_bytes,
    })
}

fn seed_descriptor_path(path: &Path) -> Option<PathBuf> {
    let file_name = path.file_name()?.to_str()?;
    Some(path.with_file_name(format!("{file_name}.mesh.seed.json")))
}

async fn load_seed_descriptor(path: &Path) -> Option<SeedDescriptor> {
    let descriptor_path = seed_descriptor_path(path)?;
    let raw = fs::read_to_string(descriptor_path).await.ok()?;
    serde_json::from_str::<SeedDescriptor>(&raw).ok()
}

async fn save_seed_descriptor(path: &Path, descriptor: &SeedDescriptor) {
    if let Some(descriptor_path) = seed_descriptor_path(path) {
        if let Ok(raw) = serde_json::to_string_pretty(descriptor) {
            let _ = fs::write(descriptor_path, raw).await;
        }
    }
}

fn build_torrent_bytes(
    display_name: &str,
    file_size: u64,
    piece_size: usize,
    piece_hashes: &[u8],
    trackers: &[String],
    web_seed_url: &str,
) -> (Vec<u8>, String) {
    let info_bytes = bencode_dict(vec![
        ("length", bencode_int(file_size as i64)),
        ("name", bencode_bytes(display_name.as_bytes())),
        ("piece length", bencode_int(piece_size as i64)),
        ("pieces", bencode_bytes(piece_hashes)),
    ]);
    let info_hash = hex::encode(Sha1::digest(&info_bytes));

    let mut root_entries = Vec::new();
    if let Some(primary_tracker) = trackers.first() {
        root_entries.push(("announce", bencode_bytes(primary_tracker.as_bytes())));
    }
    if !trackers.is_empty() {
        let announce_list = trackers
            .iter()
            .map(|tracker| bencode_list(vec![bencode_bytes(tracker.as_bytes())]))
            .collect::<Vec<_>>();
        root_entries.push(("announce-list", bencode_list(announce_list)));
    }
    root_entries.push(("created by", bencode_bytes(b"mesh-p2p")));
    root_entries.push(("creation date", bencode_int((unix_time_ms() / 1000) as i64)));
    root_entries.push(("info", info_bytes));
    root_entries.push((
        "url-list",
        bencode_list(vec![bencode_bytes(web_seed_url.as_bytes())]),
    ));

    (bencode_dict(root_entries), info_hash)
}

fn bencode_int(value: i64) -> Vec<u8> {
    format!("i{value}e").into_bytes()
}

fn bencode_bytes(value: &[u8]) -> Vec<u8> {
    let mut encoded = format!("{}:", value.len()).into_bytes();
    encoded.extend_from_slice(value);
    encoded
}

fn bencode_list(items: Vec<Vec<u8>>) -> Vec<u8> {
    let mut encoded = vec![b'l'];
    for item in items {
        encoded.extend_from_slice(&item);
    }
    encoded.push(b'e');
    encoded
}

fn bencode_dict(mut entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    entries.sort_by(|a, b| a.0.cmp(b.0));

    let mut encoded = vec![b'd'];
    for (key, value) in entries {
        encoded.extend_from_slice(format!("{}:{key}", key.len()).as_bytes());
        encoded.extend_from_slice(&value);
    }
    encoded.push(b'e');
    encoded
}

fn load_trackers_from_env() -> Vec<String> {
    let raw = std::env::var("MESH_P2P_TRACKERS").unwrap_or_else(|_| {
        "wss://tracker.openwebtorrent.com,wss://tracker.btorrent.xyz,wss://tracker.webtorrent.dev"
            .to_string()
    });

    parse_trackers(&raw)
}

fn parse_trackers(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| {
            s.starts_with("wss://")
                || s.starts_with("ws://")
                || s.starts_with("https://")
                || s.starts_with("http://")
                || s.starts_with("udp://")
        })
        .map(ToString::to_string)
        .collect()
}

fn build_magnet_uri(
    info_hash: &str,
    file_name: &str,
    file_size: u64,
    trackers: &[String],
    web_seed_url: &str,
) -> String {
    let mut uri = format!(
        "magnet:?xt=urn:btih:{}&dn={}&xl={}",
        info_hash,
        urlencoding::encode(file_name),
        file_size
    );

    for tr in trackers {
        uri.push_str("&tr=");
        uri.push_str(&urlencoding::encode(tr));
    }

    uri.push_str("&ws=");
    uri.push_str(&urlencoding::encode(web_seed_url));

    uri
}

fn load_fallback_flag() -> bool {
    std::env::var("MESH_P2P_FORCE_HTTP")
        .map(|v| {
            let lower = v.to_ascii_lowercase();
            lower == "1" || lower == "true" || lower == "yes"
        })
        .unwrap_or(false)
}

fn current_app_version() -> &'static str {
    APP_VERSION
        .get_or_init(|| {
            serde_json::from_str::<serde_json::Value>(include_str!("../tauri.conf.json"))
                .ok()
                .and_then(|value| {
                    value
                        .get("version")
                        .and_then(|version| version.as_str())
                        .map(ToString::to_string)
                })
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
        })
        .as_str()
}

fn detect_host_ip() -> String {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("8.8.8.8:80")?;
            socket.local_addr()
        })
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

fn generate_session_id() -> String {
    format!("sess-{}", unix_time_ms())
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis()
}

impl Drop for ShareRuntime {
    fn drop(&mut self) {
        if let Some(server) = self.server.as_mut() {
            if let Some(tx) = server.shutdown_tx.take() {
                let _ = tx.send(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_seed_metadata, client_stats_handler, file_handler, metadata_handler, parse_trackers,
        rebuild_reported_metrics, seed_descriptor_path, torrent_handler, ClientReportSnapshot,
        ClientStatsReport, HttpState, MetadataQuery, ShareRuntime, MAX_CONCURRENT_RANGES,
    };
    use axum::{
        extract::ConnectInfo, extract::Query, extract::State as AxumState, response::IntoResponse,
        Json,
    };
    use std::collections::{HashMap, VecDeque};
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tokio::sync::Semaphore;

    fn test_http_state() -> HttpState {
        HttpState {
            runtime: Arc::new(Mutex::new(ShareRuntime {
                server: None,
                session: None,
                tracker_urls: vec![],
                fallback_http_enabled: false,
                http_uploaded_bytes: 0,
                p2p_uploaded_bytes: 0,
                active_p2p_peer_count: 0,
                fallback_transfer_count: 0,
                seeding_peer_count: 0,
                metadata_revision: 0,
                last_activity_unix_ms: 0,
                client_activity: VecDeque::new(),
                client_reports: HashMap::new(),
                last_error: None,
                processing_progress: None,
                processing_cancel_requested: false,
            })),
            limiter: Arc::new(Mutex::new(VecDeque::new())),
            range_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_RANGES)),
        }
    }

    #[tokio::test]
    async fn build_seed_metadata_for_valid_file() {
        let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
        tmp.write_all(b"mesh-p2p-test").expect("write temp");

        let trackers = vec!["wss://tracker.example.com".to_string()];
        let metadata = build_seed_metadata(
            tmp.path(),
            &trackers,
            "http://127.0.0.1:3000/api/file/file-1",
            |_| Ok(()),
        )
        .await
        .expect("metadata should be generated");

        assert!(metadata.file_size > 0);
        assert!(!metadata.info_hash.is_empty());
        assert!(metadata.magnet_uri.contains("magnet:?xt=urn:btih:"));
        assert!(metadata.magnet_uri.contains("tracker.example.com"));
        assert!(!metadata.content_signature.is_empty());
    }

    #[tokio::test]
    async fn build_seed_metadata_for_invalid_file_fails() {
        let trackers = vec!["wss://tracker.example.com".to_string()];
        let result = build_seed_metadata(
            std::path::Path::new("/no/such/file"),
            &trackers,
            "http://127.0.0.1:3000/api/file/file-1",
            |_| Ok(()),
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn build_seed_metadata_reuses_descriptor_when_file_unchanged() {
        let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
        tmp.write_all(b"mesh-p2p-test").expect("write temp");

        let trackers = vec!["wss://tracker.example.com".to_string()];
        let first = build_seed_metadata(
            tmp.path(),
            &trackers,
            "http://127.0.0.1:3000/api/file/file-1",
            |_| Ok(()),
        )
        .await
        .expect("first metadata");
        assert!(!first.seed_reused);

        let second = build_seed_metadata(
            tmp.path(),
            &trackers,
            "http://127.0.0.1:3000/api/file/file-1",
            |_| Ok(()),
        )
        .await
        .expect("second metadata");

        assert!(second.seed_reused);
        assert_eq!(first.info_hash, second.info_hash);

        let descriptor_path = seed_descriptor_path(tmp.path()).expect("descriptor path");
        assert!(descriptor_path.exists());
    }

    #[test]
    fn parse_trackers_filters_invalid_values() {
        let trackers = parse_trackers("wss://ok,ftp://bad,udp://ok2,just-text,https://ok3");

        assert_eq!(trackers.len(), 3);
        assert!(trackers.iter().any(|s| s == "wss://ok"));
        assert!(trackers.iter().any(|s| s == "udp://ok2"));
        assert!(trackers.iter().any(|s| s == "https://ok3"));
    }

    #[test]
    fn rebuild_reported_metrics_aggregates_p2p_and_peer_counts() {
        let now = Instant::now();
        let mut runtime = ShareRuntime {
            server: None,
            session: None,
            tracker_urls: vec![],
            fallback_http_enabled: false,
            http_uploaded_bytes: 0,
            p2p_uploaded_bytes: 0,
            active_p2p_peer_count: 0,
            fallback_transfer_count: 0,
            seeding_peer_count: 0,
            metadata_revision: 0,
            last_activity_unix_ms: 0,
            client_activity: VecDeque::new(),
            client_reports: HashMap::from([
                (
                    "a".to_string(),
                    ClientReportSnapshot {
                        p2p_uploaded_bytes: 128,
                        active_peers: 2,
                        is_seeding: true,
                        last_seen: now,
                    },
                ),
                (
                    "b".to_string(),
                    ClientReportSnapshot {
                        p2p_uploaded_bytes: 256,
                        active_peers: 3,
                        is_seeding: false,
                        last_seen: now - Duration::from_secs(1),
                    },
                ),
            ]),
            last_error: None,
            processing_progress: None,
            processing_cancel_requested: false,
        };

        rebuild_reported_metrics(&mut runtime, now);

        assert_eq!(runtime.p2p_uploaded_bytes, 384);
        assert_eq!(runtime.active_p2p_peer_count, 5);
        assert_eq!(runtime.seeding_peer_count, 1);
    }

    #[tokio::test]
    async fn inactive_session_routes_return_gone() {
        let state = test_http_state();
        let addr = "127.0.0.1:45678".parse().expect("socket addr");

        let metadata_response = metadata_handler(
            Query(MetadataQuery {
                metadata_version: Some(1),
            }),
            ConnectInfo(addr),
            AxumState(state.clone()),
        )
        .await
        .into_response();
        assert_eq!(metadata_response.status(), axum::http::StatusCode::GONE);

        let file_response = file_handler(
            axum::http::HeaderMap::new(),
            axum::extract::Path("file-1".to_string()),
            ConnectInfo(addr),
            AxumState(state.clone()),
        )
        .await
        .into_response();
        assert_eq!(file_response.status(), axum::http::StatusCode::GONE);

        let torrent_response = torrent_handler(
            axum::extract::Path("file-1".to_string()),
            ConnectInfo(addr),
            AxumState(state.clone()),
        )
        .await
        .into_response();
        assert_eq!(torrent_response.status(), axum::http::StatusCode::GONE);

        let client_stats_response = client_stats_handler(
            ConnectInfo(addr),
            AxumState(state),
            Json(ClientStatsReport {
                client_id: "client-a".to_string(),
                file_id: "file-1".to_string(),
                p2p_uploaded_bytes: 64,
                active_peers: 1,
                is_seeding: true,
            }),
        )
        .await
        .into_response();
        assert_eq!(client_stats_response.status(), axum::http::StatusCode::GONE);
    }
}
