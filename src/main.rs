//! agentdeck — local web launcher. Binds 127.0.0.1:7860, serves a static
//! card UI, reads profiles from `~/.config/agentdeck/profiles.yaml`, and
//! spawns an agent-in-pty per WebSocket connection to `/ws/spawn`.

mod profile;
mod pty;
mod sessions;
mod ws_handler;

use axum::{
    extract::{Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use sessions::SessionIndex;
use std::net::SocketAddr;
use std::sync::Arc;

const INDEX_HTML: &str = include_str!("index.html");
const BIND_ADDR: &str = "127.0.0.1:7860";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let idx = SessionIndex::new();
    idx.clone().spawn_background_refresh();

    let app = Router::new()
        .route("/", get(index))
        .route("/api/profiles", get(profiles))
        .route("/api/sessions", get(sessions_handler))
        .route("/ws/spawn", get(ws_spawn))
        .with_state(idx);

    let addr: SocketAddr = BIND_ADDR.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("agentdeck listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn profiles() -> Json<profile::LoadReport> {
    Json(profile::load())
}

#[derive(Debug, Deserialize)]
struct SessionsQuery {
    profile: String,
}

async fn sessions_handler(
    State(idx): State<Arc<SessionIndex>>,
    Query(q): Query<SessionsQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let report = profile::load();
    let Some(prof) = report.profiles.iter().find(|p| p.name == q.profile) else {
        return Err(StatusCode::NOT_FOUND);
    };

    // Before the first refresh completes the cached list is empty —
    // that would look like "no history" to callers. Returning 503 lets
    // the UI render "indexing…" instead of an empty list.
    if !idx.is_ready().await {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    Ok(Json(idx.for_profile(prof).await))
}

async fn ws_spawn(
    ws: WebSocketUpgrade,
    Query(params): Query<ws_handler::SpawnParams>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_handler::handle(socket, params))
}
