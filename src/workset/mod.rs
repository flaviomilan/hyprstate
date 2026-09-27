//! Worksets: named, hand-editable desired desktops.
//!
//! A snapshot records what *was*; a workset declares what *should be*. Opening
//! one turns it into expected windows and runs the regular restore engine
//! (planner + executor), so it gets dry-run, no-duplicate launching and
//! idempotence for free.

pub mod store;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::discovery::desktop::DesktopIndex;
use crate::discovery::resolve::{
    AppIdentity, AppResolution, Confidence, LaunchSpec, LaunchVia, Source,
};
use crate::discovery::webapp;
use crate::hyprland::models::WorkspaceRef;
use crate::snapshot::capture::ResolvedWindow;
use crate::snapshot::model::{HyprlandInfo, Require, SCHEMA_VERSION, Snapshot, WindowRecord};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workset {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub windows: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub workspace: WorkspaceSpec,
    /// Shell-style command line; `~/` is expanded.
    pub command: String,
    /// Launch directory. Also required of an open window before it is reused
    /// (so a terminal in another project is never taken over).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Window class, to recognise the window when the command alone cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Only reuse an open window whose title contains this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_contains: Option<String>,
    /// `false`: always launch a new window, never reuse an open one.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub reuse: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub floating: bool,
    /// Floating only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[i32; 2]>,
    /// Floating only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[i32; 2]>,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

/// `workspace = 3`, `workspace = "special:scratch"` or `workspace = "web"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WorkspaceSpec {
    Id(i64),
    Name(String),
}

impl WorkspaceSpec {
    pub fn to_ref(&self) -> WorkspaceRef {
        match self {
            Self::Id(id) => WorkspaceRef {
                id: *id,
                name: id.to_string(),
            },
            Self::Name(n) => {
                if let Ok(id) = n.parse::<i64>() {
                    return WorkspaceRef {
                        id,
                        name: n.clone(),
                    };
                }
                let name = n.strip_prefix("name:").unwrap_or(n);
                // Named/special workspace ids are assigned by Hyprland; 0 means
                // "compare by name" (see `WorkspaceRef::same_as`).
                WorkspaceRef {
                    id: 0,
                    name: name.to_string(),
                }
            }
        }
    }

    fn from_ref(ws: &WorkspaceRef) -> Self {
        if ws.id > 0 {
            Self::Id(ws.id)
        } else {
            Self::Name(ws.name.clone())
        }
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn expand_tilde(s: &str) -> String {
    match (s.strip_prefix("~/"), home()) {
        (Some(rest), Some(h)) => h.join(rest).to_string_lossy().into_owned(),
        _ if s == "~" => home()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.into()),
        _ => s.to_string(),
    }
}

fn compress_tilde(p: &Path) -> String {
    match home() {
        Some(h) if p.starts_with(&h) && p != h => {
            format!("~/{}", p.strip_prefix(&h).unwrap().display())
        }
        _ => p.display().to_string(),
    }
}

/// What application a command opens, so an already open window of it can be
/// recognised. Mirrors the signals `discovery::resolve` uses on live windows.
fn identity_for(
    argv: &[String],
    idx: &DesktopIndex,
) -> (AppIdentity, Option<String>, Option<String>) {
    let prog = argv.first().map(String::as_str).unwrap_or_default();
    let base = Path::new(prog)
        .file_name()
        .and_then(|b| b.to_str())
        .unwrap_or(prog);

    let is_webapp = base == "omarchy-launch-webapp" || argv.iter().any(|a| a.starts_with("--app="));
    if is_webapp && let Some(url) = webapp::url_in_argv(argv).and_then(canonical_url) {
        return (AppIdentity::WebApp { url }, None, None);
    }
    if base == "flatpak"
        && argv.get(1).map(String::as_str) == Some("run")
        && let Some(id) = argv[2..].iter().find(|a| !a.starts_with('-'))
    {
        return (AppIdentity::Flatpak { app_id: id.clone() }, None, None);
    }
    if let Some(e) = idx.get(base).or_else(|| idx.by_program(base)) {
        return (
            AppIdentity::Desktop { id: e.id.clone() },
            Some(e.id.clone()),
            e.startup_wm_class.clone(),
        );
    }
    let path = crate::discovery::which(prog).unwrap_or_else(|| PathBuf::from(prog));
    (AppIdentity::Executable { path }, None, None)
}

/// `https://host/path` exactly as live web app windows are identified.
fn canonical_url(url: &str) -> Option<String> {
    let name = webapp::class_name_for_url(url)?;
    let (_, rest) = url.split_once("://")?;
    let rest = rest.split(['?', '#']).next()?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let host = host.rsplit('@').next()?.split(':').next()?;
    debug_assert_eq!(format!("{host}_{path}").replace('/', "_"), name);
    Some(format!("https://{host}{path}"))
}

impl Workset {
    /// Expected windows, in the shape the restore engine consumes.
    pub fn to_snapshot(&self, name: &str, idx: &DesktopIndex) -> Snapshot {
        let windows = self
            .windows
            .iter()
            .enumerate()
            .map(|(i, e)| e.to_record(i as u32, idx))
            .collect();
        Snapshot {
            schema_version: SCHEMA_VERSION,
            name: format!("workset:{name}"),
            created_at: jiff::Timestamp::now(),
            hyprland: HyprlandInfo {
                version: String::new(),
                commit: String::new(),
            },
            monitors: Vec::new(),
            workspaces: Vec::new(),
            active_workspace: WorkspaceRef {
                id: 0,
                name: String::new(),
            },
            windows,
            excluded_count: 0,
        }
    }

