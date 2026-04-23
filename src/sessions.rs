//! Per-profile session list, backed by auditui-core's transcript index.
//!
//! `auditui_core::session::index_all()` walks the entire `~/.claude/`,
//! `~/.codex/`, `~/.qwen/` trees and hermes' SQLite — on a laptop with
//! ten thousand sessions that's a ~10-second scan. We run it once at
//! startup on a blocking task and then refresh on a 60-second timer;
//! API handlers read from the cached `Arc<RwLock<Vec<SessionMeta>>>`.
//!
//! Filtering rules per profile:
//!   - agent must match profile.agent
//!   - for claude / codex / qwen: cwd must equal profile.cwd
//!   - for hermes: cwd is NOT recorded per session, so we don't filter
//!     on it and we surface a `cwd_scoped: false` flag so the UI can
//!     explain why the list is unscoped.

use crate::profile::{AgentKind, Profile};
use auditui_core::providers::Agent;
use auditui_core::session::{self, SessionMeta};
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

const HISTORY_LIMIT: usize = 50;
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    /// `SessionMeta.id` as-is — includes the provider prefix
    /// (`codex:` / `qwen:` / `hermes:`). Useful for display / debugging.
    pub id: String,
    /// Raw session id with any provider prefix stripped off. This is what
    /// each agent's `--resume <sid>` / `resume <sid>` actually accepts.
    pub sid: String,
    pub turns: usize,
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub last_active_ts: u64,
    pub started_at_ts: u64,
    pub is_scripted: bool,
}

#[derive(Debug, Serialize)]
pub struct SessionsResponse {
    pub profile: String,
    pub sessions: Vec<SessionSummary>,
    /// false when the agent's sessions aren't keyed by cwd (currently
    /// only hermes). UI uses this to explain why filtering is looser.
    pub cwd_scoped: bool,
    /// Sessions considered for this profile (post agent match,
    /// pre cwd + limit). Helps the UI distinguish "no sessions yet" from
    /// "filtered out by cwd".
    pub considered: usize,
}

pub struct SessionIndex {
    list: RwLock<Vec<SessionMeta>>,
    ready: RwLock<bool>,
}

impl SessionIndex {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            list: RwLock::new(Vec::new()),
            ready: RwLock::new(false),
        })
    }

    pub async fn refresh(self: &Arc<Self>) {
        let fresh = tokio::task::spawn_blocking(session::index_all)
            .await
            .unwrap_or_default();
        *self.list.write().await = fresh;
        *self.ready.write().await = true;
    }

    pub fn spawn_background_refresh(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                self.refresh().await;
                tokio::time::sleep(REFRESH_INTERVAL).await;
            }
        });
    }

    pub async fn is_ready(&self) -> bool {
        *self.ready.read().await
    }

    /// Latest `last_active_ts` across sessions that belong to this profile,
    /// using the same filtering rules as `for_profile`. Cheap — no sort,
    /// no truncate, no cloning of the list.
    pub async fn last_active_for_profile(&self, profile: &Profile) -> Option<u64> {
        let list = self.list.read().await;
        let target_agent = core_agent(profile.agent);
        let cwd_scoped = !matches!(profile.agent, AgentKind::Hermes);
        list.iter()
            .filter(|s| s.agent == target_agent)
            .filter(|s| {
                !cwd_scoped
                    || s.cwd
                        .as_deref()
                        .map(|c| c == profile.cwd)
                        .unwrap_or(false)
            })
            .map(|s| s.last_active_ts)
            .max()
    }

    pub async fn for_profile(&self, profile: &Profile) -> SessionsResponse {
        let list = self.list.read().await;
        let target_agent = core_agent(profile.agent);
        let cwd_scoped = !matches!(profile.agent, AgentKind::Hermes);

        let mut matched: Vec<&SessionMeta> = list
            .iter()
            .filter(|s| s.agent == target_agent)
            .collect();
        let considered = matched.len();

        if cwd_scoped {
            matched.retain(|s| {
                s.cwd
                    .as_deref()
                    .map(|c| c == profile.cwd)
                    .unwrap_or(false)
            });
        }

        matched.sort_by_key(|s| std::cmp::Reverse(s.last_active_ts));
        matched.truncate(HISTORY_LIMIT);

        let sessions = matched
            .into_iter()
            .map(summarize)
            .collect();

        SessionsResponse {
            profile: profile.name.clone(),
            sessions,
            cwd_scoped,
            considered,
        }
    }
}

fn summarize(m: &SessionMeta) -> SessionSummary {
    SessionSummary {
        id: m.id.clone(),
        sid: strip_prefix(&m.id).to_string(),
        turns: m.turns,
        prompt: m.prompt.clone(),
        model: m.model.clone(),
        last_active_ts: m.last_active_ts,
        started_at_ts: m.started_at_ts,
        is_scripted: m.is_scripted,
    }
}

/// Strip provider prefix so the result can be fed back into
/// `claude --resume`, `codex resume`, `qwen --resume`, `hermes --resume`.
pub fn strip_prefix(id: &str) -> &str {
    for p in ["codex:", "qwen:", "hermes:"] {
        if let Some(s) = id.strip_prefix(p) {
            return s;
        }
    }
    id
}

fn core_agent(a: AgentKind) -> Agent {
    match a {
        AgentKind::Claude => Agent::Claude,
        AgentKind::Codex => Agent::Codex,
        AgentKind::Qwen => Agent::Qwen,
        AgentKind::Hermes => Agent::Hermes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_prefix_handles_each_provider() {
        assert_eq!(strip_prefix("abc-123"), "abc-123"); // claude: no prefix
        assert_eq!(strip_prefix("codex:ulid"), "ulid");
        assert_eq!(strip_prefix("qwen:20250101"), "20250101");
        assert_eq!(strip_prefix("hermes:20251231_abc"), "20251231_abc");
    }
}
