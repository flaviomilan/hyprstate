//! Live compositor state → resolved windows → [`Snapshot`].

use std::collections::BTreeMap;
use std::path::Path;

use jiff::Timestamp;

use crate::config::Config;
use crate::discovery::desktop::DesktopIndex;
use crate::discovery::process::{self, ProcessInfo};
use crate::discovery::resolve::{self, AppIdentity, AppResolution, ResolveEnv, WindowFacts};
use crate::hyprland::models::{Client, LiveState};
use crate::policy::exclusions::{Exclusions, Subject};

use super::model::*;

/// Everything needed to turn a Hyprland client into a resolved window.
pub struct Discovery {
    pub index: DesktopIndex,
    pub env: ResolveEnv,
    pub exclusions: Exclusions,
    read_proc: Box<dyn Fn(i32) -> ProcessInfo>,
}

#[derive(Debug, Clone)]
pub struct ResolvedWindow {
    pub client: Client,
    pub app: AppResolution,
    pub excluded: Option<String>,
}

impl Discovery {
    pub fn from_system(cfg: &Config) -> Self {
        Self {
            index: DesktopIndex::load(),
            env: ResolveEnv {
                sensitive_args: cfg.security.sensitive_args,
                omarchy_webapp: crate::discovery::which("omarchy-launch-webapp"),
            },
            exclusions: cfg.exclusions(),
            read_proc: Box::new(process::read_process),
        }
    }

    pub fn new(
        index: DesktopIndex,
        env: ResolveEnv,
        exclusions: Exclusions,
        read_proc: impl Fn(i32) -> ProcessInfo + 'static,
    ) -> Self {
        Self {
            index,
            env,
            exclusions,
            read_proc: Box::new(read_proc),
        }
    }

    pub fn window(&self, c: &Client) -> ResolvedWindow {
        let proc = (self.read_proc)(c.pid);
        let facts = WindowFacts {
            class: &c.class,
            initial_class: &c.initial_class,
            initial_title: &c.initial_title,
        };
        let app = resolve::resolve(&facts, &proc, &self.index, &self.env);
        let ids = identity_names(&app);
        let excluded = self.exclusions.check(&Subject {
            class: &c.class,
            initial_class: &c.initial_class,
            title: &c.title,
            app_ids: &ids,
        });
        ResolvedWindow {
            client: c.clone(),
            app,
            excluded,
        }
    }

    /// Mapped windows only; unmapped surfaces are not user-visible windows.
    pub fn windows(&self, clients: &[Client]) -> Vec<ResolvedWindow> {
        clients
            .iter()
            .filter(|c| c.mapped)
            .map(|c| self.window(c))
            .collect()
    }
}

/// Names an exclusion list may refer to.
pub fn identity_names(app: &AppResolution) -> Vec<String> {
    let mut v = Vec::new();
    match &app.identity {
        AppIdentity::Desktop { id } => v.push(id.clone()),
        AppIdentity::Flatpak { app_id } => v.push(app_id.clone()),
        AppIdentity::WebApp { url } => v.push(url.clone()),
        AppIdentity::Executable { path } => {
            if let Some(n) = path.file_name() {
                v.push(n.to_string_lossy().into_owned());
            }
        }
        AppIdentity::Class { class } => v.push(class.clone()),
    }
    if let Some(n) = app.executable.as_deref().and_then(Path::file_name) {
        v.push(n.to_string_lossy().into_owned());
    }
    v
}

pub fn build_snapshot(name: String, live: &LiveState, windows: &[ResolvedWindow]) -> Snapshot {
    build_snapshot_at(name, Timestamp::now(), live, windows)
}

