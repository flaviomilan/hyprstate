//! Window → application. Pure: all I/O happens before (`process`, `desktop`).
//!
//! The same function resolves snapshot windows at capture time and live
//! windows at restore time, so the resulting [`AppIdentity`] is directly
//! comparable across a reboot.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::discovery::desktop::{DesktopEntry, DesktopIndex};
use crate::discovery::process::ProcessInfo;
use crate::discovery::{flatpak, systemd, webapp};
use crate::policy::security::{self, Sanitized, SensitiveArgs};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppIdentity {
    Desktop {
        id: String,
    },
    Flatpak {
        app_id: String,
    },
    WebApp {
        url: String,
    },
    Executable {
        path: PathBuf,
    },
    /// Nothing better known; identity is the window's initial class.
    Class {
        class: String,
    },
}

impl fmt::Display for AppIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Desktop { id } => write!(f, "desktop:{id}"),
            Self::Flatpak { app_id } => write!(f, "flatpak:{app_id}"),
            Self::WebApp { url } => write!(f, "webapp:{url}"),
            Self::Executable { path } => write!(f, "exe:{}", path.display()),
            Self::Class { class } => write!(f, "class:{class}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        })
    }
}

/// Which signal produced the identity (shown by `inspect --windows`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Flatpak,
    WebAppDesktop,
    WebAppClass,
    LaunchedDesktopFile,
    SystemdScope,
    StartupWmClass,
    DesktopIdIsClass,
    ExecutableDesktop,
    Cmdline,
    ClassOnly,
    /// Declared in a workset file.
    Workset,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Flatpak => "flatpak",
            Self::WebAppDesktop => "web app + desktop entry",
            Self::WebAppClass => "web app class",
            Self::LaunchedDesktopFile => "GIO launched desktop file",
            Self::SystemdScope => "systemd scope + desktop entry",
            Self::StartupWmClass => "StartupWMClass",
            Self::DesktopIdIsClass => "desktop id = class",
            Self::ExecutableDesktop => "executable + desktop entry",
            Self::Cmdline => "command line",
            Self::ClassOnly => "class only",
            Self::Workset => "workset entry",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchVia {
    Desktop,
    Flatpak,
    WebApp,
    Cmdline,
    Override,
    Workset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    pub via: LaunchVia,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppResolution {
    pub identity: AppIdentity,
    pub confidence: Confidence,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<LaunchSpec>,
    /// Why `launch` is missing, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_problem: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}

/// Window properties resolution looks at.
pub struct WindowFacts<'a> {
    pub class: &'a str,
    pub initial_class: &'a str,
    pub initial_title: &'a str,
}

/// Environment facts resolution depends on, gathered once by the caller.
#[derive(Debug, Clone, Default)]
pub struct ResolveEnv {
    pub sensitive_args: SensitiveArgs,
    /// Path of `omarchy-launch-webapp`, if installed.
    pub omarchy_webapp: Option<PathBuf>,
}

pub fn resolve(
    w: &WindowFacts,
    proc: &ProcessInfo,
    idx: &DesktopIndex,
    env: &ResolveEnv,
) -> AppResolution {
    let unit = proc.cgroup.as_deref().and_then(systemd::parse_cgroup);
    let cwd = useful_cwd(proc);
    let base = |identity, confidence, source| AppResolution {
        identity,
        confidence,
        source,
        launch: None,
        launch_problem: None,
        desktop_entry: None,
        executable: proc.exe.clone(),
        cwd: cwd.clone(),
    };
    let from_desktop = |e: &DesktopEntry, confidence, source| AppResolution {
        launch: Some(LaunchSpec {
            argv: e.exec.clone(),
            cwd: cwd.clone(),
            via: LaunchVia::Desktop,
        }),
        desktop_entry: Some(e.id.clone()),
        ..base(
            AppIdentity::Desktop { id: e.id.clone() },
            confidence,
            source,
        )
    };

    if let Some(id) = flatpak::app_id(proc, unit.as_ref()) {
        return AppResolution {
            launch: Some(LaunchSpec {
                argv: flatpak::launch_argv(&id),
                cwd: None,
                via: LaunchVia::Flatpak,
            }),
            ..base(
                AppIdentity::Flatpak { app_id: id },
                Confidence::High,
                Source::Flatpak,
            )
        };
    }

    if let Some(wa) = webapp::parse_class(w.initial_class).or_else(|| webapp::parse_class(w.class))
    {
        let url = webapp::url_for(&wa, w.initial_title);
        let identity = AppIdentity::WebApp { url: url.clone() };
        if let Some(e) = webapp::find_desktop(idx, &wa) {
            return AppResolution {
                identity,
                launch: Some(LaunchSpec {
                    argv: e.exec.clone(),
                    cwd: None,
                    via: LaunchVia::WebApp,
                }),
                ..from_desktop(e, Confidence::High, Source::WebAppDesktop)
            };
        }
        let argv = match (&env.omarchy_webapp, &proc.exe) {
            (Some(launcher), _) => Some(vec![launcher.display().to_string(), url]),
            (None, Some(browser)) => {
                Some(vec![browser.display().to_string(), format!("--app={url}")])
            }
            (None, None) => None,
        };
        return AppResolution {
            launch_problem: argv
                .is_none()
                .then(|| "no browser found for web app".to_string()),
            launch: argv.map(|argv| LaunchSpec {
                argv,
                cwd: None,
                via: LaunchVia::WebApp,
            }),
            ..base(identity, Confidence::High, Source::WebAppClass)
        };
    }

    if let Some(e) = proc
        .env_desktop_file
        .as_deref()
        .and_then(|p| idx.by_path(p))
    {
        return from_desktop(e, Confidence::High, Source::LaunchedDesktopFile);
    }

    if let Some(e) = unit.as_ref().and_then(|u| idx.get(&u.app_id))
        && plausible(e, w, proc)
    {
        return from_desktop(e, Confidence::High, Source::SystemdScope);
    }

    if let Some(e) = idx
        .by_wm_class(w.initial_class)
        .or_else(|| idx.by_wm_class(w.class))
    {
        return from_desktop(e, Confidence::Medium, Source::StartupWmClass);
    }

    if let Some(e) =
        desktop_named_like(idx, w.initial_class).or_else(|| desktop_named_like(idx, w.class))
    {
        return from_desktop(e, Confidence::Medium, Source::DesktopIdIsClass);
    }

    let program_names = proc
        .exe
        .iter()
        .map(PathBuf::as_path)
        .chain(proc.cmdline.first().map(Path::new))
        .filter_map(|p| p.file_name()?.to_str());
    let by_program = program_names
        .into_iter()
        .find_map(|name| idx.by_program(name));

    if let Some(e) = by_program
        && class_fits(e, w)
    {
        return from_desktop(e, Confidence::Medium, Source::ExecutableDesktop);
    }

    if let Some(exe) = &proc.exe {
        let mut argv = proc.cmdline.clone();
        if argv.is_empty() {
            argv.push(exe.display().to_string());
        }
        let (launch, problem) = match security::sanitize(&argv, env.sensitive_args) {
            Sanitized::Clean(argv) => (
                Some(LaunchSpec {
                    argv,
                    cwd: cwd.clone(),
                    via: LaunchVia::Cmdline,
                }),
                None,
            ),
            Sanitized::Redacted(_) | Sanitized::Rejected => (
                None,
                Some(
                    "command line contains sensitive arguments; add a [[windows]] override".into(),
                ),
            ),
        };
        // The program has a desktop entry but this window's class does not fit
        // it (e.g. `foot --app-id scratch`): keep the app identity, relaunch
        // from the command line so the custom class comes back.
        let (identity, source, entry) = match by_program {
            Some(e) => (
                AppIdentity::Desktop { id: e.id.clone() },
                Source::ExecutableDesktop,
                Some(e.id.clone()),
            ),
            None => (
                AppIdentity::Executable { path: exe.clone() },
                Source::Cmdline,
                None,
            ),
        };
        return AppResolution {
            launch,
            launch_problem: problem,
            desktop_entry: entry,
            ..base(identity, Confidence::Low, source)
        };
    }

    AppResolution {
        launch_problem: Some("owning process not found".into()),
        ..base(
            AppIdentity::Class {
                class: w.initial_class.to_string(),
            },
            Confidence::Low,
            Source::ClassOnly,
        )
    }
}

/// Desktop id equal to the class, or whose last reverse-DNS segment is.
fn desktop_named_like<'a>(idx: &'a DesktopIndex, class: &str) -> Option<&'a DesktopEntry> {
    if class.is_empty() {
        return None;
    }
    idx.get(class).or_else(|| {
        idx.entries().iter().find(|e| {
            e.id.rsplit_once('.')
                .is_some_and(|(_, last)| last.eq_ignore_ascii_case(class))
        })
    })
}

