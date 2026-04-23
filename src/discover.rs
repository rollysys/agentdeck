//! `agentdeck discover` — scan claude session transcripts and suggest
//! new profile entries for cwds where you've actually done meaningful
//! work.
//!
//! We use auditui-core's session indexer, filter to claude agents with
//! at least `--min-turns` turns, group by cwd, and compare against the
//! existing profiles.yaml to skip anything already configured. Output
//! is a YAML snippet printed to stdout (dry-run by default); `--apply`
//! appends it to the config file after backing the original up.
//!
//! File-preserving append: we parse the YAML only to read the existing
//! `cwd:` / `name:` values, then append new entries as plain text. This
//! keeps user comments and formatting intact — serde_yaml round-tripping
//! would drop them.

use crate::profile;
use auditui_core::providers::Agent;
use auditui_core::session;
use chrono::Local;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

const DEFAULT_MIN_TURNS: usize = 10;
/// A single deep conversation in a cwd counts — we don't require
/// multiple visits. Users who want to prune one-off dirs can use
/// `--min-sessions N` explicitly.
const DEFAULT_MIN_SESSIONS: usize = 1;
/// Path prefixes we skip by default. These are overwhelmingly
/// short-lived scratch dirs (temp worktrees, interview sandboxes,
/// etc.); surfacing them as profile candidates just creates noise.
const DEFAULT_EXCLUDED_PREFIXES: &[&str] =
    &["/tmp/", "/private/tmp/", "/var/tmp/", "/var/folders/"];

const HELP: &str = "\
agentdeck discover — suggest new profile entries from claude session history

USAGE:
  agentdeck discover [--min-turns N] [--min-sessions N]
                     [--include-tmp] [--exclude <substr>]... [--apply]

FLAGS:
  --min-turns N     a cwd is eligible only if at least one claude
                    session in it has ≥ N turns (default: 10)
  --min-sessions N  and it must have at least N such sessions (default: 1 —
                    a single deep conversation is enough)
  --include-tmp     don't skip /tmp, /private/tmp, /var/tmp, /var/folders
                    prefixes (default: they are excluded as ephemeral)
  --exclude <substr>
                    skip any cwd whose path contains this substring.
                    Repeatable (e.g. `--exclude /runs/ --exclude /.worktree-`).
  --apply           append the discovered entries to
                    ~/.config/agentdeck/profiles.yaml (backing the
                    original up to <path>.yaml.bak). Without this flag
                    the suggestions are just printed as a preview.
  -h, --help        print this help and exit
";

