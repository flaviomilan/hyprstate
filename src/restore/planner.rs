//! Snapshot × live state → [`Plan`]. Pure: nothing here touches the desktop.
//!
//! Only differences become commands, so planning against a desktop that
//! already matches the snapshot yields a plan with no launches and no
//! commands — that is what makes `restore` idempotent.

use serde::Serialize;

use crate::config::WindowOverride;
use crate::discovery::resolve::{AppIdentity, Confidence, LaunchSpec, LaunchVia};
use crate::hyprland::commands::Command;
use crate::hyprland::models::{Client, LiveState, WorkspaceRef};
use crate::policy::exclusions::{Exclusions, Subject};
use crate::snapshot::capture::{ResolvedWindow, identity_names};
use crate::snapshot::model::{Snapshot, WindowRecord, workspace_target};

use super::matcher::{self, Matchable, Strategy};

impl Matchable for WindowRecord {
    fn identity(&self) -> &AppIdentity {
        &self.app.identity
    }
    fn confidence(&self) -> Confidence {
        self.app.confidence
    }
    fn class(&self) -> &str {
        &self.class
    }
    fn initial_class(&self) -> &str {
        &self.initial_class
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn initial_title(&self) -> &str {
        &self.initial_title
    }
    fn workspace(&self) -> &WorkspaceRef {
        &self.workspace
    }
    fn floating(&self) -> bool {
        self.floating
    }
    fn required_cwd(&self) -> Option<&std::path::Path> {
        self.require.cwd.as_deref()
    }
    fn required_title(&self) -> Option<&str> {
        self.require.title_contains.as_deref()
    }
}

impl Matchable for ResolvedWindow {
    fn identity(&self) -> &AppIdentity {
        &self.app.identity
    }
    fn confidence(&self) -> Confidence {
        self.app.confidence
    }
    fn class(&self) -> &str {
        &self.client.class
    }
    fn initial_class(&self) -> &str {
        &self.client.initial_class
    }
    fn title(&self) -> &str {
        &self.client.title
    }
    fn initial_title(&self) -> &str {
        &self.client.initial_title
    }
    fn workspace(&self) -> &WorkspaceRef {
        &self.client.workspace
    }
    fn floating(&self) -> bool {
        self.client.floating
    }
    fn cwd(&self) -> Option<&std::path::Path> {
        self.app.cwd.as_deref()
    }
}

/// Where a window should end up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Placement {
    pub workspace: WorkspaceRef,
    pub floating: bool,
    pub at: [i32; 2],
    pub size: [i32; 2],
    pub pinned: bool,
    pub fullscreen: u8,
}

impl Placement {
    pub fn of(r: &WindowRecord) -> Self {
        Self {
            workspace: r.workspace.clone(),
            floating: r.floating,
            at: r.at,
            size: r.size,
            pinned: r.pinned,
            fullscreen: r.fullscreen,
        }
    }

