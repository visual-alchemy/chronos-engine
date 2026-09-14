use crate::domain::MediaProbe;
use crate::media;
use crate::planner;
use crate::profiles::OutputProfiles;
use crate::storage::Database;
use crate::streams::SrtListenerConfig;
use crate::supervisor::Supervisor;
use anyhow::Result;
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct AppState {
    pub media_root: Arc<PathBuf>,
    database: Arc<Mutex<Database>>,
    supervisor: Supervisor,
}
impl AppState {
    pub fn from_env() -> Result<Self> {
        let root = std::env::var("MEDIA_ROOT").unwrap_or_else(|_| "/media".into());
        let database_path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| "chronos.db".into());
        let database = Database::open(&PathBuf::from(database_path))?;
        // Pipelines are process-local. A restarted server has no running
        // supervisor entries, so prior port reservations must not survive it.
        database.release_runtime_allocations()?;
        let profiles = OutputProfiles::from_yaml(include_str!("../config/output-profiles.yaml"))?;
        for id in ["srt-ts-universal", "srt-ts-hevc"] {
            if let Some(profile) = profiles.get(id) {
                database.save_profile(id, profile)?;
            }
        }
        Ok(Self {
            media_root: Arc::new(std::fs::canonicalize(root)?),
            database: Arc::new(Mutex::new(database)),
            supervisor: Supervisor::new(),
        })
    }
}
#[derive(Serialize)]
struct Health {
    status: &'static str,
    gstreamer_initialized: bool,
    media_root: String,
}
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/assets/app.css", get(stylesheet))
        .route("/assets/app.js", get(script))
        .route("/healthz", get(health))
        .route("/api/media", get(list_media))
        .route("/api/media/{id}/probe", get(probe_media))
        .route(
            "/api/media/{id}/compatibility/{profile_id}",
            get(compatibility),
        )
        .route("/api/streams", get(list_streams).post(create_stream))
        .route("/api/streams/{id}", get(get_stream).delete(delete_stream))
        .route("/api/streams/{id}/stop", axum::routing::post(stop_stream))
        .route("/api/events", get(event_socket))
        .route("/metrics", get(metrics))
        .route("/api/network", get(network))
        .with_state(state)
}
async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        gstreamer_initialized: true,
        media_root: state.media_root.display().to_string(),
    })
}
#[derive(Serialize)]
struct NetworkInfo {
    ip: Option<String>,
}
async fn network() -> Json<NetworkInfo> {
    Json(NetworkInfo {
        ip: detect_lan_ip(),
    })
}
/// Host's LAN IP for VLC-on-another-device; prefers `LAN_IP` env (Docker can't self-detect the host's IP), else probes the egress interface.
fn detect_lan_ip() -> Option<String> {
    if let Ok(ip) = std::env::var("LAN_IP") {
        let ip = ip.trim();
        if !ip.is_empty() {
            return Some(ip.to_string());
        }
    }
    egress_ip()
}
fn egress_ip() -> Option<String> {
    use std::net::{IpAddr, UdpSocket};
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() => Some(v4.to_string()),
        _ => None,
    }
}
async fn dashboard() -> Html<&'static str> {
    Html(crate::ui::HTML)
}
async fn stylesheet() -> ([(&'static str, &'static str); 1], &'static str) {
    (
        [("content-type", "text/css; charset=utf-8")],
        crate::ui::CSS,
    )
}
async fn script() -> ([(&'static str, &'static str); 1], &'static str) {
    (
        [("content-type", "application/javascript; charset=utf-8")],
        crate::ui::JS,
    )
}
async fn metrics(State(state): State<AppState>) -> ([(&'static str, &'static str); 1], String) {
    (
        [("content-type", "text/plain; version=0.0.4")],
        crate::metrics::render(&state.supervisor.states()),
    )
}
async fn list_media(State(state): State<AppState>) -> Json<Vec<crate::domain::MediaItem>> {
    let mut items = media::scan(&state.media_root);
    if let Ok(database) = state.database.lock() {
        for item in &mut items {
            if let Ok(path) = media::resolve_media_id(&state.media_root, &item.id)
                && let Ok(Some(probe)) = database.cached_probe(&path)
            {
                item.probe_status = probe.status;
            }
        }
    }
    Json(items)
}

#[derive(Serialize)]
struct ApiError {
    code: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, Json(self)).into_response()
    }
}

