//! `~/.config/hyprstate/config.toml`

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::policy::exclusions::{ExcludeRule, Exclusions};
use crate::policy::security::SensitiveArgs;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub policy: PolicyConfig,
    pub exclude: Vec<ExcludeRule>,
    pub windows: Vec<WindowOverride>,
    pub security: SecurityConfig,
    pub restore: RestoreConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyConfig {
    pub exclude: Vec<String>,
    pub default_excludes: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            exclude: Vec::new(),
            default_excludes: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    pub sensitive_args: SensitiveArgs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RestoreConfig {
    /// Overall seconds to wait for launched windows.
    pub timeout: u64,
    /// Command prefix for every launch, e.g. `["uwsm", "app", "--"]`.
    pub launch_wrapper: Vec<String>,
}

impl Default for RestoreConfig {
    fn default() -> Self {
        Self {
            timeout: 30,
            launch_wrapper: Vec::new(),
        }
    }
}

/// `[[windows]] match.class = "foo"  command = "foo --flag"`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowOverride {
    #[serde(rename = "match")]
    pub matcher: OverrideMatch,
    pub command: String,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverrideMatch {
    pub class: Option<String>,
    pub initial_class: Option<String>,
    pub title_contains: Option<String>,
}

impl WindowOverride {
    pub fn matches(&self, class: &str, initial_class: &str, title: &str) -> bool {
        let m = &self.matcher;
        if m.class.is_none() && m.initial_class.is_none() && m.title_contains.is_none() {
            return false;
        }
        m.class
            .as_ref()
            .is_none_or(|c| c.eq_ignore_ascii_case(class))
            && m.initial_class
                .as_ref()
                .is_none_or(|c| c.eq_ignore_ascii_case(initial_class))
            && m.title_contains
                .as_ref()
                .is_none_or(|t| title.contains(t.as_str()))
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("hyprstate").join("config.toml"))
    }

    /// Missing file → defaults. Malformed file → error (never silently ignore
    /// a user's exclusion list).
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => match Self::default_path() {
                Some(p) if p.exists() => p,
                _ => return Ok(Self::default()),
            },
        };
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn exclusions(&self) -> Exclusions {
        Exclusions::new(
            &self.policy.exclude,
            &self.exclude,
            self.policy.default_excludes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documented_example() {
        let cfg: Config = toml::from_str(
            r#"
            [policy]
            exclude = ["1password", "keepassxc"]

            [[exclude]]
            class = "org.keepassxc.KeePassXC"

            [[windows]]
            match.class = "foo"
            command = "foo --some-flag"

            [security]
            sensitive_args = "reject"

            [restore]
            timeout = 45
            launch_wrapper = ["uwsm", "app", "--"]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.policy.exclude.len(), 2);
        assert!(cfg.policy.default_excludes);
        assert_eq!(
            cfg.exclude[0].class.as_deref(),
            Some("org.keepassxc.KeePassXC")
        );
        assert!(cfg.windows[0].matches("Foo", "foo", ""));
        assert_eq!(cfg.security.sensitive_args, SensitiveArgs::Reject);
        assert_eq!(cfg.restore.timeout, 45);
    }

    #[test]
    fn rejects_typos() {
        assert!(toml::from_str::<Config>("[polcy]\nexclude=[]").is_err());
    }
}