/// Does the window's class look like what this entry opens?
fn class_fits(e: &DesktopEntry, w: &WindowFacts) -> bool {
    [w.class, w.initial_class].iter().any(|c| {
        !c.is_empty()
            && (e.id.eq_ignore_ascii_case(c)
                || e.id
                    .rsplit('.')
                    .next()
                    .is_some_and(|l| l.eq_ignore_ascii_case(c))
                || e.startup_wm_class
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case(c))
                || e.program()
                    .and_then(|p| Path::new(p).file_name())
                    .is_some_and(|b| b.to_string_lossy().eq_ignore_ascii_case(c)))
    })
}

/// A scope is inherited by everything spawned inside it (e.g. a program run
/// from a terminal lives in the terminal's scope), so only trust the scope's
/// desktop entry when it plausibly describes this window.
fn plausible(e: &DesktopEntry, w: &WindowFacts, proc: &ProcessInfo) -> bool {
    let class_hit = class_fits(e, w);
    let exe_hit = match (
        e.program().and_then(|p| Path::new(p).file_name()),
        &proc.exe,
    ) {
        (Some(prog), Some(exe)) => exe.file_name() == Some(prog),
        _ => false,
    };
    class_hit || exe_hit
}

fn useful_cwd(proc: &ProcessInfo) -> Option<PathBuf> {
    proc.child_cwd
        .clone()
        .or_else(|| proc.cwd.clone())
        .filter(|p| p != Path::new("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::desktop::parse_desktop_file;

    fn entry(id: &str, body: &str) -> DesktopEntry {
        let content = format!("[Desktop Entry]\nType=Application\nName={id}\n{body}\n");
        parse_desktop_file(id, Path::new(&format!("/apps/{id}.desktop")), &content).unwrap()
    }

    fn index() -> DesktopIndex {
        DesktopIndex::from_entries(vec![
            entry(
                "chromium",
                "Exec=/usr/bin/chromium %U\nStartupWMClass=@@startup_wm_class",
            ),
            entry("foot", "Exec=foot"),
            entry(
                "WhatsApp",
                "Exec=omarchy-launch-webapp https://web.whatsapp.com/",
            ),
            entry("code", "Exec=/usr/bin/code %F\nStartupWMClass=Code"),
            entry("org.gnome.Nautilus", "Exec=nautilus --new-window %U"),
        ])
    }

    fn facts<'a>(class: &'a str, title: &'a str) -> WindowFacts<'a> {
        WindowFacts {
            class,
            initial_class: class,
            initial_title: title,
        }
    }

    fn proc(exe: &str, cgroup: &str) -> ProcessInfo {
        ProcessInfo {
            pid: 42,
            exe: Some(exe.into()),
            cmdline: vec![exe.into()],
            cwd: Some("/".into()),
            cgroup: Some(cgroup.into()),
            ..Default::default()
        }
    }

    const CHROMIUM_SCOPE: &str = "/user.slice/app.slice/app-org.chromium.Chromium-5237.scope";
    const TERM_SCOPE: &str = r"/app.slice/app-Hyprland-xdg\x2dterminal\x2dexec-4c0069b2.scope";

    #[test]
    fn chromium_and_its_pwa_share_a_pid_but_not_an_identity() {
        let idx = index();
        let env = ResolveEnv::default();
        let p = proc("/usr/lib/chromium/chromium", CHROMIUM_SCOPE);

        let browser = resolve(&facts("chromium", "New Tab"), &p, &idx, &env);
        assert_eq!(
            browser.identity,
            AppIdentity::Desktop {
                id: "chromium".into()
            }
        );
        assert_eq!(browser.source, Source::DesktopIdIsClass);

        let pwa = resolve(
            &facts("chrome-web.whatsapp.com__-Default", "web.whatsapp.com_/"),
            &p,
            &idx,
            &env,
        );
        assert_eq!(
            pwa.identity,
            AppIdentity::WebApp {
                url: "https://web.whatsapp.com/".into()
            }
        );
        assert_eq!(pwa.confidence, Confidence::High);
        assert_eq!(
            pwa.launch.unwrap().argv,
            vec!["omarchy-launch-webapp", "https://web.whatsapp.com/"]
        );
    }

    #[test]
    fn pwa_without_desktop_entry_is_constructed() {
        let env = ResolveEnv {
            omarchy_webapp: None,
            ..Default::default()
        };
        let r = resolve(
            &facts("chrome-app.example.com__-Default", "app.example.com_/"),
            &proc("/usr/lib/chromium/chromium", CHROMIUM_SCOPE),
            &DesktopIndex::default(),
            &env,
        );
        assert_eq!(r.source, Source::WebAppClass);
        assert_eq!(
            r.launch.unwrap().argv,
            vec![
                "/usr/lib/chromium/chromium",
                "--app=https://app.example.com/"
            ]
        );
    }

    #[test]
    fn terminal_keeps_shell_cwd() {
        let mut p = proc("/usr/bin/foot", TERM_SCOPE);
        p.child_cwd = Some("/home/me/proj".into());
        let r = resolve(&facts("foot", "~"), &p, &index(), &ResolveEnv::default());
        assert_eq!(r.identity, AppIdentity::Desktop { id: "foot".into() });
        assert_eq!(r.launch.unwrap().cwd, Some("/home/me/proj".into()));
    }

    #[test]
    fn scope_inherited_from_terminal_is_not_trusted() {
        let idx =
            DesktopIndex::from_entries(vec![entry("xdg-terminal-exec", "Exec=xdg-terminal-exec")]);
        let r = resolve(
            &facts("zathura", "doc.pdf"),
            &proc("/usr/bin/zathura", TERM_SCOPE),
            &idx,
            &ResolveEnv::default(),
        );
        assert_eq!(
            r.identity,
            AppIdentity::Executable {
                path: "/usr/bin/zathura".into()
            }
        );
        assert_eq!(r.confidence, Confidence::Low);
    }

    #[test]
    fn wm_class_and_reverse_dns() {
        let idx = index();
        let env = ResolveEnv::default();
        let code = resolve(
            &facts("Code", "x"),
            &proc("/usr/lib/electron/electron", "/x"),
            &idx,
            &env,
        );
        assert_eq!(code.identity, AppIdentity::Desktop { id: "code".into() });
        assert_eq!(code.source, Source::StartupWmClass);
        let naut = resolve(
            &facts("Nautilus", "x"),
            &proc("/usr/bin/nautilus", "/x"),
            &idx,
            &env,
        );
        assert_eq!(naut.desktop_entry.as_deref(), Some("org.gnome.Nautilus"));
    }

    #[test]
    fn custom_class_relaunches_from_cmdline() {
        let mut p = proc("/usr/bin/foot", TERM_SCOPE);
        p.cmdline = vec!["foot".into(), "--app-id".into(), "scratch".into()];
        let r = resolve(&facts("scratch", "x"), &p, &index(), &ResolveEnv::default());
        assert_eq!(r.identity, AppIdentity::Desktop { id: "foot".into() });
        assert_eq!(r.launch.unwrap().argv, vec!["foot", "--app-id", "scratch"]);
        assert_eq!(r.confidence, Confidence::Low);
    }

    #[test]
    fn flatpak_wins() {
        let r = resolve(
            &facts("Spotify", "x"),
            &proc(
                "/app/bin/spotify",
                "/app.slice/app-flatpak-com.spotify.Client-99.scope",
            ),
            &index(),
            &ResolveEnv::default(),
        );
        assert_eq!(
            r.identity,
            AppIdentity::Flatpak {
                app_id: "com.spotify.Client".into()
            }
        );
        assert_eq!(
            r.launch.unwrap().argv,
            vec!["flatpak", "run", "com.spotify.Client"]
        );
    }

    #[test]
    fn sensitive_cmdline_is_not_relaunchable() {
        let mut p = proc("/opt/tool/tool", "/x");
        p.cmdline = vec!["/opt/tool/tool".into(), "--token".into(), "abc".into()];
        let r = resolve(
            &facts("tool", "x"),
            &p,
            &DesktopIndex::default(),
            &ResolveEnv::default(),
        );
        assert!(r.launch.is_none());
        assert!(r.launch_problem.unwrap().contains("sensitive"));
    }

    #[test]
    fn dead_process_falls_back_to_class() {
        let r = resolve(
            &facts("ghost", "x"),
            &ProcessInfo::default(),
            &DesktopIndex::default(),
            &ResolveEnv::default(),
        );
        assert_eq!(
            r.identity,
            AppIdentity::Class {
                class: "ghost".into()
            }
        );
        assert!(r.launch.is_none());
    }
}
