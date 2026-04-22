//! Per-agent skill discovery.
//!
//! Each supported agent keeps its skills under a well-known directory in
//! `$HOME`. We scan that directory for top-level subdirs and, if the
//! subdir contains a `SKILL.md` (or `AGENTS.md`), pick up a one-line
//! description from the YAML front-matter.
//!
//! Why we don't just use `auditui_core::skills::list()`: upstream only
//! scans claude / codex / qwen and doesn't know about hermes'
//! `~/.hermes/skills/` yet. Rather than block P4 on a core patch, we do
//! the scan locally. When core grows hermes support we can swap this
//! out.

use crate::profile::{AgentKind, Profile};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    /// true when the skill name is listed in the profile's `skills:`
    /// field — the UI floats these to the top.
    pub pinned: bool,
}

#[derive(Debug, Serialize)]
pub struct SkillsResponse {
    pub profile: String,
    pub agent: AgentKind,
    /// Whether this agent's CLI accepts `--skills <name>` for preload.
    /// Only claude and hermes do; for qwen / codex the UI disables
    /// launch buttons but still shows the list so you know what's
    /// installed.
    pub preload_supported: bool,
    pub skills_dir: String,
    pub skills: Vec<SkillInfo>,
    /// Skill names that appear in profile.skills but no matching dir
    /// was found on disk — useful to surface typos / renamed skills.
    pub missing_pinned: Vec<String>,
}

pub fn skills_dir_for(agent: AgentKind) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let rel = match agent {
        AgentKind::Claude => ".claude/skills",
        AgentKind::Codex => ".codex/skills",
        AgentKind::Qwen => ".qwen/skills",
        AgentKind::Hermes => ".hermes/skills",
    };
    Some(home.join(rel))
}

pub fn for_profile(profile: &Profile) -> SkillsResponse {
    let dir = skills_dir_for(profile.agent);
    let preload = crate::pty::supports_skill_launch(profile.agent);
    let dir_str = dir
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let mut pinned_set: std::collections::BTreeSet<&str> = profile
        .skills
        .iter()
        .map(|s| s.as_str())
        .collect();

    let mut skills = match dir.as_deref().filter(|p| p.is_dir()) {
        Some(d) => scan(d, &pinned_set),
        None => Vec::new(),
    };

    // pinned + name stable for UX
    skills.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| a.name.cmp(&b.name))
    });

    // Report pinned names that didn't resolve to an on-disk skill dir.
    for s in &skills {
        pinned_set.remove(s.name.as_str());
    }
    let missing_pinned = pinned_set.into_iter().map(|s| s.to_string()).collect();

    SkillsResponse {
        profile: profile.name.clone(),
        agent: profile.agent,
        preload_supported: preload,
        skills_dir: dir_str,
        skills,
        missing_pinned,
    }
}

fn scan(dir: &Path, pinned: &std::collections::BTreeSet<&str>) -> Vec<SkillInfo> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let name = match p.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // Skip hidden dirs (e.g. `.git` under a VCS'd skills dir)
        if name.starts_with('.') {
            continue;
        }
        let description = description_of(&p);
        let is_pinned = pinned.contains(name.as_str());
        out.push(SkillInfo {
            name,
            description,
            pinned: is_pinned,
        });
    }
    out
}

fn description_of(skill_dir: &Path) -> String {
    // Try SKILL.md first, then AGENTS.md. Parse front-matter
    // `description:` line if present.
    for candidate in ["SKILL.md", "AGENTS.md"] {
        let p = skill_dir.join(candidate);
        if !p.is_file() {
            continue;
        }
        let Ok(text) = fs::read_to_string(&p) else {
            continue;
        };
        if let Some(d) = parse_front_matter_description(&text) {
            return d;
        }
    }
    String::new()
}

fn parse_front_matter_description(text: &str) -> Option<String> {
    let stripped = text.strip_prefix("---\n")?;
    let end = stripped.find("\n---")?;
    let front = &stripped[..end];
    for line in front.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("description:") {
            let rest = rest.trim().trim_matches('"').trim_matches('\'');
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_front_matter_description() {
        let md = "---\nname: foo\ndescription: \"quick helper for X\"\n---\n\n# body\n";
        assert_eq!(
            parse_front_matter_description(md).as_deref(),
            Some("quick helper for X")
        );
    }

    #[test]
    fn missing_front_matter_returns_none() {
        assert!(parse_front_matter_description("# just a heading\n").is_none());
    }

    #[test]
    fn skills_dir_resolves_for_each_agent() {
        for a in [
            AgentKind::Claude,
            AgentKind::Codex,
            AgentKind::Qwen,
            AgentKind::Hermes,
        ] {
            let p = skills_dir_for(a).expect("home dir should resolve");
            let s = p.display().to_string();
            assert!(s.ends_with("skills"), "got: {s}");
        }
    }
}
