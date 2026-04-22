//! Spawn a coding agent inside a pty, per profile.
//!
//! Deliberately minimal for P2: we look up the command by agent kind
//! (claude/codex/qwen/hermes), chdir to the profile's cwd, merge the
//! profile's env on top of the parent process env, and hand back the
//! pty endpoints. Passing `--model` / skill preloading is P3+: each
//! agent has its own flags and we don't want to lie about behavior
//! by guessing.

use crate::profile::{AgentKind, Profile};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Deserialize;
use std::io::{Read, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpawnMode {
    /// Start a fresh agent session.
    #[default]
    New,
    /// Continue the most recent session in the profile's cwd.
    /// Each agent has slightly different spelling:
    ///   claude / hermes / qwen  → `--continue`
    ///   codex                    → subcommand `resume --last`
    Continue,
}

pub struct PtySession {
    pub master: Box<dyn MasterPty + Send>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    // Keeping the child alive here is defensive: dropping `master`
    // closes the pty and the child will receive SIGHUP and exit on
    // its own, but retaining the handle prevents an unnamed drop
    // from racing teardown.
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
}

/// Returns (command, args) for launching `agent` in `mode`.
fn resolve_command(agent: AgentKind, mode: SpawnMode) -> (&'static str, &'static [&'static str]) {
    match (agent, mode) {
        (AgentKind::Claude, SpawnMode::New) => ("claude", &[]),
        (AgentKind::Claude, SpawnMode::Continue) => ("claude", &["--continue"]),
        (AgentKind::Hermes, SpawnMode::New) => ("hermes", &[]),
        (AgentKind::Hermes, SpawnMode::Continue) => ("hermes", &["--continue"]),
        (AgentKind::Qwen, SpawnMode::New) => ("qwen", &[]),
        (AgentKind::Qwen, SpawnMode::Continue) => ("qwen", &["--continue"]),
        (AgentKind::Codex, SpawnMode::New) => ("codex", &[]),
        (AgentKind::Codex, SpawnMode::Continue) => ("codex", &["resume", "--last"]),
    }
}

pub fn spawn_for_profile(
    p: &Profile,
    cols: u16,
    rows: u16,
    mode: SpawnMode,
) -> anyhow::Result<PtySession> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let (program, args) = resolve_command(p.agent, mode);
    let mut cmd = CommandBuilder::new(program);
    for a in args {
        cmd.arg(a);
    }
    cmd.cwd(&p.cwd);
    for (k, v) in &p.env {
        cmd.env(k, v);
    }

    let child = pair.slave.spawn_command(cmd)?;
    // Drop the slave so the master sees EOF when the child exits.
    drop(pair.slave);

    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    Ok(PtySession {
        master: pair.master,
        reader,
        writer,
        child,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continue_flags_per_agent() {
        assert_eq!(
            resolve_command(AgentKind::Claude, SpawnMode::Continue),
            ("claude", &["--continue"] as &[_])
        );
        assert_eq!(
            resolve_command(AgentKind::Codex, SpawnMode::Continue),
            ("codex", &["resume", "--last"] as &[_])
        );
        assert_eq!(
            resolve_command(AgentKind::Qwen, SpawnMode::New),
            ("qwen", &[] as &[_])
        );
    }
}
