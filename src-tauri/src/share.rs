use std::{
    collections::{HashSet, VecDeque},
    net::{SocketAddr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::ConnectInfo,
    extract::Path as AxumPath,
    extract::State as AxumState,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use sha1::{Digest, Sha1};
use tauri::{State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tokio::{fs, io::AsyncReadExt, sync::oneshot};

const DEFAULT_PIECE_SIZE: usize = 256 * 1024;
const RATE_LIMIT_WINDOW_MS: u64 = 1000;
const RATE_LIMIT_MAX: usize = 40;
const CLIENT_ACTIVITY_WINDOW_SECS: u64 = 300;
const CHUNK_SIZE: usize = 10 * 1024 * 1024; // 10MB chunks for streaming hash

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
    processing_progress: Option<ProcessingProgress>,
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
    metadata_revision: u64,
    last_activity_unix_ms: u128,
    client_activity: VecDeque<ClientActivity>,
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
}

#[derive(Debug)]
struct ClientActivity {
    ip: String,
    seen_at: Instant,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    error: String,
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
                metadata_revision: 0,
                last_activity_unix_ms: unix_time_ms(),
                client_activity: VecDeque::new(),
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

    let (tracker_urls, existing_paths, next_index, file_count) = {
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
    let new_files =
        match build_shared_files(file_paths, &tracker_urls, &existing_paths, next_index, {
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
        })
        .await
        {
            Ok(files) => files,
            Err(err) => {
                if let Ok(mut guard) = state.inner.lock() {
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
    let files = match build_shared_files(file_paths, &tracker_urls, &HashSet::new(), 0, {
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
    })
    .await
    {
        Ok(files) => files,
        Err(err) => {
            if let Ok(mut guard) = state.inner.lock() {
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
        tracker_urls,
        started_at_unix_ms: started_at,
    };

    {
        let mut guard = state
            .inner
            .lock()
            .map_err(|_| "State lock poisoned".to_string())?;
        guard.http_uploaded_bytes = 0;
        guard.metadata_revision = 1;
        guard.last_activity_unix_ms = started_at;
        guard.client_activity.clear();
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
    };

    let router = Router::new()
        .route("/", get(download_page_handler))
        .route("/api/metadata", get(metadata_handler))
        .route("/api/file/{file_id}", get(file_handler))
        .route("/api/health", get(health_handler))
        .with_state(http_state);

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

async fn download_page_handler() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html lang="zh-Hant">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Mesh P2P Download</title>
    <style>
            body { font-family: sans-serif; padding: 2rem; line-height: 1.5; }
      .card { max-width: 680px; margin: 0 auto; border: 1px solid #ddd; border-radius: 10px; padding: 1.2rem; }
      code { background: #f2f2f2; padding: 0.1rem 0.25rem; border-radius: 4px; }
      .muted { color: #666; }
      .row { margin-bottom: 0.75rem; }
            .grid { display: grid; gap: 0.75rem; }
      button { padding: 0.5rem 1rem; }
    </style>
  </head>
  <body>
    <div class="card">
      <h1>Mesh P2P 分享下載頁</h1>
            <div id="status" class="row muted">載入中...</div>
            <div id="meta" class="grid"></div>
            <div id="files" class="grid"></div>
    </div>
    <script>
            let lastRevision = -1;
      async function loadMetadata() {
        const status = document.getElementById('status');
        const meta = document.getElementById('meta');
                const files = document.getElementById('files');
        try {
          const resp = await fetch('/api/metadata');
          if (!resp.ok) {
            status.textContent = '目前沒有可用分享。';
            meta.textContent = '';
                        files.textContent = '';
            return;
          }
          const data = await resp.json();
                    if (data.revision === lastRevision) {
                        status.textContent = `分享中，已同步 ${data.fileCount} 個檔案`;
                        return;
                    }
                    lastRevision = data.revision;
          status.textContent = '分享中';
                    meta.innerHTML = `<div class='row'>分享檔案數：<strong>${data.fileCount}</strong></div><div class='row'>總大小：${data.totalSize} bytes</div><div class='row'>版本：<code>${data.revision}</code></div>`;
                    files.innerHTML = data.files.map((file) => `<div class='row'><strong>${file.fileName}</strong> (${file.fileSize} bytes) <a href='/api/file/${file.fileId}' download>下載</a></div>`).join('');
        } catch (_) {
          status.textContent = '無法連線到分享服務。';
        }
      }
      loadMetadata();
            setInterval(loadMetadata, 5000);
    </script>
  </body>
</html>"#,
    )
}

async fn metadata_handler(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumState(state): AxumState<HttpState>,
) -> impl IntoResponse {
    if let Err(status) = enforce_rate_limit(&state) {
        return (
            status,
            Json(ErrorResponse {
                error: "Rate limit exceeded".to_string(),
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
                }),
            )
                .into_response();
        }
    };

    record_client_activity(&mut guard, addr);

    match &guard.session {
        Some(session) => (StatusCode::OK, Json(session.clone())).into_response(),
        None => (
            StatusCode::GONE,
            Json(ErrorResponse {
                error: "Share session is not active".to_string(),
            }),
        )
            .into_response(),
    }
}

async fn file_handler(
    AxumPath(file_id): AxumPath<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    AxumState(state): AxumState<HttpState>,
) -> impl IntoResponse {
    if let Err(status) = enforce_rate_limit(&state) {
        return (
            status,
            Json(ErrorResponse {
                error: "Rate limit exceeded".to_string(),
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
                    }),
                )
                    .into_response();
            }
        }
    };

    match fs::read(&file_path).await {
        Ok(bytes) => {
            if let Ok(mut guard) = state.runtime.lock() {
                guard.http_uploaded_bytes += bytes.len() as u64;
                guard.last_activity_unix_ms = unix_time_ms();
            }

            let file_name = file_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("download.bin")
                .to_string();

            (
                StatusCode::OK,
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        "application/octet-stream".to_string(),
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{file_name}\""),
                    ),
                ],
                bytes,
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to read shared file: {err}"),
            }),
        )
            .into_response(),
    }
}

async fn health_handler() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true }))
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

        let file_idx = offset + 1;
        let file_total_size = fs::metadata(&path)
            .await
            .map_err(|e| format!("Failed to stat file: {e}"))?
            .len();

        progress_callback(file_name.clone(), file_idx, file_total_size, 0)?;

        let metadata = build_seed_metadata(&path, trackers, {
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
            file_id: format!("file-{}", next_index + offset + 1),
            file_name,
            file_path: normalized.clone(),
            file_size: metadata.file_size,
            info_hash: metadata.info_hash,
            piece_size: metadata.piece_size,
            piece_count: metadata.piece_count,
            magnet_uri: metadata.magnet_uri,
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
        metadata_revision: runtime.metadata_revision,
        last_activity_unix_ms: runtime.last_activity_unix_ms,
    }
}

