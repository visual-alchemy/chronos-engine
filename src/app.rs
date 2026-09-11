use std::{path::PathBuf, sync::Arc};
use anyhow::Result;
use axum::{extract::State, routing::get, Json, Router};
use serde::Serialize;
use crate::media;

#[derive(Clone)]
pub struct AppState { pub media_root: Arc<PathBuf> }
impl AppState {
    pub fn from_env() -> Result<Self> {
        let root = std::env::var("MEDIA_ROOT").unwrap_or_else(|_| "/media".into());
        Ok(Self { media_root: Arc::new(PathBuf::from(root)) })
    }
}
#[derive(Serialize)]
struct Health { status: &'static str, gstreamer_initialized: bool, media_root: String }
pub fn router(state: AppState) -> Router {
    Router::new().route("/healthz", get(health)).route("/api/media", get(list_media)).with_state(state)
}
async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health { status: "ok", gstreamer_initialized: true, media_root: state.media_root.display().to_string() })
}
async fn list_media(State(state): State<AppState>) -> Json<Vec<crate::domain::MediaItem>> {
    Json(media::scan(&state.media_root))
}
