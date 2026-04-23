//! Read `<profile.cwd>/.env` and surface the important bits to the UI.
//!
//! - Parses dotenv style: `KEY=value`, `export KEY=value`, with optional
//!   surrounding single or double quotes on the value. Skips blanks and
//!   `#`-prefixed comments.
//! - Never rewrites or touches the file — agentdeck treats `.env` as
//!   read-only, like transcript files.
//! - Masks values whose *key name* looks credential-shaped
//!   (contains `secret`, `token`, `password`, `pwd`, `api_key`,
//!   `apikey`, or `access_key`). Non-credential names (e.g. `APP_ID`,
//!   `BASE_URL`, `FEISHU_APP_ID`) are shown verbatim — those are IDs
//!   and endpoints you want at a glance, not passwords.

use crate::profile::Profile;
use serde::Serialize;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize)]
pub struct EnvResponse {
    pub profile: String,
    pub env_path: String,
    pub exists: bool,
    pub parse_error: Option<String>,
    pub entries: Vec<EnvEntry>,
}

#[derive(Debug, Serialize)]
pub struct EnvEntry {
    pub key: String,
    /// Value after optional masking. Always safe to render in the UI.
    pub value: String,
    /// True when the name matched a secret-shaped keyword and the value
    /// was replaced with a `<prefix>…<suffix>` preview.
    pub masked: bool,
}

pub fn for_profile(profile: &Profile) -> EnvResponse {
    let env_path = PathBuf::from(&profile.cwd).join(".env");
    let env_path_str = env_path.display().to_string();

    if !env_path.exists() {
        return EnvResponse {
            profile: profile.name.clone(),
            env_path: env_path_str,
            exists: false,
            parse_error: None,
            entries: vec![],
        };
    }

    let text = match fs::read_to_string(&env_path) {
        Ok(s) => s,
        Err(e) => {
            return EnvResponse {
                profile: profile.name.clone(),
                parse_error: Some(format!("read {env_path_str}: {e}")),
                env_path: env_path_str,
                exists: true,
                entries: vec![],
            };
        }
    };

    let entries = parse(&text)
        .into_iter()
        .map(|(k, v)| {
            let masked = should_mask(&k);
            EnvEntry {
                value: if masked { mask_value(&v) } else { v },
                key: k,
                masked,
            }
        })
        .collect();

    EnvResponse {
        profile: profile.name.clone(),
        env_path: env_path_str,
        exists: true,
        parse_error: None,
        entries,
    }
}

fn parse(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Optional leading `export `
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some(eq) = line.find('=') else {
            continue;
        };
        let key = line[..eq].trim();
        if key.is_empty() {
            continue;
        }
        let val = strip_inline_comment(line[eq + 1..].trim());
        let val = strip_quotes(val);
        out.push((key.to_string(), val.to_string()));
    }
    out
}

fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return &s[1..s.len() - 1];
    }
    s
}

/// Drop trailing ` # comment` from an unquoted value. Doesn't touch
/// inside-quote `#` characters.
fn strip_inline_comment(s: &str) -> &str {
    if s.starts_with('"') || s.starts_with('\'') {
        return s;
    }
    match s.find(" #").or_else(|| s.find("\t#")) {
        Some(i) => s[..i].trim_end(),
        None => s,
    }
}

const SECRET_SUBSTRINGS: &[&str] = &[
    "secret",
    "token",
    "password",
    "pwd",
    "api_key",
    "apikey",
    "access_key",
    "private_key",
];

fn should_mask(key: &str) -> bool {
    let lo = key.to_ascii_lowercase();
    SECRET_SUBSTRINGS.iter().any(|w| lo.contains(w))
}

fn mask_value(v: &str) -> String {
    if v.is_empty() {
        return String::new();
    }
    if v.chars().count() <= 8 {
        return "****".into();
    }
    let chars: Vec<char> = v.chars().collect();
    let head: String = chars.iter().take(4).collect();
    let tail: String = chars.iter().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_lines() {
        let p = parse("A=1\nB=2\n# comment\n\n  C = hi\n");
        assert_eq!(p, vec![
            ("A".into(), "1".into()),
            ("B".into(), "2".into()),
            ("C".into(), "hi".into()),
        ]);
    }

    #[test]
    fn strips_quotes_and_export() {
        let p = parse(r#"export BASE="https://api.example.com"
APP_ID='cli_abc123'
"#);
        assert_eq!(p, vec![
            ("BASE".into(), "https://api.example.com".into()),
            ("APP_ID".into(), "cli_abc123".into()),
        ]);
    }

    #[test]
    fn strips_inline_comment() {
        let p = parse("X=foo # tail\nY=bar\n");
        assert_eq!(p, vec![
            ("X".into(), "foo".into()),
            ("Y".into(), "bar".into()),
        ]);
    }

    #[test]
    fn masks_credential_names_only() {
        assert!(should_mask("FEISHU_APP_SECRET"));
        assert!(should_mask("API_KEY"));
        assert!(should_mask("MY_ACCESS_TOKEN"));
        assert!(should_mask("db_password"));
        assert!(!should_mask("APP_ID"));
        assert!(!should_mask("FEISHU_APP_ID"));
        assert!(!should_mask("BASE_URL"));
        assert!(!should_mask("SOMETHING_IDENTIFIER"));
    }

    #[test]
    fn mask_preserves_ends() {
        assert_eq!(mask_value(""), "");
        assert_eq!(mask_value("short"), "****");
        assert_eq!(mask_value("abcdefghij"), "abcd…ghij");
        assert_eq!(mask_value("a1b2c3d4e5f6g7h8"), "a1b2…g7h8");
    }
}