    /// Declares the live desktop (optionally only some workspaces) as a workset.
    /// Returns the workset and the windows that could not be expressed.
    pub fn from_live(windows: &[ResolvedWindow], only: &[WorkspaceSpec]) -> (Self, Vec<String>) {
        let wanted: Vec<WorkspaceRef> = only.iter().map(WorkspaceSpec::to_ref).collect();
        let mut sorted: Vec<&ResolvedWindow> = windows
            .iter()
            .filter(|w| w.excluded.is_none())
            .filter(|w| {
                wanted.is_empty() || wanted.iter().any(|ws| ws.same_as(&w.client.workspace))
            })
            .collect();
        // Stable, readable order: by workspace, then top-left first.
        sorted.sort_by_key(|w| {
            let ws = &w.client.workspace;
            (
                ws.id <= 0,
                ws.id,
                ws.name.clone(),
                w.client.at[1],
                w.client.at[0],
            )
        });
        let home = home();
        let mut skipped = Vec::new();
        let mut entries = Vec::new();
        for w in sorted {
            let c = &w.client;
            let Some(launch) = &w.app.launch else {
                let why = w
                    .app
                    .launch_problem
                    .as_deref()
                    .unwrap_or("no launch command");
                skipped.push(format!(
                    "{} on workspace {}: {why}",
                    c.class, c.workspace.name
                ));
                continue;
            };
            let cwd = w
                .app
                .cwd
                .as_deref()
                .filter(|d| Some(*d) != home.as_deref())
                .map(compress_tilde);
            entries.push(Entry {
                workspace: WorkspaceSpec::from_ref(&c.workspace),
                command: shell_words::join(&launch.argv),
                cwd,
                class: Some(c.initial_class.clone()).filter(|s| !s.is_empty()),
                title_contains: None,
                reuse: true,
                floating: c.floating,
                position: c.floating.then_some(c.at),
                size: c.floating.then_some(c.size),
            });
        }
        (
            Self {
                description: None,
                windows: entries,
            },
            skipped,
        )
    }
}

impl Entry {
    fn to_record(&self, key: u32, idx: &DesktopIndex) -> WindowRecord {
        let cwd = self.cwd.as_deref().map(|d| PathBuf::from(expand_tilde(d)));
        let (argv, problem) = match shell_words::split(&self.command) {
            Ok(argv) if !argv.is_empty() => (argv.iter().map(|a| expand_tilde(a)).collect(), None),
            Ok(_) => (Vec::new(), Some("empty command".to_string())),
            Err(e) => (Vec::new(), Some(format!("cannot parse command: {e}"))),
        };
        let (identity, desktop_entry, wm_class) = identity_for(&argv, idx);
        let class = self.class.clone().or(wm_class).unwrap_or_default();
        WindowRecord {
            key,
            class: class.clone(),
            initial_class: class,
            title: String::new(),
            initial_title: String::new(),
            workspace: self.workspace.to_ref(),
            monitor: None,
            at: self.position.unwrap_or_default(),
            size: self.size.unwrap_or_default(),
            floating: self.floating,
            fullscreen: 0,
            pinned: false,
            xwayland: false,
            group: None,
            pid: 0,
            address: String::new(),
            app: AppResolution {
                identity,
                confidence: Confidence::High,
                source: Source::Workset,
                launch: problem.is_none().then(|| LaunchSpec {
                    argv,
                    cwd: cwd.clone(),
                    via: LaunchVia::Workset,
                }),
                launch_problem: problem,
                desktop_entry,
                executable: None,
                cwd: cwd.clone(),
            },
            require: Require {
                cwd,
                title_contains: self.title_contains.clone(),
                always_launch: !self.reuse,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::desktop::parse_desktop_file;

    fn index() -> DesktopIndex {
        let e = |id: &str, body: &str| {
            let c = format!("[Desktop Entry]\nType=Application\nName={id}\n{body}\n");
            parse_desktop_file(id, Path::new(&format!("/apps/{id}.desktop")), &c).unwrap()
        };
        DesktopIndex::from_entries(vec![
            e("foot", "Exec=foot"),
            e("code", "Exec=/usr/bin/code %F\nStartupWMClass=Code"),
            e(
                "WhatsApp",
                "Exec=omarchy-launch-webapp https://web.whatsapp.com/",
            ),
        ])
    }

    const EXAMPLE: &str = r#"
        description = "Recsys work"

        [[windows]]
        workspace = 1
        command = "code ~/projects/recsys"

        [[windows]]
        workspace = 2
        command = "foot"
        cwd = "~/projects/recsys"

        [[windows]]
        workspace = "special:chat"
        command = "omarchy-launch-webapp https://web.whatsapp.com/"

        [[windows]]
        workspace = 4
        command = "flatpak run com.spotify.Client"
        floating = true
        position = [100, 100]
        size = [800, 600]
        reuse = false
    "#;

    #[test]
    fn parses_and_converts() {
        let ws: Workset = toml::from_str(EXAMPLE).unwrap();
        let snap = ws.to_snapshot("recsys", &index());
        let w = &snap.windows;
        assert_eq!(snap.name, "workset:recsys");

        assert_eq!(
            w[0].app.identity,
            AppIdentity::Desktop { id: "code".into() }
        );
        assert_eq!(w[0].initial_class, "Code");
        let argv = &w[0].app.launch.as_ref().unwrap().argv;
        assert!(argv[1].ends_with("/projects/recsys") && !argv[1].starts_with('~'));

        assert!(
            w[1].require
                .cwd
                .as_ref()
                .unwrap()
                .ends_with("projects/recsys")
        );
        assert_eq!(
            w[2].app.identity,
            AppIdentity::WebApp {
                url: "https://web.whatsapp.com/".into()
            }
        );
        assert_eq!(
            w[2].workspace,
            WorkspaceRef {
                id: 0,
                name: "special:chat".into()
            }
        );
        assert_eq!(
            w[3].app.identity,
            AppIdentity::Flatpak {
                app_id: "com.spotify.Client".into()
            }
        );
        assert!(w[3].floating && w[3].require.always_launch);
        assert_eq!((w[3].at, w[3].size), ([100, 100], [800, 600]));
    }

    #[test]
    fn rejects_unknown_keys_and_reports_bad_commands() {
        assert!(
            toml::from_str::<Workset>(
                "[[windows]]\nworkspace = 1\ncommand = \"x\"\nworkspce = 2\n"
            )
            .is_err()
        );
        let ws: Workset =
            toml::from_str("[[windows]]\nworkspace = 1\ncommand = \"foo 'unclosed\"\n").unwrap();
        let r = &ws.to_snapshot("x", &index()).windows[0];
        assert!(r.app.launch.is_none() && r.app.launch_problem.is_some());
    }

    #[test]
    fn open_reuses_only_windows_meeting_the_requirements() {
        use crate::policy::exclusions::Exclusions;
        use crate::restore::planner::{Action, PlanInput, plan};
        use crate::snapshot::capture::tests::{fixture_discovery, fixture_live};

        let live = fixture_live();
        let d = fixture_discovery();
        let wins = d.windows(&live.clients);
        let run = |toml_src: &str| {
            let ws: Workset = toml::from_str(toml_src).unwrap();
            let snap = ws.to_snapshot("t", &d.index);
            let excl = Exclusions::new(&[], &[], true);
            let p = plan(&PlanInput {
                snapshot: &snap,
                live: &live,
                windows: &wins,
                overrides: &[],
                exclusions: &excl,
                launch_wrapper: &[],
                runnable: &|_| true,
            });
            p.items.into_iter().map(|i| i.action).collect::<Vec<_>>()
        };

        // The fixture terminal's shell is in /home/me/proj, on workspace 3.
        let same_dir =
            run("[[windows]]\nworkspace = 3\ncommand = \"foot\"\ncwd = \"/home/me/proj\"\n");
        assert!(matches!(&same_dir[0], Action::Reuse { commands, .. } if commands.is_empty()));

        let other_dir =
            run("[[windows]]\nworkspace = 3\ncommand = \"foot\"\ncwd = \"/home/me/other\"\n");
        assert!(
            matches!(&other_dir[0], Action::Launch { spec } if spec.cwd.as_deref() == Some(Path::new("/home/me/other")))
        );

        let never = run("[[windows]]\nworkspace = 3\ncommand = \"foot\"\nreuse = false\n");
        assert!(matches!(&never[0], Action::Launch { .. }));

        // The open WhatsApp PWA is reused and moved, not launched twice.
        let pwa = run(
            "[[windows]]\nworkspace = 5\ncommand = \"omarchy-launch-webapp https://web.whatsapp.com/\"\n",
        );
        assert!(matches!(&pwa[0], Action::Reuse { commands, .. } if commands.len() == 1));
    }

    #[test]
    fn live_desktop_round_trips_through_toml() {
        use crate::snapshot::capture::tests::{fixture_discovery, fixture_live};
        let live = fixture_live();
        let wins = fixture_discovery().windows(&live.clients);
        let (ws, skipped) =
            Workset::from_live(&wins, &[WorkspaceSpec::Id(2), WorkspaceSpec::Id(3)]);
        assert!(skipped.is_empty());
        assert_eq!(ws.windows.len(), 2);
        assert_eq!(
            ws.windows[0].command,
            "omarchy-launch-webapp https://web.whatsapp.com/"
        );
        assert_eq!(ws.windows[1].cwd.as_deref(), Some("/home/me/proj"));
        let text = toml::to_string_pretty(&ws).unwrap();
        assert_eq!(toml::from_str::<Workset>(&text).unwrap(), ws);
    }
}
