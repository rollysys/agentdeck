//! agentdeck — local web launcher. Binds 127.0.0.1:7860, serves a static
//! card UI, reads profiles from `~/.config/agentdeck/profiles.yaml`, and
//! spawns an agent-in-pty per WebSocket connection to `/ws/spawn`.

mod profile;
mod pty;
mod ws_handler;

use axum::{
    extract::{Query, WebSocketUpgrade},
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use std::net::SocketAddr;

const INDEX_HTML: &str = include_str!("index.html");
const BIND_ADDR: &str = "127.0.0.1:7860";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/profiles", get(profiles))
        .route("/ws/spawn", get(ws_spawn));

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

async fn ws_spawn(
    ws: WebSocketUpgrade,
    Query(params): Query<ws_handler::SpawnParams>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_handler::handle(socket, params))
}