#[derive(Debug)]
struct SeedMetadata {
    file_size: u64,
    info_hash: String,
    piece_size: usize,
    piece_count: usize,
    magnet_uri: String,
}

async fn build_seed_metadata(
    path: &Path,
    trackers: &[String],
    progress_callback: impl Fn(u64) -> Result<(), String> + Send,
) -> Result<SeedMetadata, String> {
    let file_size = fs::metadata(path)
        .await
        .map_err(|e| format!("Failed to stat file: {e}"))?
        .len();

    let piece_size = DEFAULT_PIECE_SIZE;
    let piece_count = if file_size == 0 {
        1
    } else {
        (file_size as usize).div_ceil(piece_size)
    };

    let mut file = fs::File::open(path)
        .await
        .map_err(|e| format!("Failed to open file: {e}"))?;

    let mut hasher = Sha1::new();
    let mut buffer = vec![0; CHUNK_SIZE];
    let mut bytes_processed = 0u64;

    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|e| format!("Failed to read file: {e}"))?;
        if n == 0 {
            break;
        }

        hasher.update(&buffer[..n]);
        bytes_processed += n as u64;
        progress_callback(bytes_processed)?;
    }

    let info_hash = hex::encode(hasher.finalize());

    let display_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("shared-file");

    let magnet_uri = build_magnet_uri(&info_hash, display_name, file_size, trackers);

    Ok(SeedMetadata {
        file_size,
        info_hash,
        piece_size,
        piece_count,
        magnet_uri,
    })
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
    use super::{build_seed_metadata, parse_trackers};
    use std::io::Write;

    #[tokio::test]
    async fn build_seed_metadata_for_valid_file() {
        let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
        tmp.write_all(b"mesh-p2p-test").expect("write temp");

        let trackers = vec!["wss://tracker.example.com".to_string()];
        let metadata = build_seed_metadata(tmp.path(), &trackers, |_| Ok(()))
            .await
            .expect("metadata should be generated");

        assert!(metadata.file_size > 0);
        assert!(!metadata.info_hash.is_empty());
        assert!(metadata.magnet_uri.contains("magnet:?xt=urn:btih:"));
        assert!(metadata.magnet_uri.contains("tracker.example.com"));
    }

    #[tokio::test]
    async fn build_seed_metadata_for_invalid_file_fails() {
        let trackers = vec!["wss://tracker.example.com".to_string()];
        let result =
            build_seed_metadata(std::path::Path::new("/no/such/file"), &trackers, |_| Ok(())).await;

        assert!(result.is_err());
    }

    #[test]
    fn parse_trackers_filters_invalid_values() {
        let trackers = parse_trackers("wss://ok,ftp://bad,udp://ok2,just-text,https://ok3");

        assert_eq!(trackers.len(), 3);
        assert!(trackers.iter().any(|s| s == "wss://ok"));
        assert!(trackers.iter().any(|s| s == "udp://ok2"));
        assert!(trackers.iter().any(|s| s == "https://ok3"));
    }
}
