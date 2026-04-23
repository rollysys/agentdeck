//! agentdeck — local web launcher. Binds 127.0.0.1:7860, serves a static
//! card UI, reads profiles from `~/.config/agentdeck/profiles.yaml`, and
//! spawns an agent-in-pty per WebSocket connection to `/ws/spawn`.

mod aliases;
mod discover;
mod env;
mod profile;
mod pty;
mod running;
mod sessions;
mod skills;
mod ws_handler;

use axum::{
    extract::{Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use running::{ProfileStatus, RunningCounter};
use serde::{Deserialize, Serialize};
use sessions::SessionIndex;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

const INDEX_HTML: &str = include_str!("index.html");
const BIND_ADDR: &str = "127.0.0.1:7860";

#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<SessionIndex>,
    pub running: Arc<RunningCounter>,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("discover") {
        return discover::run(&args[2..]);
    }
    if matches!(args.get(1).map(String::as_str), Some("-h") | Some("--help")) {
        print!("{TOP_HELP}");
        return Ok(());
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_server())
}

const TOP_HELP: &str = "\
agentdeck — local web launcher for multi-agent profiles

USAGE:
  agentdeck              start the web server on 127.0.0.1:7860
  agentdeck discover     scan claude history for new cwds to configure
                         (run `agentdeck discover --help` for flags)
  agentdeck --help       print this message
";

async fn run_server() -> anyhow::Result<()> {
    let state = AppState {
        sessions: SessionIndex::new(),
        running: RunningCounter::new(),
    };
    state.sessions.clone().spawn_background_refresh();

    let app = Router::new()
        .route("/", get(index))
        .route("/api/profiles", get(profiles))
        .route("/api/sessions", get(sessions_handler))
        .route("/api/skills", get(skills_handler))
        .route("/api/env", get(env_handler))
        .route("/api/status", get(status_handler))
        .route("/api/aliases", get(aliases_get).post(aliases_post))
        .route("/ws/spawn", get(ws_spawn))
        .with_state(state);

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
    State(state): State<AppState>,
    Query(q): Query<SessionsQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let report = profile::load();
    let Some(prof) = report.profiles.iter().find(|p| p.name == q.profile) else {
        return Err(StatusCode::NOT_FOUND);
    };

    // Before the first refresh completes the cached list is empty —
    // that would look like "no history" to callers. Returning 503 lets
    // the UI render "indexing…" instead of an empty list.
    if !state.sessions.is_ready().await {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    Ok(Json(state.sessions.for_profile(prof).await))
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    /// Keyed by profile name.
    profiles: HashMap<String, ProfileStatus>,
    /// `false` until the first session index refresh finishes; UI treats
    /// `last_active_ts` as "unknown" in that window instead of "never".
    session_index_ready: bool,
}

async fn status_handler(State(state): State<AppState>) -> impl IntoResponse {
    let report = profile::load();
    let running = state.running.snapshot();
    let ready = state.sessions.is_ready().await;

    let mut profiles = HashMap::with_capacity(report.profiles.len());
    for p in &report.profiles {
        let last_active_ts = if ready {
            state.sessions.last_active_for_profile(p).await
        } else {
            None
        };
        profiles.insert(
            p.name.clone(),
            ProfileStatus {
                running: running.get(&p.name).copied().unwrap_or(0),
                last_active_ts,
            },
        );
    }
    Json(StatusResponse {
        profiles,
        session_index_ready: ready,
    })
}

#[derive(Debug, Deserialize)]
struct SkillsQuery {
    profile: String,
}

async fn skills_handler(
    Query(q): Query<SkillsQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let report = profile::load();
    let Some(prof) = report.profiles.iter().find(|p| p.name == q.profile) else {
        return Err(StatusCode::NOT_FOUND);
    };
    Ok(Json(skills::for_profile(prof)))
}

#[derive(Debug, Deserialize)]
struct EnvQuery {
    profile: String,
}

async fn env_handler(
    Query(q): Query<EnvQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let report = profile::load();
    let Some(prof) = report.profiles.iter().find(|p| p.name == q.profile) else {
        return Err(StatusCode::NOT_FOUND);
    };
    Ok(Json(env::for_profile(prof)))
}

async fn ws_spawn(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(params): Query<ws_handler::SpawnParams>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_handler::handle(socket, params, state.running))
}

async fn aliases_get() -> Json<aliases::Aliases> {
    Json(aliases::load())
}

#[derive(Debug, Deserialize)]
struct AliasUpdate {
    profile: String,
    /// `None` or empty clears the alias.
    #[serde(default)]
    alias: Option<String>,
}

async fn aliases_post(
    axum::Json(body): axum::Json<AliasUpdate>,
) -> Result<Json<aliases::Aliases>, (StatusCode, String)> {
    aliases::set(&body.profile, body.alias.as_deref())
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
