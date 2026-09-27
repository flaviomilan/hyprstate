//! Windows the user never wants captured or restored.

use serde::{Deserialize, Serialize};

/// Applications excluded even without configuration.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "1password",
    "keepassxc",
    "org.keepassxc.keepassxc",
    "bitwarden",
    "com.bitwarden.desktop",
    "proton-pass",
    "enpass",
    "seahorse",
    "org.gnome.seahorse.application",
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExcludeRule {
    pub class: Option<String>,
    pub initial_class: Option<String>,
    pub title: Option<String>,
}

/// What an exclusion is checked against.
pub struct Subject<'a> {
    pub class: &'a str,
    pub initial_class: &'a str,
    pub title: &'a str,
    /// Desktop id, flatpak id, executable basename…
    pub app_ids: &'a [String],
}

#[derive(Debug, Clone, Default)]
pub struct Exclusions {
    names: Vec<String>,
    rules: Vec<ExcludeRule>,
}

impl Exclusions {
    pub fn new(names: &[String], rules: &[ExcludeRule], use_defaults: bool) -> Self {
        let mut all: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
        if use_defaults {
            all.extend(DEFAULT_EXCLUDES.iter().map(|s| s.to_string()));
        }
        Self {
            names: all,
            rules: rules.to_vec(),
        }
    }

    /// Returns the reason a window is excluded, if it is.
    pub fn check(&self, s: &Subject) -> Option<String> {
        let candidates = [s.class, s.initial_class]
            .into_iter()
            .chain(s.app_ids.iter().map(String::as_str))
            .map(|c| c.to_ascii_lowercase());
        for c in candidates {
            if let Some(n) = self
                .names
                .iter()
                .find(|n| **n == c || c.ends_with(&format!(".{n}")))
            {
                return Some(format!("excluded by name '{n}'"));
            }
        }
        let eq = |want: &Option<String>, got: &str| {
            want.as_ref().is_none_or(|w| w.eq_ignore_ascii_case(got))
        };
        for r in &self.rules {
            let any_set = r.class.is_some() || r.initial_class.is_some() || r.title.is_some();
            if any_set
                && eq(&r.class, s.class)
                && eq(&r.initial_class, s.initial_class)
                && eq(&r.title, s.title)
            {
                return Some(format!("excluded by rule {r:?}"));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subj<'a>(class: &'a str, ids: &'a [String]) -> Subject<'a> {
        Subject {
            class,
            initial_class: class,
            title: "",
            app_ids: ids,
        }
    }

    #[test]
    fn matches_names_rules_and_defaults() {
        let ex = Exclusions::new(
            &["Signal".into()],
            &[ExcludeRule {
                class: Some("secret-app".into()),
                ..Default::default()
            }],
            true,
        );
        assert!(ex.check(&subj("org.keepassxc.KeePassXC", &[])).is_some());
        assert!(ex.check(&subj("signal", &[])).is_some());
        assert!(
            ex.check(&subj("x", &["com.bitwarden.desktop".into()]))
                .is_some()
        );
        assert!(ex.check(&subj("Secret-App", &[])).is_some());
        assert!(ex.check(&subj("foot", &[])).is_none());
        assert!(
            Exclusions::new(&[], &[], false)
                .check(&subj("1password", &[]))
                .is_none()
        );
    }
}
