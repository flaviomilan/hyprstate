//! Keeps credentials out of snapshots.
//!
//! Command lines are the only free-form process data hyprstate persists, so
//! every argv passes through [`sanitize`] before it is stored.

use serde::{Deserialize, Serialize};

pub const REDACTED: &str = "<redacted>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SensitiveArgs {
    /// Replace sensitive arguments with `<redacted>`; the command is then not
    /// relaunchable without a user override.
    #[default]
    Redact,
    /// Drop the whole command line.
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sanitized {
    Clean(Vec<String>),
    Redacted(Vec<String>),
    Rejected,
}

const SENSITIVE_WORDS: &[&str] = &[
    "password",
    "passwd",
    "pass",
    "pwd",
    "token",
    "secret",
    "apikey",
    "api-key",
    "api_key",
    "auth",
    "authorization",
    "credential",
    "credentials",
    "cookie",
    "session",
    "private-key",
    "access-key",
    "access_key",
    "client-secret",
    "client_secret",
    "bearer",
];

const SENSITIVE_QUERY_KEYS: &[&str] = &[
    "token",
    "access_token",
    "id_token",
    "refresh_token",
    "key",
    "api_key",
    "apikey",
    "auth",
    "code",
    "session",
    "sessionid",
    "sig",
    "signature",
    "secret",
    "password",
    "pass",
];

fn is_sensitive_name(name: &str) -> bool {
    let n = name.trim_start_matches('-').to_ascii_lowercase();
    // Suffix only: `--db-password` is a secret, `--password-store=…` is not.
    SENSITIVE_WORDS
        .iter()
        .any(|w| n == *w || n.ends_with(&format!("-{w}")) || n.ends_with(&format!("_{w}")))
}

/// URL with `user:pass@` or with a sensitive query parameter.
fn url_is_sensitive(arg: &str) -> bool {
    let Some((_, rest)) = arg.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return true;
    }
    let Some((_, query)) = rest.split_once('?') else {
        return false;
    };
    let query = query.split('#').next().unwrap_or("");
    query.split('&').any(|kv| {
        let k = kv.split('=').next().unwrap_or("").to_ascii_lowercase();
        SENSITIVE_QUERY_KEYS.contains(&k.as_str())
    })
}

/// Long opaque strings (JWTs, API keys, hex secrets). Paths and URLs are exempt.
fn looks_like_secret(arg: &str) -> bool {
    if arg.len() < 32 || arg.contains('/') || arg.contains(' ') {
        return false;
    }
    let charset_ok = arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '=' | '~'));
    if !charset_ok {
        return false;
    }
    let has_digit = arg.chars().any(|c| c.is_ascii_digit());
    let has_upper = arg.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = arg.chars().any(|c| c.is_ascii_lowercase());
    let classes = has_digit as u8 + has_upper as u8 + has_lower as u8;
    // hex secrets are 2 classes; plain words rarely reach 32 chars without separators
    classes >= 2 && shannon_entropy(arg) > 3.5
}

fn shannon_entropy(s: &str) -> f64 {
    let mut counts = [0usize; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let len = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

pub fn sanitize(argv: &[String], mode: SensitiveArgs) -> Sanitized {
    let mut out = Vec::with_capacity(argv.len());
    let mut redacted = false;
    let mut redact_next = false;
    for arg in argv {
        if redact_next {
            redact_next = false;
            out.push(REDACTED.to_string());
            redacted = true;
            continue;
        }
        if arg.starts_with('-') {
            if let Some((name, _)) = arg.split_once('=') {
                if is_sensitive_name(name) {
                    out.push(format!("{name}={REDACTED}"));
                    redacted = true;
                    continue;
                }
            } else if is_sensitive_name(arg) {
                out.push(arg.clone());
                redact_next = true;
                continue;
            }
        } else if let Some((name, _)) = arg.split_once('=')
            && !name.contains('/')
            && !name.contains(':')
            && is_sensitive_name(name)
        {
            out.push(format!("{name}={REDACTED}"));
            redacted = true;
            continue;
        }
        if url_is_sensitive(arg) || looks_like_secret(arg) {
            out.push(REDACTED.to_string());
            redacted = true;
            continue;
        }
        out.push(arg.clone());
    }
    match (redacted, mode) {
        (false, _) => Sanitized::Clean(out),
        (true, SensitiveArgs::Redact) => Sanitized::Redacted(out),
        (true, SensitiveArgs::Reject) => Sanitized::Rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn keeps_ordinary_commands() {
        let argv = v(&[
            "/usr/lib/chromium/chromium",
            "--ozone-platform=wayland",
            "--password-store=gnome-libsecret",
            "--app=https://web.whatsapp.com/",
            "/home/me/projects/some-very-long-directory-name-here",
        ]);
        assert_eq!(
            sanitize(&argv, SensitiveArgs::Redact),
            Sanitized::Clean(argv)
        );
    }

    #[test]
    fn redacts_flags_env_urls_and_blobs() {
        let argv = v(&[
            "app",
            "--token",
            "abc",
            "--api-key=xyz",
            "DB_PASSWORD=hunter2",
            "https://user:pw@example.com/x",
            "https://example.com/cb?code=123&state=a",
            "eyJhbGciOiJIUzI1NiJ9eyJzdWIiOiIxMjM0NTY3ODkwIn0",
        ]);
        let Sanitized::Redacted(out) = sanitize(&argv, SensitiveArgs::Redact) else {
            panic!()
        };
        assert_eq!(
            out,
            v(&[
                "app",
                "--token",
                REDACTED,
                "--api-key=<redacted>",
                "DB_PASSWORD=<redacted>",
                REDACTED,
                REDACTED,
                REDACTED,
            ])
        );
        assert_eq!(sanitize(&argv, SensitiveArgs::Reject), Sanitized::Rejected);
    }
}
