//! WebSocket ↔ pty bridge.
//!
//! Protocol:
//!   - Server → client **binary** frames: pty stdout bytes, forwarded verbatim.
//!   - Client → server **binary** frames: pty stdin bytes, forwarded verbatim.
//!   - Client → server **text** frames: JSON control messages.
//!   - Server → client **text** frames: JSON control messages (errors, exits).
//!
//! When the WebSocket closes (or either direction errors), we drop the
//! pty master; portable-pty closes the fd, the child receives SIGHUP
//! and exits. No session persistence in P2 — a browser reload starts
//! a fresh agent.

use crate::profile;
use crate::pty::{spawn_for_profile, PtySession, SpawnMode};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use portable_pty::PtySize;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Debug, Deserialize)]
pub struct SpawnParams {
    pub profile: String,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
    #[serde(default)]
    pub mode: SpawnMode,
    /// Required when `mode=resume`; ignored otherwise.
    #[serde(default)]
    pub sid: Option<String>,
    /// Optional skill to preload at launch. Honored for claude / hermes
    /// via `--skills <name>`; silently ignored for qwen / codex (they
    /// don't expose an equivalent flag).
    #[serde(default)]
    pub skill: Option<String>,
}

fn default_cols() -> u16 {
    120
}
fn default_rows() -> u16 {
    32
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientControl {
    Resize { cols: u16, rows: u16 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerControl {
    Error { message: String },
    Exit,
}

pub async fn handle(socket: WebSocket, params: SpawnParams) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Resolve profile.
    let report = profile::load();
    let prof = match report.profiles.iter().find(|p| p.name == params.profile) {
        Some(p) => p.clone(),
        None => {
            send_error(&mut ws_tx, format!("profile {:?} not found", params.profile)).await;
            return;
        }
    };

    // Spawn the pty on a blocking thread — portable-pty does openpty/fork
    // synchronously and we don't want to stall the tokio scheduler.
    let spawn_prof = prof.clone();
    let cols = params.cols;
    let rows = params.rows;
    let mode = params.mode;
    let sid = params.sid.clone();
    let skill = params.skill.clone();
    let session_res = tokio::task::spawn_blocking(move || {
        spawn_for_profile(
            &spawn_prof,
            cols,
            rows,
            mode,
            sid.as_deref(),
            skill.as_deref(),
        )
    })
    .await;

    let session = match session_res {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            send_error(&mut ws_tx, format!("spawn failed: {e}")).await;
            return;
        }
        Err(e) => {
            send_error(&mut ws_tx, format!("spawn panicked: {e}")).await;
            return;
        }
    };

    let PtySession {
        master,
        mut reader,
        mut writer,
        child,
    } = session;

    // Hold master inside an Arc<Mutex> so we can resize from the async
    // control-message branch while the reader thread owns its own clone.
    let master = Arc::new(Mutex::new(master));

    // pty stdout → mpsc → ws.send(Binary). The read loop is blocking, so
    // it lives in a std thread and hands bytes over an unbounded channel.
    let (pty_out_tx, mut pty_out_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if pty_out_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // ws stdin → mpsc → std thread → pty.writer. A std mpsc channel is
    // enough — we never cross await points with the writer.
    let (pty_in_tx, pty_in_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Ok(bytes) = pty_in_rx.recv() {
            if writer.write_all(&bytes).is_err() {
                break;
            }
            if writer.flush().is_err() {
                break;
            }
        }
    });

    // Skill preload is wired entirely through spawn args in `pty.rs`:
    //   hermes → native `--skills <name>` flag
    //   claude → positional `[prompt]` arg `/skill-name` (claude's TUI
    //            dispatches messages starting with `/` as slash commands)
    //   qwen / codex → no equivalent; UI keeps the launch button disabled.

    loop {
        tokio::select! {
            // pty → ws
            out = pty_out_rx.recv() => {
                let Some(bytes) = out else { break };
                if ws_tx.send(Message::Binary(bytes.into())).await.is_err() {
                    break;
                }
            }
            // ws → pty / control
            msg = ws_rx.next() => {
                let Some(Ok(msg)) = msg else { break };
                match msg {
                    Message::Binary(b) => {
                        if pty_in_tx.send(b.to_vec()).is_err() {
                            break;
                        }
                    }
                    Message::Text(t) => {
                        match serde_json::from_str::<ClientControl>(t.as_str()) {
                            Ok(ClientControl::Resize { cols, rows }) => {
                                let m = master.clone();
                                let _ = tokio::task::spawn_blocking(move || {
                                    let _ = m.lock().unwrap().resize(PtySize {
                                        cols,
                                        rows,
                                        pixel_width: 0,
                                        pixel_height: 0,
                                    });
                                }).await;
                            }
                            Err(_) => {
                                // Ignore malformed control; we deliberately don't
                                // close the socket on client-side protocol junk.
                            }
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }

    // Cleanup: drop stdin channel (ends writer thread), drop master (pty
    // close → SIGHUP to child), then send a final "exit" control frame
    // so the UI can tell the difference between a transient ws glitch
    // and an intentional end-of-session.
    drop(pty_in_tx);
    drop(master);
    drop(child);

    let _ = ws_tx
        .send(Message::Text(
            serde_json::to_string(&ServerControl::Exit)
                .unwrap_or_default()
                .into(),
        ))
        .await;
    let _ = ws_tx.close().await;
}

async fn send_error<S>(ws_tx: &mut S, message: String)
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::fmt::Display,
{
    let payload = serde_json::to_string(&ServerControl::Error { message })
        .unwrap_or_else(|_| "{\"type\":\"error\"}".into());
    let _ = ws_tx.send(Message::Text(payload.into())).await;
    let _ = ws_tx.close().await;
}