pub fn run(args: &[String]) -> anyhow::Result<()> {
    let mut apply = false;
    let mut min_turns = DEFAULT_MIN_TURNS;
    let mut min_sessions = DEFAULT_MIN_SESSIONS;
    let mut exclude_tmp = true;
    let mut excludes: Vec<String> = Vec::new();
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--apply" => apply = true,
            "--include-tmp" => exclude_tmp = false,
            "--exclude" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--exclude needs a value"))?;
                excludes.push(v.clone());
            }
            "--min-turns" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--min-turns needs a value"))?;
                min_turns = v.parse().map_err(|e| {
                    anyhow::anyhow!("--min-turns expects a non-negative integer: {e}")
                })?;
            }
            "--min-sessions" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--min-sessions needs a value"))?;
                min_sessions = v.parse().map_err(|e| {
                    anyhow::anyhow!("--min-sessions expects a non-negative integer: {e}")
                })?;
            }
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            other => anyhow::bail!("unknown argument: {other} (try --help)"),
        }
    }

    let config_path = profile::config_path();
    if !config_path.exists() {
        anyhow::bail!(
            "{} does not exist. create the initial yaml (even just `profiles: []`) before running discover.",
            config_path.display()
        );
    }

    let report = profile::load();
    for e in &report.errors {
        eprintln!("warning: {e}");
    }
    let existing_cwds: HashSet<String> =
        report.profiles.iter().map(|p| p.cwd.clone()).collect();
    let mut used_names: HashSet<String> =
        report.profiles.iter().map(|p| p.name.clone()).collect();

    let sessions = session::index_all();
    let scanned = sessions.len();
    let mut by_cwd: HashMap<String, usize> = HashMap::new();
    for s in &sessions {
        if !matches!(s.agent, Agent::Claude) {
            continue;
        }
        if s.turns < min_turns {
            continue;
        }
        if let Some(cwd) = s.cwd.as_deref() {
            *by_cwd.entry(cwd.to_string()).or_insert(0) += 1;
        }
    }

    let is_tmp_path = |cwd: &str| -> bool {
        DEFAULT_EXCLUDED_PREFIXES.iter().any(|p| cwd.starts_with(p))
    };
    let matches_user_exclude = |cwd: &str| -> bool {
        excludes.iter().any(|pat| cwd.contains(pat))
    };

    let mut candidates: Vec<(String, usize)> = by_cwd
        .into_iter()
        .filter(|(cwd, count)| {
            !existing_cwds.contains(cwd)
                && *count >= min_sessions
                && !(exclude_tmp && is_tmp_path(cwd))
                && !matches_user_exclude(cwd)
        })
        .collect();
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    if candidates.is_empty() {
        eprintln!(
            "no new cwds match filters (scanned {} sessions, {} already configured)\n  --min-turns={}, --min-sessions={}, exclude-tmp={}",
            scanned,
            existing_cwds.len(),
            min_turns,
            min_sessions,
            exclude_tmp,
        );
        return Ok(());
    }

    let mut suggestions: Vec<Suggestion> = Vec::new();
    for (cwd, sessions_count) in candidates {
        let base = Path::new(&cwd)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("profile")
            .to_string();
        let mut name = base.clone();
        let mut i = 2;
        while used_names.contains(&name) {
            name = format!("{base}-{i}");
            i += 1;
        }
        used_names.insert(name.clone());
        suggestions.push(Suggestion {
            name,
            cwd,
            sessions_count,
        });
    }

    let exclude_summary = if excludes.is_empty() {
        String::new()
    } else {
        format!(", --exclude=[{}]", excludes.join(","))
    };
    eprintln!(
        "discovered {} new cwd(s) — ≥ {} claude turns AND ≥ {} sessions (scanned {}, {} already configured{}{}):",
        suggestions.len(),
        min_turns,
        min_sessions,
        scanned,
        existing_cwds.len(),
        if exclude_tmp { ", tmp dirs excluded" } else { "" },
        exclude_summary,
    );
    for s in &suggestions {
        eprintln!("  {:<32} {:>4} session(s)  {}", s.name, s.sessions_count, s.cwd);
    }

    let now = Local::now().format("%Y-%m-%d %H:%M");
    let mut snippet = format!(
        "\n# auto-discovered by `agentdeck discover` on {} (min {} turns, min {} sessions)\n",
        now, min_turns, min_sessions,
    );
    for s in &suggestions {
        snippet.push_str(&format!(
            "  - name: {}\n    cwd: {}\n    agent: claude\n    skills: []\n",
            s.name, s.cwd
        ));
    }

    if apply {
        let mut content = fs::read_to_string(&config_path)?;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&snippet);

        let backup = config_path.with_extension("yaml.bak");
        fs::copy(&config_path, &backup)?;
        fs::write(&config_path, content)?;
        eprintln!(
            "\nappended {} profile(s) to {}\n(backup saved to {})",
            suggestions.len(),
            config_path.display(),
            backup.display(),
        );
    } else {
        println!();
        print!("{}", snippet.trim_start());
        eprintln!(
            "\ndry-run. re-run with --apply to append to {}",
            config_path.display()
        );
    }

    Ok(())
}

struct Suggestion {
    name: String,
    cwd: String,
    sessions_count: usize,
}