async fn probe_media(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<MediaProbe>, ApiError> {
    let path = media::resolve_media_id(&state.media_root, &id).map_err(|error| ApiError {
        code: "invalid_media",
        message: error.to_string(),
    })?;
    if let Ok(database) = state.database.lock()
        && let Ok(Some(probe)) = database.cached_probe(&path)
    {
        return Ok(Json(probe.clone()));
    }
    let probe_path = path.clone();
    let probe_id = id.clone();
    let probe = tokio::task::spawn_blocking(move || media::probe(&probe_path, probe_id))
        .await
        .map_err(|error| ApiError {
            code: "probe_task_failed",
            message: error.to_string(),
        })?;
    if let Ok(database) = state.database.lock()
        && let Err(error) = database.cache_probe(&path, &probe)
    {
        tracing::warn!(%error, "could not persist media probe cache");
    }
    Ok(Json(probe))
}

async fn compatibility(
    Path((id, profile_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<crate::domain::CompatibilityPlan>, ApiError> {
    let path = media::resolve_media_id(&state.media_root, &id).map_err(|error| ApiError {
        code: "invalid_media",
        message: error.to_string(),
    })?;
    let probe = state
        .database
        .lock()
        .ok()
        .and_then(|database| database.cached_probe(&path).ok().flatten())
        .ok_or_else(|| ApiError {
            code: "probe_required",
            message: "probe the media before requesting compatibility".into(),
        })?;
    let profiles = OutputProfiles::from_yaml(include_str!("../config/output-profiles.yaml"))
        .map_err(|error| ApiError {
            code: "profile_configuration_invalid",
            message: error.to_string(),
        })?;
    let profile = profiles.get(&profile_id).ok_or_else(|| ApiError {
        code: "unknown_profile",
        message: format!("profile {profile_id} does not exist"),
    })?;
    let mut plan = planner::plan(&probe, profile);
    plan.profile_id = profile_id;
    Ok(Json(plan))
}

#[derive(Deserialize)]
struct CreateStreamRequest {
    id: String,
    media_id: String,
    port: u16,
    latency_ms: Option<u32>,
    transcode: Option<bool>,
    mode: Option<crate::domain::ProcessingMode>,
}

async fn list_streams(State(state): State<AppState>) -> Json<Vec<crate::domain::StreamEvent>> {
    Json(state.supervisor.states())
}

async fn get_stream(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<crate::domain::StreamEvent>, ApiError> {
    state
        .supervisor
        .states()
        .into_iter()
        .find(|stream| stream.stream_id == id)
        .map(Json)
        .ok_or_else(|| ApiError {
            code: "stream_not_found",
            message: "stream does not exist".into(),
        })
}

async fn create_stream(
    State(state): State<AppState>,
    Json(request): Json<CreateStreamRequest>,
) -> Result<Json<crate::domain::StreamEvent>, ApiError> {
    let config = SrtListenerConfig::new(request.port, request.latency_ms.unwrap_or(120)).map_err(
        |error| ApiError {
            code: "invalid_stream_config",
            message: error.to_string(),
        },
    )?;
    let path = media::resolve_media_id(&state.media_root, &request.media_id).map_err(|error| {
        ApiError {
            code: "invalid_media",
            message: error.to_string(),
        }
    })?;
    state
        .database
        .lock()
        .map_err(|_| ApiError {
            code: "storage_unavailable",
            message: "stream storage lock unavailable".into(),
        })?
        .allocate_port(&request.id, config.port)
        .map_err(|error| ApiError {
            code: "port_unavailable",
            message: error.to_string(),
        })?;
    let selected_mode = request.mode.clone();
    let start = if let Some(
        mode @ (crate::domain::ProcessingMode::CopyVideoEncodeAudio
        | crate::domain::ProcessingMode::EncodeVideoCopyAudio),
    ) = selected_mode.clone()
    {
        state
            .supervisor
            .start_hybrid(request.id.clone(), &path, config, mode)
    } else if request.transcode.unwrap_or(false) {
        state
            .supervisor
            .start_transcode(request.id.clone(), &path, config)
    } else {
        state
            .supervisor
            .start_copy(request.id.clone(), &path, config)
    };
    if let Err(error) = start {
        if let Ok(database) = state.database.lock() {
            let _ = database.release_port(&request.id);
        }
        return Err(ApiError {
            code: "stream_start_failed",
            message: error.to_string(),
        });
    }
    Ok(Json(crate::domain::StreamEvent {
        stream_id: request.id,
        state: crate::domain::StreamState::WaitingForCaller,
        detail: None,
        port: Some(config.port),
        latency_ms: Some(config.latency_ms),
        mode: selected_mode
            .or_else(|| {
                request
                    .transcode
                    .unwrap_or(false)
                    .then_some(crate::domain::ProcessingMode::FullTranscode)
            })
            .or(Some(crate::domain::ProcessingMode::RemuxCopy)),
        loop_count: Some(0),
        clients: Vec::new(),
    }))
}

async fn stop_stream(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<crate::domain::StreamEvent>, ApiError> {
    state.supervisor.stop(&id).map_err(|error| ApiError {
        code: "stream_stop_failed",
        message: error.to_string(),
    })?;
    if let Ok(database) = state.database.lock() {
        let _ = database.release_port(&id);
    }
    Ok(Json(crate::domain::StreamEvent {
        stream_id: id,
        state: crate::domain::StreamState::Stopped,
        detail: None,
        port: None,
        latency_ms: None,
        mode: None,
        loop_count: None,
        clients: Vec::new(),
    }))
}

async fn delete_stream(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<crate::domain::StreamEvent>, ApiError> {
    stop_stream(Path(id), State(state)).await
}

async fn event_socket(
    websocket: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let mut events = state.supervisor.subscribe();
    websocket.on_upgrade(move |mut socket| async move {
        while let Ok(event) = events.recv().await {
            let Ok(payload) = serde_json::to_string(&event) else {
                continue;
            };
            if socket.send(Message::Text(payload.into())).await.is_err() {
                break;
            }
        }
    })
}
