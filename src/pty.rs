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
    /// Resume a specific session id (supplied out-of-band via the `sid`
    /// query parameter).
    ///   claude / qwen / hermes  → `--resume <sid>`
    ///   codex                    → subcommand `resume <sid>`
    Resume,
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
/// `sid` is required when `mode == SpawnMode::Resume`, ignored otherwise.
fn resolve_command(
    agent: AgentKind,
    mode: SpawnMode,
    sid: Option<&str>,
) -> (&'static str, Vec<String>) {
    let empty = || Vec::<String>::new();
    let s = || sid.unwrap_or_default().to_string();
    match (agent, mode) {
        (AgentKind::Claude, SpawnMode::New) => ("claude", empty()),
        (AgentKind::Claude, SpawnMode::Continue) => ("claude", vec!["--continue".into()]),
        (AgentKind::Claude, SpawnMode::Resume) => ("claude", vec!["--resume".into(), s()]),
        (AgentKind::Hermes, SpawnMode::New) => ("hermes", empty()),
        (AgentKind::Hermes, SpawnMode::Continue) => ("hermes", vec!["--continue".into()]),
        (AgentKind::Hermes, SpawnMode::Resume) => ("hermes", vec!["--resume".into(), s()]),
        (AgentKind::Qwen, SpawnMode::New) => ("qwen", empty()),
        (AgentKind::Qwen, SpawnMode::Continue) => ("qwen", vec!["--continue".into()]),
        (AgentKind::Qwen, SpawnMode::Resume) => ("qwen", vec!["--resume".into(), s()]),
        (AgentKind::Codex, SpawnMode::New) => ("codex", empty()),
        (AgentKind::Codex, SpawnMode::Continue) => {
            ("codex", vec!["resume".into(), "--last".into()])
        }
        (AgentKind::Codex, SpawnMode::Resume) => ("codex", vec!["resume".into(), s()]),
    }
}

/// Whether the UI should let users click "launch" on a skill for this
/// agent. Claude is included even though its CLI has no `--skills`
/// flag — we inject `/skill-name` into stdin after spawn instead
/// (see `ws_handler`).
pub fn supports_skill_launch(agent: AgentKind) -> bool {
    matches!(agent, AgentKind::Claude | AgentKind::Hermes)
}

/// Whether this agent accepts `--skills <name>` at the CLI. Only hermes.
/// For claude we use stdin injection; for qwen / codex we do nothing.
fn supports_skill_flag(agent: AgentKind) -> bool {
    matches!(agent, AgentKind::Hermes)
}

pub fn spawn_for_profile(
    p: &Profile,
    cols: u16,
    rows: u16,
    mode: SpawnMode,
    sid: Option<&str>,
    skill: Option<&str>,
) -> anyhow::Result<PtySession> {
    if mode == SpawnMode::Resume && sid.filter(|s| !s.is_empty()).is_none() {
        anyhow::bail!("resume mode requires a non-empty `sid` query parameter");
    }

    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let (program, mut args) = resolve_command(p.agent, mode, sid);

    // Claude defaults to interactive permission prompts on every tool
    // call. For agentdeck we want fewer interruptions — `--permission-mode
    // auto` lets claude's built-in classifier decide which calls still
    // need explicit approval. Applies to new / continue / resume alike,
    // and goes before any positional `[prompt]` arg appended below.
    if matches!(p.agent, AgentKind::Claude) {
        let mut withdef = vec!["--permission-mode".into(), "auto".into()];
        withdef.extend(args);
        args = withdef;
    }

    if let Some(name) = skill.filter(|s| !s.is_empty()) {
        if supports_skill_flag(p.agent) {
            // hermes: native flag
            args.push("--skills".into());
            args.push(name.into());
        } else if matches!(p.agent, AgentKind::Claude) {
            // claude: has no `--skills` flag, but accepts a positional
            // `[prompt]` arg that becomes the first user message. The
            // TUI treats a message starting with `/` as a slash command,
            // so `claude "/pdca"` is equivalent to the user opening
            // claude and immediately typing `/pdca`.
            args.push(format!("/{name}"));
        }
        // qwen / codex: no equivalent, UI keeps the launch button
        // disabled so we never get here for them.
    }

    let mut cmd = CommandBuilder::new(program);
    for a in &args {
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
            resolve_command(AgentKind::Claude, SpawnMode::Continue, None),
            ("claude", vec!["--continue".to_string()])
        );
        assert_eq!(
            resolve_command(AgentKind::Codex, SpawnMode::Continue, None),
            ("codex", vec!["resume".into(), "--last".into()])
        );
        assert_eq!(
            resolve_command(AgentKind::Qwen, SpawnMode::New, None),
            ("qwen", Vec::<String>::new())
        );
    }

    #[test]
    fn resume_flags_per_agent() {
        assert_eq!(
            resolve_command(AgentKind::Claude, SpawnMode::Resume, Some("abc123")),
            ("claude", vec!["--resume".into(), "abc123".into()])
        );
        assert_eq!(
            resolve_command(AgentKind::Codex, SpawnMode::Resume, Some("ulid")),
            ("codex", vec!["resume".into(), "ulid".into()])
        );
        assert_eq!(
            resolve_command(AgentKind::Hermes, SpawnMode::Resume, Some("20260422_x")),
            ("hermes", vec!["--resume".into(), "20260422_x".into()])
        );
    }

    #[test]
    fn skill_launch_support_table() {
        // UI-visible: claude + hermes both get launch buttons.
        assert!(supports_skill_launch(AgentKind::Claude));
        assert!(supports_skill_launch(AgentKind::Hermes));
        assert!(!supports_skill_launch(AgentKind::Qwen));
        assert!(!supports_skill_launch(AgentKind::Codex));
    }

    #[test]
    fn only_hermes_uses_skill_cli_flag() {
        // Internal: only hermes has a real `--skills` flag. Claude's
        // launch path is stdin injection of `/skill-name`.
        assert!(supports_skill_flag(AgentKind::Hermes));
        assert!(!supports_skill_flag(AgentKind::Claude));
        assert!(!supports_skill_flag(AgentKind::Qwen));
        assert!(!supports_skill_flag(AgentKind::Codex));
    }
}
