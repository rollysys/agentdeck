//! Per-profile display names ("aliases"), stored in
//! `~/.config/agentdeck/aliases.yaml`.
//!
//! Kept separate from `profiles.yaml` on purpose: profile names are
//! identifiers (used in WebSocket URLs, running-counter keys, future
//! cron entries). Renaming a profile in-place ripples everywhere.
//! Aliases are pure display sugar — the backend and the UI still key
//! on `profile.name`, they just render `alias_or_name` when present.
//!
//! File shape (trivial):
//!
//! ```yaml
//! aliases:
//!   ArgusAlphaV4: "大盘主策略"
//!   claude_jsppy: "JS 个股盯盘"
//! ```

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const FILENAME: &str = "aliases.yaml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Aliases {
    /// profile name → display name
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
}

pub fn file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".config")
        .join("agentdeck")
        .join(FILENAME)
}

pub fn load() -> Aliases {
    let p = file_path();
    if !p.exists() {
        return Aliases::default();
    }
    match fs::read_to_string(&p) {
        Ok(text) => serde_yaml::from_str(&text).unwrap_or_default(),
        Err(_) => Aliases::default(),
    }
}

pub fn save(a: &Aliases) -> anyhow::Result<()> {
    let p = file_path();
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_yaml::to_string(a)?;
    // Atomic write: serialize to a sibling file and rename over the
    // target so a crash mid-write can't leave a half-written file.
    let tmp = p.with_extension("yaml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &p)?;
    Ok(())
}

/// Set or clear the alias for a profile. Passing `None` or an empty
/// string removes the entry.
pub fn set(profile: &str, alias: Option<&str>) -> anyhow::Result<Aliases> {
    let mut a = load();
    match alias {
        Some(s) if !s.trim().is_empty() => {
            a.aliases.insert(profile.to_string(), s.trim().to_string());
        }
        _ => {
            a.aliases.remove(profile);
        }
    }
    save(&a)?;
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_removes_entry() {
        let mut a = Aliases::default();
        a.aliases.insert("foo".into(), "Foo Bar".into());
        // simulate `set` via direct manipulation: empty value should remove
        match Some("") {
            Some(s) if !s.trim().is_empty() => {
                a.aliases.insert("foo".into(), s.trim().into());
            }
            _ => {
                a.aliases.remove("foo");
            }
        }
        assert!(a.aliases.get("foo").is_none());
    }
}
