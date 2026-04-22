//! Profile loading from `~/.config/agentdeck/profiles.yaml`.
//!
//! Deliberate non-goals for P1: we do not spawn anything, we do not write to
//! the config, we do not auto-generate a template. If the file is missing or
//! malformed, the API returns an empty profile list plus human-readable
//! error strings so the UI can say "fix your YAML here".

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Qwen,
    Hermes,
}

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub name: String,
    pub cwd: String,
    pub agent: AgentKind,
    pub model: Option<String>,
    pub skills: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadReport {
    pub profiles: Vec<Profile>,
    pub errors: Vec<String>,
    pub config_path: String,
    pub config_exists: bool,
}

#[derive(Debug, Deserialize)]
struct ProfilesFile {
    profiles: Vec<RawProfile>,
}

#[derive(Debug, Deserialize)]
struct RawProfile {
    name: String,
    cwd: String,
    agent: AgentKind,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".config")
        .join("agentdeck")
        .join("profiles.yaml")
}

pub fn load() -> LoadReport {
    let path = config_path();
    let config_path_str = path.display().to_string();

    if !path.exists() {
        return LoadReport {
            profiles: vec![],
            errors: vec![],
            config_path: config_path_str,
            config_exists: false,
        };
    }

    let text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return LoadReport {
                profiles: vec![],
                errors: vec![format!("read {}: {e}", path.display())],
                config_path: config_path_str,
                config_exists: true,
            };
        }
    };

    let parsed: ProfilesFile = match serde_yaml::from_str(&text) {
        Ok(p) => p,
        Err(e) => {
            return LoadReport {
                profiles: vec![],
                errors: vec![format!("parse {}: {e}", path.display())],
                config_path: config_path_str,
                config_exists: true,
            };
        }
    };

    let mut profiles = Vec::with_capacity(parsed.profiles.len());
    let mut errors = Vec::new();
    let mut seen_names = BTreeMap::new();

    for raw in parsed.profiles {
        match validate(raw, &mut seen_names) {
            Ok(p) => profiles.push(p),
            Err(e) => errors.push(e),
        }
    }

    LoadReport {
        profiles,
        errors,
        config_path: config_path_str,
        config_exists: true,
    }
}

fn validate(raw: RawProfile, seen: &mut BTreeMap<String, ()>) -> Result<Profile, String> {
    if raw.name.trim().is_empty() {
        return Err("profile with empty name".into());
    }
    if seen.contains_key(&raw.name) {
        return Err(format!("duplicate profile name: {}", raw.name));
    }
    seen.insert(raw.name.clone(), ());

    let cwd = expand_tilde(&raw.cwd);
    if !Path::new(&cwd).is_absolute() {
        return Err(format!(
            "profile {:?}: cwd {:?} is not absolute (tilde-expansion done on paths starting with ~/)",
            raw.name, raw.cwd
        ));
    }

    Ok(Profile {
        name: raw.name,
        cwd,
        agent: raw.agent,
        model: raw.model,
        skills: raw.skills,
        env: raw.env,
    })
}

fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).display().to_string();
        }
    }
    if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.display().to_string();
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(name: &str, cwd: &str, agent: AgentKind) -> RawProfile {
        RawProfile {
            name: name.into(),
            cwd: cwd.into(),
            agent,
            model: None,
            skills: vec![],
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn relative_cwd_rejected() {
        let mut seen = BTreeMap::new();
        let err = validate(raw("p", "relative/path", AgentKind::Claude), &mut seen).unwrap_err();
        assert!(err.contains("not absolute"), "got: {err}");
    }

    #[test]
    fn empty_name_rejected() {
        let mut seen = BTreeMap::new();
        let err = validate(raw("", "/tmp", AgentKind::Claude), &mut seen).unwrap_err();
        assert!(err.contains("empty name"), "got: {err}");
    }

    #[test]
    fn duplicate_name_rejected() {
        let mut seen = BTreeMap::new();
        validate(raw("dup", "/tmp", AgentKind::Claude), &mut seen).unwrap();
        let err = validate(raw("dup", "/tmp", AgentKind::Claude), &mut seen).unwrap_err();
        assert!(err.contains("duplicate"), "got: {err}");
    }

    #[test]
    fn tilde_slash_expands() {
        let home = dirs::home_dir().unwrap().display().to_string();
        let out = expand_tilde("~/foo/bar");
        assert!(out.starts_with(&home));
        assert!(out.ends_with("/foo/bar"));
    }

    #[test]
    fn bare_tilde_expands() {
        let home = dirs::home_dir().unwrap().display().to_string();
        assert_eq!(expand_tilde("~"), home);
    }

    #[test]
    fn non_tilde_passthrough() {
        assert_eq!(expand_tilde("/abs/path"), "/abs/path");
        assert_eq!(expand_tilde("rel/path"), "rel/path");
    }
}