pub fn build_snapshot_at(
    name: String,
    created_at: Timestamp,
    live: &LiveState,
    windows: &[ResolvedWindow],
) -> Snapshot {
    // Group index per distinct set of grouped addresses.
    let mut groups: BTreeMap<Vec<String>, u32> = BTreeMap::new();
    let mut records = Vec::new();
    let mut excluded_count = 0;
    for w in windows {
        if w.excluded.is_some() {
            excluded_count += 1;
            continue;
        }
        let c = &w.client;
        let group = (c.grouped.len() > 1).then(|| {
            let mut key = c.grouped.clone();
            key.sort();
            let next = groups.len() as u32;
            *groups.entry(key).or_insert(next)
        });
        records.push(WindowRecord {
            key: records.len() as u32,
            class: c.class.clone(),
            initial_class: c.initial_class.clone(),
            title: c.title.clone(),
            initial_title: c.initial_title.clone(),
            workspace: c.workspace.clone(),
            monitor: live.monitor_name(c.monitor).map(str::to_string),
            at: c.at,
            size: c.size,
            floating: c.floating,
            fullscreen: c.fullscreen,
            pinned: c.pinned,
            xwayland: c.xwayland,
            group,
            pid: c.pid,
            address: c.address.clone(),
            app: w.app.clone(),
            require: Default::default(),
        });
    }
    let mut workspaces: Vec<WorkspaceRecord> = live
        .workspaces
        .iter()
        .map(|w| WorkspaceRecord {
            id: w.id,
            name: w.name.clone(),
            monitor: w.monitor.clone(),
            windows: records.iter().filter(|r| r.workspace.id == w.id).count() as u32,
            tiled_layout: w.tiled_layout.clone(),
        })
        .collect();
    workspaces.sort_by_key(|w| w.id);
    Snapshot {
        schema_version: SCHEMA_VERSION,
        name,
        created_at,
        hyprland: HyprlandInfo {
            version: live.version.version.clone(),
            commit: live.version.commit.clone(),
        },
        monitors: live
            .monitors
            .iter()
            .filter(|m| !m.disabled)
            .map(|m| MonitorRecord {
                name: m.name.clone(),
                description: m.description.clone(),
                width: m.width,
                height: m.height,
                x: m.x,
                y: m.y,
                scale: m.scale,
                active_workspace: m.active_workspace.clone(),
            })
            .collect(),
        workspaces,
        active_workspace: live.active_workspace.clone(),
        windows: records,
        excluded_count,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::discovery::desktop::parse_desktop_file;
    use crate::hyprland::models::*;

    fn fixture<T: serde::de::DeserializeOwned>(name: &str) -> T {
        let p = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    pub fn fixture_live() -> LiveState {
        LiveState {
            version: fixture("version"),
            monitors: fixture("monitors"),
            workspaces: fixture("workspaces"),
            active_workspace: fixture("activeworkspace"),
            clients: fixture("clients"),
        }
    }

    /// Discovery with a fake /proc mirroring the fixture machine.
    pub fn fixture_discovery() -> Discovery {
        let entries = [
            ("chromium", "Exec=/usr/bin/chromium %U"),
            ("foot", "Exec=foot"),
            (
                "WhatsApp",
                "Exec=omarchy-launch-webapp https://web.whatsapp.com/",
            ),
        ]
        .into_iter()
        .map(|(id, body)| {
            let c = format!("[Desktop Entry]\nType=Application\nName={id}\n{body}\n");
            parse_desktop_file(id, Path::new(&format!("/apps/{id}.desktop")), &c).unwrap()
        })
        .collect();
        Discovery::new(
            DesktopIndex::from_entries(entries),
            ResolveEnv::default(),
            Exclusions::new(&[], &[], true),
            |pid| match pid {
                5237 => ProcessInfo {
                    pid,
                    exe: Some("/usr/lib/chromium/chromium".into()),
                    cmdline: vec!["/usr/lib/chromium/chromium".into()],
                    cgroup: Some("/app.slice/app-org.chromium.Chromium-5237.scope".into()),
                    ..Default::default()
                },
                147550 => ProcessInfo {
                    pid,
                    exe: Some("/usr/bin/foot".into()),
                    cmdline: vec!["foot".into()],
                    child_cwd: Some("/home/me/proj".into()),
                    cgroup: Some(r"/app-Hyprland-xdg\x2dterminal\x2dexec-4c0069b2.scope".into()),
                    ..Default::default()
                },
                _ => ProcessInfo::default(),
            },
        )
    }

    #[test]
    fn captures_fixture_state() {
        let live = fixture_live();
        let d = fixture_discovery();
        let windows = d.windows(&live.clients);
        let snap = build_snapshot_at("t".into(), Timestamp::UNIX_EPOCH, &live, &windows);
        assert_eq!(snap.windows.len(), 4);
        assert_eq!(snap.hyprland.version, "0.56.2");
        assert_eq!(snap.windows[0].monitor.as_deref(), Some("DP-1"));
        assert!(matches!(
            snap.windows[0].app.identity,
            AppIdentity::WebApp { .. }
        ));
        let ws1 = snap.workspaces.iter().find(|w| w.id == 1).unwrap();
        assert_eq!(ws1.windows, 2);
        // round trip
        let json = serde_json::to_string(&snap).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.windows, snap.windows);
    }

    #[test]
    fn excluded_windows_are_counted_not_stored() {
        let mut live = fixture_live();
        live.clients[2].class = "org.keepassxc.KeePassXC".into();
        live.clients[2].initial_class = "org.keepassxc.KeePassXC".into();
        let d = fixture_discovery();
        let snap = build_snapshot_at(
            "t".into(),
            Timestamp::UNIX_EPOCH,
            &live,
            &d.windows(&live.clients),
        );
        assert_eq!(snap.windows.len(), 3);
        assert_eq!(snap.excluded_count, 1);
        assert!(!serde_json::to_string(&snap).unwrap().contains("KeePassXC"));
    }

    #[test]
    fn workspace_targets() {
        let r = |id, name: &str| WorkspaceRef {
            id,
            name: name.into(),
        };
        assert_eq!(workspace_target(&r(3, "3")), "3");
        assert_eq!(workspace_target(&r(-98, "special:magic")), "special:magic");
    }

    #[test]
    fn identity_names_cover_every_identity() {
        let mut app = fixture_discovery().windows(&fixture_live().clients)[2]
            .app
            .clone();
        app.executable = None;
        let names = |app: &AppResolution, id| {
            let mut app = app.clone();
            app.identity = id;
            identity_names(&app)
        };
        assert_eq!(
            names(
                &app,
                AppIdentity::Flatpak {
                    app_id: "a.b".into()
                }
            ),
            ["a.b"]
        );
        assert_eq!(
            names(
                &app,
                AppIdentity::Executable {
                    path: "/bin/htop".into()
                }
            ),
            ["htop"]
        );
        assert!(names(&app, AppIdentity::Executable { path: "/".into() }).is_empty());
        assert_eq!(names(&app, AppIdentity::Class { class: "k".into() }), ["k"]);
    }

    #[test]
    fn grouped_windows_share_a_group_index() {
        let mut live = fixture_live();
        let pair = vec![
            live.clients[1].address.clone(),
            live.clients[3].address.clone(),
        ];
        live.clients[1].grouped = pair.clone();
        live.clients[3].grouped = pair.into_iter().rev().collect();
        let d = fixture_discovery();
        let snap = build_snapshot("g".into(), &live, &d.windows(&live.clients));
        assert_eq!(snap.windows[1].group, Some(0));
        assert_eq!(snap.windows[3].group, Some(0));
        assert_eq!(snap.windows[0].group, None);
    }
}