    /// Commands that take `current` to this placement (only what differs).
    pub fn commands(&self, current: &Client) -> Vec<Command> {
        let address = &current.address;
        let mut out = Vec::new();
        if !current.workspace.same_as(&self.workspace) {
            out.push(Command::MoveToWorkspace {
                address: address.clone(),
                workspace: workspace_target(&self.workspace),
            });
        }
        if current.floating != self.floating {
            out.push(Command::SetFloating {
                address: address.clone(),
                floating: self.floating,
            });
        }
        // Tiled geometry belongs to the layout; only floating windows are placed.
        if self.floating {
            if current.size != self.size {
                out.push(Command::ResizeExact {
                    address: address.clone(),
                    w: self.size[0],
                    h: self.size[1],
                });
            }
            if current.at != self.at {
                out.push(Command::MoveExact {
                    address: address.clone(),
                    x: self.at[0],
                    y: self.at[1],
                });
            }
            if current.pinned != self.pinned {
                out.push(Command::SetPinned {
                    address: address.clone(),
                    pinned: self.pinned,
                });
            }
        }
        if current.fullscreen != self.fullscreen {
            out.push(Command::Fullscreen {
                address: address.clone(),
                internal: self.fullscreen,
                client: if self.fullscreen == 2 { 2 } else { 0 },
            });
        }
        out
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Reuse {
        address: String,
        strategy: Strategy,
        confidence: Confidence,
        ambiguous: bool,
        commands: Vec<Command>,
    },
    Launch {
        spec: LaunchSpec,
    },
    Unresolved {
        reason: String,
    },
    Excluded {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanItem {
    /// `WindowRecord::key` in the snapshot.
    pub key: u32,
    pub label: String,
    pub placement: Placement,
    pub action: Action,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub reuse: usize,
    pub launch: usize,
    pub unresolved: usize,
    pub excluded: usize,
    pub ambiguous: usize,
    /// Reused windows that need at least one command.
    pub to_move: usize,
    /// Live windows not claimed by the snapshot (left alone).
    pub untouched: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub snapshot: String,
    pub items: Vec<PlanItem>,
    /// Workspace → monitor bindings, applied after windows are placed.
    pub workspace_commands: Vec<Command>,
    pub summary: Summary,
}

impl Plan {
    pub fn is_noop(&self) -> bool {
        self.summary.launch == 0 && self.summary.to_move == 0 && self.workspace_commands.is_empty()
    }
}

pub struct PlanInput<'a> {
    pub snapshot: &'a Snapshot,
    pub live: &'a LiveState,
    /// Live windows, resolved with the same discovery used at capture time.
    pub windows: &'a [ResolvedWindow],
    pub overrides: &'a [WindowOverride],
    pub exclusions: &'a Exclusions,
    /// Prefix for every launch (e.g. `uwsm app --`).
    pub launch_wrapper: &'a [String],
    /// Is this program runnable? (`which`, injected for tests.)
    pub runnable: &'a dyn Fn(&str) -> bool,
}

pub fn label(r: &WindowRecord) -> String {
    match &r.app.identity {
        AppIdentity::WebApp { url } => url
            .split("://")
            .nth(1)
            .and_then(|s| s.split('/').next())
            .unwrap_or(url)
            .to_string(),
        AppIdentity::Desktop { id } | AppIdentity::Flatpak { app_id: id } => {
            // Reverse-DNS ids read better by their last segment.
            let last = id.rsplit('.').next().unwrap_or(id);
            if id.matches('.').count() >= 2 {
                last.to_string()
            } else {
                id.clone()
            }
        }
        AppIdentity::Executable { path } => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| r.class.clone()),
        AppIdentity::Class { class } => class.clone(),
    }
}

pub fn plan(input: &PlanInput) -> Plan {
    let snap = input.snapshot;

    // Excluded live windows are never touched, not even as match candidates.
    let candidates: Vec<&ResolvedWindow> = input
        .windows
        .iter()
        .filter(|w| w.excluded.is_none())
        .collect();

    // Records the current policy excludes (config may have changed since capture).
    let mut items: Vec<Option<PlanItem>> = vec![None; snap.windows.len()];
    let mut wanted: Vec<usize> = Vec::new();
    for (i, r) in snap.windows.iter().enumerate() {
        let ids = identity_names(&r.app);
        let subject = Subject {
            class: &r.class,
            initial_class: &r.initial_class,
            title: &r.title,
            app_ids: &ids,
        };
        match input.exclusions.check(&subject) {
            Some(reason) => {
                items[i] = Some(PlanItem {
                    key: r.key,
                    label: label(r),
                    placement: Placement::of(r),
                    action: Action::Excluded { reason },
                });
            }
            None => wanted.push(i),
        }
    }

    // `always_launch` records never take over an already open window.
    let reusable: Vec<usize> = wanted
        .iter()
        .copied()
        .filter(|&i| !snap.windows[i].require.always_launch)
        .collect();
    let assignment = matcher::assign(
        &reusable
            .iter()
            .map(|&i| snap.windows[i].clone())
            .collect::<Vec<_>>(),
        &candidates.iter().map(|w| (*w).clone()).collect::<Vec<_>>(),
    );

    for &i in &wanted {
        let r = &snap.windows[i];
        let placement = Placement::of(r);
        let matched = reusable
            .iter()
            .position(|&j| j == i)
            .and_then(|ei| assignment.for_expected(ei));
        let action = match matched {
            Some(m) => {
                let client = &candidates[m.candidate].client;
                Action::Reuse {
                    address: client.address.clone(),
                    strategy: m.strategy,
                    confidence: m.confidence,
                    ambiguous: m.ambiguous,
                    commands: placement.commands(client),
                }
            }
            None => launch_action(r, input),
        };
        items[i] = Some(PlanItem {
            key: r.key,
            label: label(r),
            placement,
            action,
        });
    }
    let items: Vec<PlanItem> = items.into_iter().flatten().collect();

    let workspace_commands = workspace_commands(snap, input.live);

    let mut summary = Summary {
        untouched: assignment.unmatched_candidates.len(),
        ..Default::default()
    };
    for it in &items {
        match &it.action {
            Action::Reuse {
                commands,
                ambiguous,
                ..
            } => {
                summary.reuse += 1;
                summary.to_move += usize::from(!commands.is_empty());
                summary.ambiguous += usize::from(*ambiguous);
            }
            Action::Launch { .. } => summary.launch += 1,
            Action::Unresolved { .. } => summary.unresolved += 1,
            Action::Excluded { .. } => summary.excluded += 1,
        }
    }
    Plan {
        snapshot: snap.name.clone(),
        items,
        workspace_commands,
        summary,
    }
}

fn launch_action(r: &WindowRecord, input: &PlanInput) -> Action {
    let spec = match input
        .overrides
        .iter()
        .find(|o| o.matches(&r.class, &r.initial_class, &r.title))
    {
        Some(o) => match shell_words::split(&o.command) {
            Ok(argv) if !argv.is_empty() => LaunchSpec {
                argv,
                cwd: o.cwd.clone(),
                via: LaunchVia::Override,
            },
            _ => {
                return Action::Unresolved {
                    reason: format!("invalid override command: {}", o.command),
                };
            }
        },
        None => match &r.app.launch {
            Some(spec) => spec.clone(),
            None => {
                return Action::Unresolved {
                    reason: r
                        .app
                        .launch_problem
                        .clone()
                        .unwrap_or_else(|| "no launch command".into()),
                };
            }
        },
    };
    let mut argv = input.launch_wrapper.to_vec();
    argv.extend(spec.argv.iter().cloned());
    if let Some(prog) = argv.first()
        && !(input.runnable)(prog)
    {
        return Action::Unresolved {
            reason: format!("executable not found: {prog}"),
        };
    }
    Action::Launch {
        spec: LaunchSpec { argv, ..spec },
    }
}

/// Bind snapshot workspaces back to their monitors when those monitors exist
/// and more than one is connected.
fn workspace_commands(snap: &Snapshot, live: &LiveState) -> Vec<Command> {
    let monitors: Vec<&str> = live
        .monitors
        .iter()
        .filter(|m| !m.disabled)
        .map(|m| m.name.as_str())
        .collect();
    if monitors.len() < 2 {
        return Vec::new();
    }
    snap.workspaces
        .iter()
        .filter(|w| w.windows > 0 && monitors.contains(&w.monitor.as_str()))
        .filter(|w| {
            live.workspaces
                .iter()
                .find(|lw| {
                    WorkspaceRef {
                        id: lw.id,
                        name: lw.name.clone(),
                    }
                    .same_as(&WorkspaceRef {
                        id: w.id,
                        name: w.name.clone(),
                    })
                })
                .is_none_or(|lw| lw.monitor != w.monitor)
        })
        .map(|w| Command::MoveWorkspaceToMonitor {
            workspace: workspace_target(&WorkspaceRef {
                id: w.id,
                name: w.name.clone(),
            }),
            monitor: w.monitor.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::capture::build_snapshot_at;
    use crate::snapshot::capture::tests::{fixture_discovery, fixture_live};
    use jiff::Timestamp;

    fn setup() -> (LiveState, Vec<ResolvedWindow>, Snapshot) {
        let live = fixture_live();
        let wins = fixture_discovery().windows(&live.clients);
        let snap = build_snapshot_at("s".into(), Timestamp::UNIX_EPOCH, &live, &wins);
        (live, wins, snap)
    }

    fn run(
        snap: &Snapshot,
        live: &LiveState,
        wins: &[ResolvedWindow],
        runnable: &dyn Fn(&str) -> bool,
    ) -> Plan {
        let excl = Exclusions::new(&[], &[], true);
        plan(&PlanInput {
            snapshot: snap,
            live,
            windows: wins,
            overrides: &[],
            exclusions: &excl,
            launch_wrapper: &[],
            runnable,
        })
    }

    #[test]
    fn restoring_current_state_is_a_noop() {
        let (live, wins, snap) = setup();
        let p = run(&snap, &live, &wins, &|_| true);
        assert!(p.is_noop(), "{:#?}", p.summary);
        assert_eq!(p.summary.reuse, 4);
        assert_eq!(p.summary.ambiguous, 0);
    }

    #[test]
    fn moved_window_is_moved_back() {
        let (mut live, _, snap) = setup();
        live.clients[2].workspace = WorkspaceRef {
            id: 7,
            name: "7".into(),
        };
        let wins = fixture_discovery().windows(&live.clients);
        let p = run(&snap, &live, &wins, &|_| true);
        assert_eq!(p.summary.to_move, 1);
        let Action::Reuse { commands, .. } = &p.items[2].action else {
            panic!()
        };
        assert_eq!(
            commands,
            &vec![Command::MoveToWorkspace {
                address: live.clients[2].address.clone(),
                workspace: "3".into()
            }]
        );
    }

    #[test]
    fn missing_windows_are_launched_once_each() {
        // After a reboot only one Chromium window came back.
        let (mut live, _, snap) = setup();
        live.clients.retain(|c| c.class == "chromium");
        live.clients.truncate(1);
        let wins = fixture_discovery().windows(&live.clients);
        let p = run(&snap, &live, &wins, &|_| true);
        assert_eq!(p.summary.reuse, 1);
        assert_eq!(p.summary.launch, 3);
        let launches: Vec<&Vec<String>> = p
            .items
            .iter()
            .filter_map(|i| match &i.action {
                Action::Launch { spec } => Some(&spec.argv),
                _ => None,
            })
            .collect();
        assert!(launches.contains(&&vec![
            "omarchy-launch-webapp".to_string(),
            "https://web.whatsapp.com/".into()
        ]));
        assert!(launches.contains(&&vec!["foot".to_string()]));
    }

    #[test]
    fn missing_executable_is_unresolved_not_fatal() {
        let (mut live, _, snap) = setup();
        live.clients.clear();
        let p = run(&snap, &live, &[], &|prog| prog != "foot");
        assert_eq!(p.summary.unresolved, 1);
        assert_eq!(p.summary.launch, 3);
        let Action::Unresolved { reason } = &p.items[2].action else {
            panic!()
        };
        assert_eq!(reason, "executable not found: foot");
    }

    #[test]
    fn floating_geometry_is_restored_tiled_is_not() {
        let (mut live, _, mut snap) = setup();
        snap.windows[2].floating = true;
        snap.windows[2].at = [100, 100];
        snap.windows[2].size = [800, 600];
        live.clients[0].at = [999, 999]; // tiled window drift is ignored
        let wins = fixture_discovery().windows(&live.clients);
        let p = run(&snap, &live, &wins, &|_| true);
        let Action::Reuse { commands, .. } = &p.items[0].action else {
            panic!()
        };
        assert!(commands.is_empty());
        let Action::Reuse { commands, .. } = &p.items[2].action else {
            panic!()
        };
        let ops: Vec<String> = commands.iter().map(|c| c.to_string()).collect();
        assert_eq!(ops, vec!["float", "size 800x600", "position 100,100"]);
    }
}
