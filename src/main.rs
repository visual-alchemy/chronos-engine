mod app;
mod domain;
mod gst_runtime;
mod media;
mod metrics;
mod planner;
mod profiles;
mod storage;
mod streams;
mod supervisor;
mod ui;

use anyhow::Result;
use std::net::SocketAddr;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("chronos_engine=info,tower_http=info")
        .init();
    gstreamer::init()?;
    let state = app::AppState::from_env()?;
    let router = app::router(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());
    let address: SocketAddr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8502".into())
        .parse()?;
    tracing::info!(%address, "CHRONOS API started");
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, router).await?;
    Ok(())
}
