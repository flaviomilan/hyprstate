//! Chromium-family web apps (PWAs, `--app=URL`, Omarchy web apps).
//!
//! Chromium names an app window `<browser>-<appname>-<profile>`, where
//! `appname = host + "_" + path` with `/` replaced by `_`. The app shares the
//! browser's PID, so the class is the only reliable identity.

use crate::discovery::desktop::{DesktopEntry, DesktopIndex};

const PREFIXES: &[&str] = &["chrome-", "msedge-", "brave-", "vivaldi-"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAppClass {
    /// `host_/path` with `/` already replaced by `_`, e.g. `web.whatsapp.com__`.
    pub app_name: String,
    pub profile: String,
}

pub fn parse_class(class: &str) -> Option<WebAppClass> {
    let rest = PREFIXES.iter().find_map(|p| class.strip_prefix(p))?;
    let (app_name, profile) = rest.rsplit_once('-')?;
    // Needs a host and the host/path separator.
    if !app_name.contains('_') || !app_name.contains('.') || profile.is_empty() {
        return None;
    }
    Some(WebAppClass {
        app_name: app_name.to_string(),
        profile: profile.to_string(),
    })
}

/// Chromium's class-form app name for a URL (`https://a.b/c/` → `a.b__c_`).
pub fn class_name_for_url(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let rest = rest.split(['?', '#']).next()?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let host = host.rsplit('@').next()?;
    let host = host.split(':').next()?;
    if host.is_empty() {
        return None;
    }
    Some(format!("{host}_{path}").replace('/', "_"))
}

/// Best-effort URL from the class. `initial_title` is Chromium's unmangled app
/// name (`web.whatsapp.com_/`) on first map, which recovers `_` in paths.
pub fn url_for(wa: &WebAppClass, initial_title: &str) -> String {
    if let Some((host, path)) = initial_title.split_once("_/")
        && format!("{host}_/{path}").replace('/', "_") == wa.app_name
    {
        return format!("https://{host}/{path}");
    }
    let (host, path) = wa.app_name.split_once('_').unwrap_or((&wa.app_name, ""));
    format!("https://{host}{}", path.replace('_', "/"))
}

/// First http(s) URL in an argv, including `--app=URL`.
pub fn url_in_argv(argv: &[String]) -> Option<&str> {
    argv.iter().find_map(|a| {
        let a = a.strip_prefix("--app=").unwrap_or(a);
        (a.starts_with("https://") || a.starts_with("http://")).then_some(a)
    })
}

/// Desktop entry that launches this web app.
pub fn find_desktop<'a>(idx: &'a DesktopIndex, wa: &WebAppClass) -> Option<&'a DesktopEntry> {
    idx.entries().iter().find(|e| {
        url_in_argv(&e.exec)
            .and_then(class_name_for_url)
            .is_some_and(|n| n == wa.app_name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::desktop::parse_desktop_file;
    use std::path::Path;

    #[test]
    fn parses_real_pwa_class() {
        let wa = parse_class("chrome-web.whatsapp.com__-Default").unwrap();
        assert_eq!(wa.app_name, "web.whatsapp.com__");
        assert_eq!(wa.profile, "Default");
        assert_eq!(
            url_for(&wa, "web.whatsapp.com_/"),
            "https://web.whatsapp.com/"
        );
        assert_eq!(url_for(&wa, "WhatsApp"), "https://web.whatsapp.com/");
        assert!(parse_class("chromium").is_none());
        assert!(parse_class("chrome-foo").is_none());
    }

    #[test]
    fn hyphenated_hosts_and_paths() {
        let wa = parse_class("chrome-my-site.example.com__app_x-Profile_1").unwrap();
        assert_eq!(wa.app_name, "my-site.example.com__app_x");
        assert_eq!(wa.profile, "Profile_1");
        assert_eq!(
            url_for(&wa, "my-site.example.com_/app_x"),
            "https://my-site.example.com/app_x"
        );
        assert_eq!(
            class_name_for_url("https://my-site.example.com/app_x?q=1").unwrap(),
            "my-site.example.com__app_x"
        );
    }

    #[test]
    fn finds_omarchy_desktop() {
        let e = parse_desktop_file(
            "WhatsApp",
            Path::new("/x"),
            "[Desktop Entry]\nType=Application\nName=WhatsApp\nExec=omarchy-launch-webapp https://web.whatsapp.com/\n",
        )
        .unwrap();
        let idx = DesktopIndex::from_entries(vec![e]);
        let wa = parse_class("chrome-web.whatsapp.com__-Default").unwrap();
        assert_eq!(find_desktop(&idx, &wa).unwrap().id, "WhatsApp");
    }

    #[test]
    fn plain_browser_classes_are_not_web_apps() {
        assert_eq!(parse_class("chrome-foo-Default"), None);
    }

    #[test]
    fn url_without_path_or_host() {
        assert_eq!(
            class_name_for_url("https://example.com").as_deref(),
            Some("example.com__")
        );
        assert_eq!(class_name_for_url("https:///path"), None);
    }
}
