//! systemd user units following the XDG naming convention:
//! `app[-<launcher>]-<ApplicationID>[@<RANDOM>].service` and
//! `app[-<launcher>]-<ApplicationID>-<RANDOM>.scope`, where `-` inside
//! components is escaped as `\x2d`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUnit {
    pub launcher: Option<String>,
    /// Desktop entry id without `.desktop`, or a flatpak app id.
    pub app_id: String,
}

impl AppUnit {
    pub fn is_flatpak(&self) -> bool {
        self.launcher.as_deref() == Some("flatpak")
    }
}

/// Parses the last `app-*.scope|service` component of a cgroup path.
pub fn parse_cgroup(cgroup: &str) -> Option<AppUnit> {
    let unit = cgroup
        .rsplit('/')
        .find(|c| c.starts_with("app-") && !c.ends_with(".slice"))?;
    parse_unit(unit)
}

pub fn parse_unit(unit: &str) -> Option<AppUnit> {
    let body = unit.strip_prefix("app-")?;
    let (body, is_scope) = if let Some(b) = body.strip_suffix(".scope") {
        (b, true)
    } else {
        (body.strip_suffix(".service")?, false)
    };
    let mut parts: Vec<&str> = if is_scope {
        body.split('-').collect()
    } else {
        let body = body.split('@').next()?;
        body.split('-').collect()
    };
    if is_scope {
        // Scopes always end in a random suffix.
        if parts.len() < 2 {
            return None;
        }
        parts.pop();
    }
    let (launcher, id) = match parts.as_slice() {
        [id] => (None, *id),
        [launcher, id] => (Some(unescape(launcher)), *id),
        _ => return None,
    };
    let app_id = unescape(id);
    (!app_id.is_empty()).then_some(AppUnit { launcher, app_id })
}

/// systemd `\xNN` unescaping.
pub fn unescape(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // Bytes, not a str slice: `\x` may be followed by a multibyte char.
        let hex = (b[i] == b'\\' && i + 4 <= b.len() && b[i + 1] == b'x')
            .then(|| std::str::from_utf8(&b[i + 2..i + 4]).ok())
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        if let Some(v) = hex {
            out.push(v);
            i += 4;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "/user.slice/user-1000.slice/user@1000.service/app.slice/";

    #[test]
    fn parses_real_scopes() {
        assert_eq!(
            parse_cgroup(&format!("{BASE}app-org.chromium.Chromium-5237.scope")),
            Some(AppUnit {
                launcher: None,
                app_id: "org.chromium.Chromium".into()
            })
        );
        assert_eq!(
            parse_cgroup(&format!(
                r"{BASE}app-graphical.slice/app-Hyprland-xdg\x2dterminal\x2dexec-4c0069b2.scope"
            )),
            Some(AppUnit {
                launcher: Some("Hyprland".into()),
                app_id: "xdg-terminal-exec".into()
            })
        );
        let fp = parse_unit("app-flatpak-com.spotify.Client-1234.scope").unwrap();
        assert!(fp.is_flatpak());
        assert_eq!(fp.app_id, "com.spotify.Client");
        assert_eq!(
            parse_unit("app-gnome-firefox@abc123.service"),
            Some(AppUnit {
                launcher: Some("gnome".into()),
                app_id: "firefox".into()
            })
        );
        assert_eq!(
            parse_cgroup("/user.slice/user-1000.slice/session-2.scope"),
            None
        );
        assert_eq!(unescape("a\\xé-b"), "a\\xé-b");
    }
}
