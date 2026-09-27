//! Flatpak apps: identified by cgroup scope or the sandbox's `FLATPAK_ID`.

use crate::discovery::process::ProcessInfo;
use crate::discovery::systemd::AppUnit;

pub fn app_id(proc: &ProcessInfo, unit: Option<&AppUnit>) -> Option<String> {
    unit.filter(|u| u.is_flatpak())
        .map(|u| u.app_id.clone())
        .or_else(|| proc.env_flatpak_id.clone())
        .filter(|id| id.contains('.'))
}

pub fn launch_argv(app_id: &str) -> Vec<String> {
    vec!["flatpak".into(), "run".into(), app_id.into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_scope_then_env() {
        let unit = AppUnit {
            launcher: Some("flatpak".into()),
            app_id: "com.spotify.Client".into(),
        };
        let p = ProcessInfo::default();
        assert_eq!(
            app_id(&p, Some(&unit)).as_deref(),
            Some("com.spotify.Client")
        );
        let p = ProcessInfo {
            env_flatpak_id: Some("org.x.Y".into()),
            ..Default::default()
        };
        assert_eq!(app_id(&p, None).as_deref(), Some("org.x.Y"));
        let other = AppUnit {
            launcher: None,
            app_id: "foot".into(),
        };
        assert_eq!(app_id(&ProcessInfo::default(), Some(&other)), None);
    }
}
