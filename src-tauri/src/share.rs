use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::{SocketAddr, UdpSocket},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    extract::ConnectInfo,
    extract::Path as AxumPath,
    extract::Query,
    extract::State as AxumState,
    extract::WebSocketUpgrade,
    http::{Request, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine as _;
use futures::{SinkExt, StreamExt};
use rcgen::generate_simple_self_signed;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tauri::{State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tokio::{
    fs,
    io::AsyncReadExt,
    sync::{mpsc, oneshot, Semaphore},
};

macro_rules! dev_log {
    ($($arg:tt)*) => {
        if cfg!(debug_assertions) {
            eprintln!($($arg)*);
        }
    };
}

/// 依檔案大小選擇適當的 piece size，大檔案使用較大 piece 以減少 piece 數量。
/// 目標：piece 數量控制在 2,000–10,000 之間。
fn piece_size_for_file(file_size: u64) -> usize {
    if file_size <= 256 * 1024 * 1024 {
        // ≤ 256 MB → 256 KB pieces
        256 * 1024
    } else if file_size <= 1024 * 1024 * 1024 {
        // ≤ 1 GB → 512 KB pieces
        512 * 1024
    } else if file_size <= 4 * 1024 * 1024 * 1024 {
        // ≤ 4 GB → 1 MB pieces
        1024 * 1024
    } else {
        // > 4 GB → 2 MB pieces
        2 * 1024 * 1024
    }
}
const RATE_LIMIT_WINDOW_MS: u64 = 1000;
const RATE_LIMIT_MAX: usize = 5000;
const CLIENT_ACTIVITY_WINDOW_SECS: u64 = 300;
const METADATA_VERSION: u16 = 1;
const MIN_SUPPORTED_METADATA_VERSION: u16 = 1;
const MAX_SUPPORTED_METADATA_VERSION: u16 = METADATA_VERSION;
const APP_VERSION_PLACEHOLDER: &str = "__APP_VERSION__";
const MAX_CHUNK_SIZE: usize = 50 * 1024 * 1024; // 改為 50 MB (原 200 MB)
const MAX_CONCURRENT_RANGES: usize = 30; // 限制並行 range requests，促進 P2P 分擔
const TRACKER_MAX_CONNECTIONS: usize = 200;
const TRACKER_ANNOUNCE_INTERVAL: u64 = 120;
const TRACKER_PEER_TIMEOUT_SECS: u64 = 300;
const NUM_PIECE_SLICES: u64 = 16; // piece diversity 分組數

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

// ─── Built-in WebSocket Tracker ───

type WsSender = mpsc::UnboundedSender<axum::extract::ws::Message>;

#[derive(Debug, Clone)]
struct TrackerPeer {
    peer_id: String,
    sender: WsSender,
    is_complete: bool,
    info_hashes: HashSet<String>,
    last_seen: Instant,
}

#[derive(Debug, Default)]
struct TrackerSwarm {
    peers: HashMap<String, WsSender>, // peer_id → sender
    complete: HashSet<String>,        // peer_ids that are seeders
}

struct TrackerState {
    swarms: HashMap<String, TrackerSwarm>,   // info_hash → swarm
    peer_meta: HashMap<String, TrackerPeer>, // peer_id → peer metadata
    connection_count: Arc<AtomicU64>,
}

impl std::fmt::Debug for TrackerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackerState")
            .field("swarm_count", &self.swarms.len())
            .field(
                "connection_count",
                &self.connection_count.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl Default for TrackerState {
    fn default() -> Self {
        Self {
            swarms: HashMap::new(),
            peer_meta: HashMap::new(),
            connection_count: Arc::new(AtomicU64::new(0)),
        }
    }
}

// ─── Application State ───

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
    tracker: TrackerState,
    piece_offset_counter: AtomicU64,
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
    tracker_conn_count: Arc<AtomicU64>,
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
    piece_priority_offset: u64,
    builtin_tracker_url: String,
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
                tracker: TrackerState::default(),
                piece_offset_counter: AtomicU64::new(0),
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

        let base = guard
            .server
            .as_ref()
            .map(|server| server.base_url.clone())
            .ok_or_else(|| "Share server is not running".to_string())?;

        (
            trackers_with_builtin(&base, &guard.tracker_urls),
            base,
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
        (
            trackers_with_builtin(&server_url, &guard.tracker_urls),
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
    // Clear tracker state: close all WS connections by dropping senders
    guard.tracker.swarms.clear();
    guard.tracker.peer_meta.clear();
    guard.piece_offset_counter = AtomicU64::new(0);
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

    dev_log!("[mesh-p2p][share] Starting local HTTPS server...");

    let bind_addr = resolve_bind_addr();

    let advertised_host = resolve_advertised_host();
    dev_log!(
        "[mesh-p2p][share] Binding on {}, advertised host {}",
        bind_addr,
        advertised_host
    );

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let tracker_conn_count = {
        let guard = runtime
            .lock()
            .map_err(|_| "State lock poisoned".to_string())?;
        guard.tracker.connection_count.clone()
    };

    let http_state = HttpState {
        runtime: runtime.clone(),
        limiter: Arc::new(Mutex::new(VecDeque::new())),
        range_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_RANGES)),
        tracker_conn_count,
    };

    let router = Router::new()
        .route("/", get(download_page_handler))
        .route("/webtorrent.min.js", get(webtorrent_js_handler))
        .route("/mesh.png", get(mesh_png_handler))
        .route("/announce", get(tracker_ws_handler))
        .route("/api/metadata", get(metadata_handler))
        .route("/api/client-stats", post(client_stats_handler))
        .route("/api/torrent/{file_id}", get(torrent_handler))
        .route("/api/file/{file_id}", get(file_handler))
        .route("/api/health", get(health_handler))
        .with_state(http_state)
        .layer(axum::middleware::from_fn(request_log_middleware))
        .layer(tower_http::cors::CorsLayer::permissive());

    let handle = axum_server::Handle::new();
    let handle_for_wait = handle.clone();
    let advertised_host_for_tls = advertised_host.clone();

    tauri::async_runtime::spawn(async move {
        let (cert, key) = match generate_runtime_tls_material(&advertised_host_for_tls) {
            Ok((cert, key)) => {
                dev_log!(
                    "[mesh-p2p][share] Generated runtime TLS certificate (SAN includes localhost)"
                );
                (cert, key)
            }
            Err(err) => {
                dev_log!(
                    "[mesh-p2p][share] Runtime TLS cert generation failed: {err}; fallback to embedded cert"
                );
                (
                    include_bytes!("../certs/cert.pem").to_vec(),
                    include_bytes!("../certs/key.pem").to_vec(),
                )
            }
        };

        let base_tls_config = axum_server::tls_rustls::RustlsConfig::from_pem(cert, key)
            .await
            .expect("Failed to load TLS certificate/key");

        let mut rustls_server_config = (*base_tls_config.get_inner()).clone();
        rustls_server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
        rustls_server_config.send_tls13_tickets = 0;
        let tls_config =
            axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(rustls_server_config));

        dev_log!("[mesh-p2p][share] TLS config loaded, starting Axum server loop...");

        let handle_clone = handle.clone();
        tokio::spawn(async move {
            let _ = shutdown_rx.await;
            handle_clone.graceful_shutdown(Some(std::time::Duration::from_secs(3)));
        });

        let server_result = axum_server::bind_rustls(bind_addr, tls_config)
            .http1_only()
            .handle(handle)
            .serve(router.into_make_service_with_connect_info::<SocketAddr>())
            .await;

        if let Err(err) = server_result {
            dev_log!("[mesh-p2p][share] Server failed: {err}");
        } else {
            dev_log!("[mesh-p2p][share] Server loop exited cleanly");
        }
    });

    let listening_addr = tokio::time::timeout(Duration::from_secs(4), handle_for_wait.listening())
        .await
        .map_err(|_| "server did not report listening address in time".to_string())?
        .ok_or_else(|| "server failed to bind listening socket".to_string())?;

    let base_url = format!(
        "https://{}:{}",
        format_host_for_url(&advertised_host),
        listening_addr.port()
    );
    dev_log!(
        "[mesh-p2p][share] HTTPS server ready at {} (health: {}/api/health)",
        base_url,
        base_url
    );

    if listening_addr.ip().is_unspecified() {
        dev_log!(
            "[mesh-p2p][share] Listener is on wildcard {}",
            listening_addr
        );
    }

    if listening_addr.port() == 0 {
        return Err(format!(
            "Failed to start local HTTPS server on {base_url}. Invalid port detected. Try setting MESH_P2P_HOST to a reachable LAN IP."
        ));
    }

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

async fn request_log_middleware(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let remote = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let started = Instant::now();

    dev_log!(
        "[mesh-p2p][http] incoming {} {}{} from {}",
        method,
        path,
        query,
        remote
    );

    let response = next.run(req).await;
    let status = response.status().as_u16();
    let elapsed_ms = started.elapsed().as_millis();

    dev_log!(
        "[mesh-p2p][http] {} {}{} from {} -> {} ({} ms)",
        method,
        path,
        query,
        remote,
        status,
        elapsed_ms
    );

    response
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

            // FileSystemChunkStore: 透過 File System Access API 直接讀寫檔案系統，
            // 下載期間使用 .mesh-download 暫存檔名，完成後才寫入正式檔名。
            // 中途停止或瀏覽器關閉不會產生損壞的正式檔。
            class FileSystemChunkStore {
                constructor(chunkLength, opts) {
                    this.chunkLength = chunkLength;
                    this.length = opts.length || 0;
                    this.dirHandle = opts.dirHandle;
                    this.fileName = opts.fileName;
                    this._tempFileName = this.fileName + '.mesh-download';
                    this._tempFileHandle = null;
                    this._finalFileHandle = null;
                    this._writable = null;
                    this._writableReady = null;
                    this._writeQueue = Promise.resolve();
                    this._closed = false;
                    this._committed = false;
                }

                _ensureWritable() {
                    if (!this._writableReady) {
                        this._writableReady = (async () => {
                            if (!this._tempFileHandle) {
                                this._tempFileHandle = await this.dirHandle.getFileHandle(this._tempFileName, { create: true });
                            }
                            const w = await this._tempFileHandle.createWritable({ keepExistingData: true });
                            this._writable = w;
                            return w;
                        })();
                    }
                    return this._writableReady;
                }

                put(index, buf, cb) {
                    this._writeQueue = this._writeQueue.then(async () => {
                        if (this._closed) { cb(new Error('Store is closed')); return; }
                        try {
                            const writable = await this._ensureWritable();
                            const offset = index * this.chunkLength;
                            await writable.seek(offset);
                            await writable.write(buf);
                            cb(null);
                        } catch (err) {
                            this._writable = null;
                            this._writableReady = null;
                            cb(err);
                        }
                    });
                }

                get(index, opts, cb) {
                    if (typeof opts === 'function') { cb = opts; opts = {}; }
                    const offset = (opts && opts.offset) || 0;
                    const len = opts && opts.length != null ? opts.length : null;
                    const byteOffset = index * this.chunkLength + offset;
                    const byteLength = len != null ? len : this.chunkLength;
                    const handle = this._finalFileHandle || this._tempFileHandle;
                    if (!handle) { cb(new Error('File not ready')); return; }
                    handle.getFile().then((file) => {
                        const slice = file.slice(byteOffset, byteOffset + byteLength);
                        return slice.arrayBuffer();
                    }).then((ab) => {
                        cb(null, new Uint8Array(ab));
                    }).catch((err) => {
                        cb(err);
                    });
                }

                commit() {
                    return new Promise((resolve, reject) => {
                        this._writeQueue = this._writeQueue.then(async () => {
                            try {
                                // 1. 關閉 writable → .crswap 原子寫入暫存檔
                                if (this._writable) {
                                    await this._writable.close();
                                    this._writable = null;
                                    this._writableReady = null;
                                }
                                // 2. 暫存檔串流寫入正式檔名
                                const tempFile = await this._tempFileHandle.getFile();
                                this._finalFileHandle = await this.dirHandle.getFileHandle(this.fileName, { create: true });
                                const finalWritable = await this._finalFileHandle.createWritable();
                                await tempFile.stream().pipeTo(finalWritable);
                                // 3. 刪除暫存檔
                                await this.dirHandle.removeEntry(this._tempFileName).catch(() => {});
                                this._committed = true;
                                resolve();
                            } catch (err) {
                                reject(err);
                            }
                        });
                    });
                }

                _abort() {
                    return this._writeQueue = this._writeQueue.then(async () => {
                        try {
                            if (this._writable) {
                                await this._writable.abort().catch(() => {});
                                this._writable = null;
                                this._writableReady = null;
                            }
                            if (this.dirHandle && this._tempFileName) {
                                await this.dirHandle.removeEntry(this._tempFileName).catch(() => {});
                            }
                        } catch (_) {}
                    });
                }

                close(cb) {
                    this._closed = true;
                    if (this._committed) {
                        if (cb) cb(null);
                    } else {
                        this._abort().then(() => { if (cb) cb(null); }).catch((err) => { if (cb) cb(err); });
                    }
                }

                destroy(cb) {
                    this._closed = true;
                    this._abort().then(() => { if (cb) cb(null); }).catch((err) => { if (cb) cb(err); });
                }
            }

            createApp({
                setup() {
                    const statusText = ref('載入中...');
                    const warningText = ref('');
                    const metadata = ref(null);
                    const metadataError = ref('');
                    const lastRevision = ref(-1);
                    const timerId = ref(null);
                    const downloads = ref({});
                    const browserSupported = ref(true);
                    const directoryReady = ref(false);
                    const dirHandle = ref(null);
                    const dirName = ref('');
                    let torrentClient = null;
                    const torrentSessions = {};

                    const SAVE_DIR_DB_NAME = 'mesh-p2p-app';
                    const SAVE_DIR_DB_STORE = 'kv';
                    const SAVE_DIR_KEY = 'downloadDirHandle';

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
                                lastBytesReceived: 0,
                                smoothedSpeedBps: 0,
                            };
                        }
                        return downloads.value[fileId];
                    }

                    function openSaveDirDb() {
                        return new Promise((resolve, reject) => {
                            const req = indexedDB.open(SAVE_DIR_DB_NAME, 1);
                            req.onupgradeneeded = (e) => {
                                const db = e.target.result;
                                if (!db.objectStoreNames.contains(SAVE_DIR_DB_STORE)) {
                                    db.createObjectStore(SAVE_DIR_DB_STORE);
                                }
                            };
                            req.onsuccess = (e) => resolve(e.target.result);
                            req.onerror = (e) => reject(e.target.error);
                        });
                    }

                    async function loadSavedDirectoryHandle() {
                        try {
                            const db = await openSaveDirDb();
                            return await new Promise((resolve, reject) => {
                                const tx = db.transaction(SAVE_DIR_DB_STORE, 'readonly');
                                const req = tx.objectStore(SAVE_DIR_DB_STORE).get(SAVE_DIR_KEY);
                                req.onsuccess = () => resolve(req.result || null);
                                req.onerror = (e) => reject(e.target.error);
                            });
                        } catch (_) {
                            return null;
                        }
                    }

                    async function persistDirectoryHandle(handle) {
                        const db = await openSaveDirDb();
                        await new Promise((resolve, reject) => {
                            const tx = db.transaction(SAVE_DIR_DB_STORE, 'readwrite');
                            tx.objectStore(SAVE_DIR_DB_STORE).put(handle, SAVE_DIR_KEY);
                            tx.oncomplete = resolve;
                            tx.onerror = (e) => reject(e.target.error);
                        });
                    }

                    async function activateDirectory(handle) {
                        const opts = { mode: 'readwrite' };
                        const queried = await handle.queryPermission(opts);
                        if (queried !== 'granted') {
                            const requested = await handle.requestPermission(opts);
                            if (requested !== 'granted') return false;
                        }
                        dirHandle.value = handle;
                        dirName.value = handle.name || '已選擇';
                        directoryReady.value = true;
                        return true;
                    }

                    async function pickDirectory() {
                        try {
                            const picked = await window.showDirectoryPicker({ mode: 'readwrite' });
                            const ok = await activateDirectory(picked);
                            if (!ok) {
                                warningText.value = '未取得目錄寫入權限。';
                                return;
                            }
                            try { await persistDirectoryHandle(picked); } catch (_) {}
                        } catch (err) {
                            if (err.name !== 'AbortError') {
                                warningText.value = String(err);
                            }
                        }
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

                    function updateStateFromTorrent(file, torrent, shouldReportStats) {
                        const state = ensureDownloadState(file.fileId);
                        const currentBytes = torrent.downloaded || 0;
                        const currentTick = nowMs();
                        const elapsedMs = Math.max(1, currentTick - (state.lastTickAt || currentTick));
                        const deltaBytes = Math.max(0, currentBytes - (state.lastBytesReceived || 0));
                        const measuredSpeed = Math.round((deltaBytes * 1000) / elapsedMs);
                        const reportedSpeed = Math.round(torrent.downloadSpeed || 0);

                        state.totalBytes = torrent.length || file.fileSize || 0;
                        state.bytesReceived = currentBytes;
                        state.progressPercent = state.totalBytes > 0
                            ? Math.min(100, Math.round((state.bytesReceived / state.totalBytes) * 100))
                            : 0;
                        const blended = measuredSpeed > 0
                            ? Math.round((measuredSpeed * 0.7) + (reportedSpeed * 0.3))
                            : reportedSpeed;
                        state.smoothedSpeedBps = state.smoothedSpeedBps > 0
                            ? Math.round((state.smoothedSpeedBps * 0.8) + (blended * 0.2))
                            : blended;
                        state.speedBps = Math.max(0, state.smoothedSpeedBps);
                        state.etaSeconds = state.speedBps > 0 && state.totalBytes > state.bytesReceived
                            ? Math.ceil((state.totalBytes - state.bytesReceived) / state.speedBps)
                            : 0;
                        state.sourceMix = torrent.numPeers > 0 ? 'P2P + HTTP web seed' : 'HTTP web seed fallback';
                        state.lastTickAt = currentTick;
                        state.lastBytesReceived = currentBytes;
                        if (shouldReportStats) {
                            void reportClientStats(file, torrent, state.phase === 'seeding' || torrent.progress === 1);
                        }
                    }

                    async function downloadFile(file) {
                        const state = ensureDownloadState(file.fileId);
                        if (state.phase === 'downloading' || state.phase === 'seeding') {
                            return;
                        }

                        if (!directoryReady.value || !dirHandle.value) {
                            warningText.value = '請先選擇下載資料夾。';
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
                        state.lastBytesReceived = 0;
                        state.smoothedSpeedBps = 0;

                        try {
                            const client = ensureClient();
                            const torrentBytes = await fetchTorrentBytes(file.fileId);

                            const currentDirHandle = dirHandle.value;
                            let chunkStore = null;
                            const torrent = client.add(torrentBytes, {
                                store: function(chunkLength, storeOpts) {
                                    chunkStore = new FileSystemChunkStore(chunkLength, {
                                        ...storeOpts,
                                        dirHandle: currentDirHandle,
                                        fileName: file.fileName,
                                    });
                                    return chunkStore;
                                },
                                maxWebConns: 2, // 限制每個 torrent 的 HTTP web seed 連線數，促進 P2P 分擔
                                destroyStoreOnDestroy: false
                            });
                            
                            const MAX_CONCURRENT_PEERS = 15;
                            if (typeof torrent.setMaxConns === 'function') {
                                torrent.setMaxConns(MAX_CONCURRENT_PEERS);
                            }

                            // Piece diversity: prioritize a different slice per client
                            const NUM_SLICES = 16;
                            const piecePriorityOffset = (metadata.value && metadata.value.piecePriorityOffset) || 0;
                            torrent.on('ready', () => {
                                const numPieces = torrent.pieces.length;
                                if (numPieces > NUM_SLICES) {
                                    const sliceSize = Math.floor(numPieces / NUM_SLICES);
                                    const startPiece = (piecePriorityOffset * sliceSize) % numPieces;
                                    const endPiece = Math.min(startPiece + sliceSize - 1, numPieces - 1);
                                    torrent.select(startPiece, endPiece, 5);
                                }
                            });
                            
                            torrent.on('error', (error) => {
                                destroySession(file.fileId, 'error');
                                state.errorCode = String(error);
                            });

                            let statsTickCount = 0;
                            const STATS_REPORT_EVERY_N_TICKS = 3; // 每 3 秒回報一次 stats
                            const tickId = setInterval(() => {
                                statsTickCount++;
                                const shouldReport = statsTickCount % STATS_REPORT_EVERY_N_TICKS === 0;
                                updateStateFromTorrent(file, torrent, shouldReport);
                            }, 1000);
                            torrentSessions[file.fileId] = { file, torrent, tickId };

                            torrent.on('download', () => updateStateFromTorrent(file, torrent, false));
                            torrent.on('wire', () => updateStateFromTorrent(file, torrent, false));
                            torrent.on('warning', (warning) => {
                                const msg = String(warning);
                                if (!msg.includes('WebSocket') && !msg.includes('tracker') && !msg.includes('wss://')) {
                                    warningText.value = msg;
                                }
                                updateStateFromTorrent(file, torrent, false);
                            });
                            torrent.on('done', async () => {
                                updateStateFromTorrent(file, torrent, true);

                                // 關閉 writable stream，將 .crswap 臨時檔 flush 為正式檔案
                                // commit() 後 get() 仍可透過 fileHandle.getFile() 繼續 seeding
                                if (chunkStore) {
                                    try {
                                        await chunkStore.commit();
                                    } catch (err) {
                                        console.warn('commit writable failed:', err);
                                    }
                                }

                                state.phase = 'seeding';
                                state.progressPercent = 100;
                                state.etaSeconds = 0;
                                state.sourceMix = torrent.numPeers > 0 ? 'P2P seeding' : '下載完成';
                                state.errorCode = null;

                                void reportClientStats(file, torrent, true);
                            });
                        } catch (error) {
                            state.phase = 'error';
                            state.errorCode = String(error);
                        }
                    }

                    function stopTransfer(fileId) {
                        destroySession(fileId, 'idle');
                    }

                    function phaseColor(phase) {
                        if (phase === 'seeding') return 'success';
                        if (phase === 'downloading') return 'info';
                        if (phase === 'error') return 'error';
                        return 'grey';
                    }

                    function phaseLabel(phase) {
                        if (phase === 'seeding') return '分享中';
                        if (phase === 'downloading') return '下載中';
                        if (phase === 'error') return '失敗';
                        return '待下載';
                    }

                    onMounted(async () => {
                        if (!window.isSecureContext) {
                            browserSupported.value = false;
                            statusText.value = '目前連線不是受信任的安全內容（憑證未受信任或主機名稱不符），瀏覽器已停用 File System Access API。請改用受信任憑證後再開啟此頁。';
                            return;
                        }

                        if (!window.showDirectoryPicker) {
                            browserSupported.value = false;
                            statusText.value = '本系統僅支援 File System Access API 之瀏覽器（如 Chrome、Edge）。';
                            return;
                        }

                        loadMetadata();
                        timerId.value = setInterval(loadMetadata, 5000);

                        const saved = await loadSavedDirectoryHandle();
                        if (saved) {
                            try {
                                const ok = await activateDirectory(saved);
                                if (ok) return;
                            } catch (_) {}
                        }
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
                        browserSupported,
                        directoryReady,
                        dirName,
                        loadMetadata,
                        downloadFile,
                        stopTransfer,
                        pickDirectory,
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
                                        <v-alert v-if="!browserSupported" type="error" variant="tonal" class="mb-3">
                                            {{ statusText }}
                                            <div class="tiny mt-2">請改用 Chrome、Edge 或其他支援 File System Access API 的瀏覽器。</div>
                                        </v-alert>

                                        <template v-if="browserSupported">
                                            <v-alert type="info" variant="tonal" class="mb-3">{{ statusText }}</v-alert>
                                            <v-alert v-if="warningText" type="warning" variant="tonal" class="mb-3">{{ warningText }}</v-alert>
                                            <v-alert v-if="metadataError" type="error" variant="tonal" class="mb-3">
                                                讀取 metadata 失敗：{{ metadataError }}
                                                <template #append>
                                                    <v-btn size="small" color="error" variant="text" @click="loadMetadata">重試</v-btn>
                                                </template>
                                            </v-alert>

                                            <v-card variant="outlined" class="mb-4">
                                                <v-card-text class="d-flex flex-wrap align-center ga-4">
                                                    <template v-if="directoryReady">
                                                        <v-icon size="small" color="success" class="mr-1">mdi-folder-check</v-icon>
                                                        <span>下載資料夾：<strong>{{ dirName }}</strong></span>
                                                        <v-btn size="small" variant="text" color="primary" @click="pickDirectory">變更資料夾</v-btn>
                                                    </template>
                                                    <template v-else>
                                                        <v-icon size="small" color="warning" class="mr-1">mdi-folder-alert</v-icon>
                                                        <span>請先選擇下載資料夾才能開始下載</span>
                                                        <v-btn size="small" color="primary" @click="pickDirectory">選擇下載資料夾</v-btn>
                                                    </template>
                                                    <template v-if="metadata">
                                                        <v-divider vertical class="mx-2" />
                                                        <div>檔案數：<strong>{{ metadata.fileCount }}</strong></div>
                                                        <div>總大小：<strong>{{ formatBytes(metadata.totalSize) }}</strong></div>
                                                    </template>
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
                                                        <div style="min-width: 280px" class="d-flex align-center ga-2">
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
                                                                :disabled="!directoryReady && !(downloads[file.fileId]?.phase === 'seeding' || downloads[file.fileId]?.phase === 'downloading')"
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
                                        </template>
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
        Some(session) => {
            let offset =
                guard.piece_offset_counter.fetch_add(1, Ordering::Relaxed) % NUM_PIECE_SLICES;
            let tracker_url = guard
                .server
                .as_ref()
                .map(|s| builtin_tracker_url(&s.base_url))
                .unwrap_or_default();
            (
                StatusCode::OK,
                Json(build_metadata_response(
                    session,
                    guard.fallback_http_enabled,
                    offset,
                    tracker_url,
                )),
            )
                .into_response()
        }
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

    // 全檔請求 (無 Range header)：用串流回應，不將整檔 buffer 進記憶體
    if !is_partial {
        use tokio_util::io::ReaderStream;

        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download.bin")
            .to_string();

        if let Ok(mut guard) = state.runtime.lock() {
            guard.fallback_transfer_count += 1;
            guard.last_activity_unix_ms = unix_time_ms();
        }

        let stream = ReaderStream::new(file);
        let body = axum::body::Body::from_stream(stream);

        let mut res_headers = axum::http::HeaderMap::new();
        res_headers.insert(axum::http::header::ACCEPT_RANGES, "bytes".parse().unwrap());
        res_headers.insert(
            axum::http::header::CONTENT_TYPE,
            "application/octet-stream".parse().unwrap(),
        );
        res_headers.insert(
            axum::http::header::CONTENT_LENGTH,
            file_size.to_string().parse().unwrap(),
        );
        res_headers.insert(
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{file_name}\"")
                .parse()
                .unwrap(),
        );
        return (StatusCode::OK, res_headers, body).into_response();
    }

    // Range 請求路徑：限制單次 chunk 大小，使用 semaphore 控制並行數
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

    // HTTP throttle: slow down web seed when P2P seeders are active
    {
        let seeding_peers = state
            .runtime
            .lock()
            .map(|g| {
                let reported = g.seeding_peer_count as usize;
                let tracker_seeders = tracker_total_seeders(&g.tracker);
                reported + tracker_seeders
            })
            .unwrap_or(0);
        let delay = compute_http_throttle_delay(seeding_peers);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
    }

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
    dev_log!("[mesh-p2p][http] health handler reached");
    Json(serde_json::json!({ "ok": true }))
}

// ─── Built-in WebSocket Tracker Handler ───

async fn tracker_ws_handler(
    ws: WebSocketUpgrade,
    AxumState(state): AxumState<HttpState>,
) -> impl IntoResponse {
    let current = state.tracker_conn_count.load(Ordering::Relaxed);
    if current >= TRACKER_MAX_CONNECTIONS as u64 {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    ws.on_upgrade(move |socket| tracker_ws_connection(socket, state))
        .into_response()
}

async fn tracker_ws_connection(socket: axum::extract::ws::WebSocket, state: HttpState) {
    use axum::extract::ws::Message;

    let conn_count = state.tracker_conn_count.clone();
    conn_count.fetch_add(1, Ordering::Relaxed);

    let (mut ws_sink, mut ws_stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    // Spawn outbound message forwarder
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut peer_id: Option<String> = None;

    // Process inbound messages
    while let Some(Ok(msg)) = ws_stream.next().await {
        match msg {
            Message::Text(text) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                    tracker_handle_message(&parsed, &tx, &mut peer_id, &state).await;
                }
            }
            Message::Binary(data) => {
                if let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&data) {
                    tracker_handle_message(&parsed, &tx, &mut peer_id, &state).await;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    // Cleanup: remove peer from all swarms
    if let Some(pid) = &peer_id {
        if let Ok(mut guard) = state.runtime.lock() {
            tracker_remove_peer(&mut guard.tracker, pid);
        }
    }

    conn_count.fetch_sub(1, Ordering::Relaxed);
    send_task.abort();
}

async fn tracker_handle_message(
    msg: &serde_json::Value,
    sender: &WsSender,
    peer_id_slot: &mut Option<String>,
    state: &HttpState,
) {
    let action = msg.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match action {
        "announce" => {
            let info_hash = match msg.get("info_hash").and_then(|v| v.as_str()) {
                Some(h) => h.to_string(),
                None => return,
            };
            let peer_id = match msg.get("peer_id").and_then(|v| v.as_str()) {
                Some(p) => p.to_string(),
                None => return,
            };

            *peer_id_slot = Some(peer_id.clone());

            let event = msg.get("event").and_then(|v| v.as_str()).unwrap_or("");
            let is_complete = event == "completed";

            // Check if this is an answer message (relay to target peer)
            if let Some(answer) = msg.get("answer") {
                let to_peer_id = msg.get("to_peer_id").and_then(|v| v.as_str()).unwrap_or("");
                let offer_id = msg.get("offer_id").cloned();

                if !to_peer_id.is_empty() {
                    let target_sender = {
                        let guard = match state.runtime.lock() {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                        guard
                            .tracker
                            .swarms
                            .get(&info_hash)
                            .and_then(|swarm| swarm.peers.get(to_peer_id))
                            .cloned()
                    };

                    if let Some(target_tx) = target_sender {
                        let mut relay = serde_json::json!({
                            "action": "announce",
                            "info_hash": info_hash,
                            "peer_id": peer_id,
                            "answer": answer,
                        });
                        if let Some(oid) = offer_id {
                            relay["offer_id"] = oid;
                        }
                        let _ = target_tx
                            .send(axum::extract::ws::Message::Text(relay.to_string().into()));
                    }
                }
                return;
            }

            // Normal announce: register peer and distribute offers
            let (complete_count, incomplete_count, offers_to_relay) = {
                let mut guard = match state.runtime.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                let tracker = &mut guard.tracker;

                // Register peer in swarm
                let swarm = tracker.swarms.entry(info_hash.clone()).or_default();
                swarm.peers.insert(peer_id.clone(), sender.clone());
                if is_complete || event == "completed" {
                    swarm.complete.insert(peer_id.clone());
                }
                if event == "stopped" {
                    swarm.peers.remove(&peer_id);
                    swarm.complete.remove(&peer_id);
                }

                // Track peer metadata
                let meta =
                    tracker
                        .peer_meta
                        .entry(peer_id.clone())
                        .or_insert_with(|| TrackerPeer {
                            peer_id: peer_id.clone(),
                            sender: sender.clone(),
                            is_complete,
                            info_hashes: HashSet::new(),
                            last_seen: Instant::now(),
                        });
                meta.info_hashes.insert(info_hash.clone());
                meta.last_seen = Instant::now();
                meta.sender = sender.clone();
                if is_complete {
                    meta.is_complete = true;
                }

                let complete_count = swarm.complete.len();
                let incomplete_count = swarm.peers.len().saturating_sub(complete_count);

                // Collect offers to relay to other peers
                let mut offers_to_relay: Vec<(WsSender, serde_json::Value)> = Vec::new();
                if let Some(offers) = msg.get("offers").and_then(|v| v.as_array()) {
                    // Get list of other peers in this swarm
                    let other_peers: Vec<(String, WsSender)> = swarm
                        .peers
                        .iter()
                        .filter(|(pid, _)| **pid != peer_id)
                        .map(|(pid, tx)| (pid.clone(), tx.clone()))
                        .collect();

                    if !other_peers.is_empty() {
                        for (i, offer) in offers.iter().enumerate() {
                            let target = &other_peers[i % other_peers.len()];
                            let relay_msg = serde_json::json!({
                                "action": "announce",
                                "info_hash": info_hash,
                                "peer_id": peer_id,
                                "offer": offer.get("offer"),
                                "offer_id": offer.get("offer_id"),
                            });
                            offers_to_relay.push((target.1.clone(), relay_msg));
                        }
                    }
                }

                (complete_count, incomplete_count, offers_to_relay)
            };

            // Send offers outside of lock
            for (target_tx, relay_msg) in offers_to_relay {
                let _ = target_tx.send(axum::extract::ws::Message::Text(
                    relay_msg.to_string().into(),
                ));
            }

            // Send announce response to the announcing peer
            let response = serde_json::json!({
                "action": "announce",
                "info_hash": info_hash,
                "complete": complete_count,
                "incomplete": incomplete_count,
                "interval": TRACKER_ANNOUNCE_INTERVAL,
            });
            let _ = sender.send(axum::extract::ws::Message::Text(
                response.to_string().into(),
            ));
        }
        "scrape" => {
            // Minimal scrape support
            if let Some(hashes) = msg.get("info_hash") {
                let guard = match state.runtime.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                let mut files = serde_json::Map::new();
                let hash_list: Vec<&str> = if let Some(arr) = hashes.as_array() {
                    arr.iter().filter_map(|v| v.as_str()).collect()
                } else if let Some(s) = hashes.as_str() {
                    vec![s]
                } else {
                    return;
                };
                for h in hash_list {
                    let (c, i) = guard
                        .tracker
                        .swarms
                        .get(h)
                        .map(|s| {
                            let c = s.complete.len();
                            (c, s.peers.len().saturating_sub(c))
                        })
                        .unwrap_or((0, 0));
                    files.insert(
                        h.to_string(),
                        serde_json::json!({ "complete": c, "incomplete": i }),
                    );
                }
                let resp = serde_json::json!({
                    "action": "scrape",
                    "files": files,
                });
                let _ = sender.send(axum::extract::ws::Message::Text(resp.to_string().into()));
            }
        }
        _ => {}
    }
}

fn tracker_remove_peer(tracker: &mut TrackerState, peer_id: &str) {
    if let Some(meta) = tracker.peer_meta.remove(peer_id) {
        for ih in &meta.info_hashes {
            if let Some(swarm) = tracker.swarms.get_mut(ih) {
                swarm.peers.remove(peer_id);
                swarm.complete.remove(peer_id);
                // Remove empty swarms
                if swarm.peers.is_empty() {
                    tracker.swarms.remove(ih);
                }
            }
        }
    }
}

fn tracker_total_seeders(tracker: &TrackerState) -> usize {
    tracker.swarms.values().map(|s| s.complete.len()).sum()
}

fn compute_http_throttle_delay(seeding_peers: usize) -> Duration {
    match seeding_peers {
        0 => Duration::ZERO,
        1..=3 => Duration::from_millis(50),
        4..=7 => Duration::from_millis(150),
        _ => Duration::from_millis(300),
    }
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

    dev_log!(
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
    piece_priority_offset: u64,
    builtin_tracker_url: String,
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
        piece_priority_offset,
        builtin_tracker_url,
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

    let piece_size = piece_size_for_file(file_size);
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

/// Build tracker URL list with builtin tracker prepended.
/// Converts base_url like `https://host:port` to `wss://host:port/announce`.
fn trackers_with_builtin(base_url: &str, external_trackers: &[String]) -> Vec<String> {
    let builtin = builtin_tracker_url(base_url);
    let mut all = vec![builtin];
    all.extend(external_trackers.iter().cloned());
    all
}

fn builtin_tracker_url(base_url: &str) -> String {
    let ws_base = base_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    format!("{ws_base}/announce")
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

fn resolve_bind_addr() -> SocketAddr {
    let port = std::env::var("MESH_P2P_PORT")
        .ok()
        .and_then(|raw| raw.trim().parse::<u16>().ok())
        .unwrap_or(0);

    SocketAddr::from(([0, 0, 0, 0], port))
}

fn resolve_advertised_host() -> String {
    if let Ok(raw) = std::env::var("MESH_P2P_HOST") {
        let value = raw.trim();
        if !value.is_empty() {
            if is_loopback_host(value) {
                dev_log!(
                    "[mesh-p2p][share] Ignoring loopback MESH_P2P_HOST='{}' for LAN sharing",
                    value
                );
            } else {
                return value.to_string();
            }
        }
    }

    let detected = detect_host_ip();
    if is_loopback_host(&detected) {
        dev_log!(
            "[mesh-p2p][share] Could not detect a LAN IP, fallback to {}",
            detected
        );
    }
    detected
}

fn is_loopback_host(host: &str) -> bool {
    let normalized = host.trim().trim_start_matches('[').trim_end_matches(']');
    if normalized.eq_ignore_ascii_case("localhost") {
        return true;
    }

    normalized
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

fn generate_runtime_tls_material(advertised_host: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut names = vec!["localhost".to_string()];
    let trimmed_host = advertised_host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');

    // rcgen SAN entries here are DNS names; IP hostnames are intentionally skipped.
    if !trimmed_host.is_empty()
        && !trimmed_host.eq_ignore_ascii_case("localhost")
        && trimmed_host.parse::<std::net::IpAddr>().is_err()
    {
        names.push(trimmed_host.to_string());
    }

    let certified = generate_simple_self_signed(names)
        .map_err(|e| format!("failed to generate self-signed cert: {e}"))?;
    let cert_pem = certified.cert.pem().into_bytes();
    let key_pem = certified.key_pair.serialize_pem().into_bytes();
    Ok((cert_pem, key_pem))
}

fn format_host_for_url(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
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
