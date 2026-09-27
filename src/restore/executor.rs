//! Carries out a [`Plan`]. The only restore code with side effects, and it
//! reaches the desktop exclusively through [`Compositor`] and [`EventSource`].
//!
//! A failure affects only its own item: every other window is still handled.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde::Serialize;
use tracing::{info, warn};

use crate::hyprland::commands::Command;
use crate::hyprland::events::{Event, EventSource};
use crate::hyprland::ipc::Compositor;
use crate::hyprland::models::Client;
use crate::snapshot::capture::{Discovery, ResolvedWindow};
use crate::snapshot::model::{Snapshot, WindowRecord, workspace_target};

use super::matcher;
use super::planner::{Action, Placement, Plan, PlanItem};

#[derive(Debug, Clone)]
pub struct ExecOptions {
    /// How long to wait for launched windows, overall.
    pub timeout: Duration,
    /// Pause before verifying placement (animations, float transitions).
    pub settle: Duration,
    /// How often to rescan when windows change class/title after mapping.
    pub rescan: Duration,
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            settle: Duration::from_millis(300),
            rescan: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Existing window, already in place.
    Unchanged,
    /// Existing window, moved into place.
    Placed,
    /// Launched, matched and placed.
    Launched,
    SpawnFailed,
    TimedOut,
    /// Commands for this window failed.
    Failed,
    Unresolved,
    Excluded,
}

impl Outcome {
    pub fn is_failure(self) -> bool {
        matches!(
            self,
            Self::SpawnFailed | Self::TimedOut | Self::Failed | Self::Unresolved
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemReport {
    pub key: u32,
    pub label: String,
    pub workspace: String,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub snapshot: String,
    pub items: Vec<ItemReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub workspace_errors: Vec<String>,
    pub elapsed_ms: u128,
}

impl Report {
    pub fn failures(&self) -> usize {
        self.items.iter().filter(|i| i.outcome.is_failure()).count()
    }
}

pub struct Executor<'a> {
    pub compositor: &'a dyn Compositor,
    pub discovery: &'a Discovery,
    pub options: ExecOptions,
}

struct Pending {
    /// Index into `plan.items` / report.
    item: usize,
    record: WindowRecord,
    launched_at: Instant,
}

impl Executor<'_> {
    /// `events` must be connected *before* this call so no `openwindow` is missed.
    pub fn run(&self, plan: &Plan, snapshot: &Snapshot, events: &mut dyn EventSource) -> Report {
        let started = Instant::now();
        let mut reports: Vec<ItemReport> = plan.items.iter().map(initial_report).collect();
        let mut placed: Vec<(usize, String)> = Vec::new();

        // Addresses that existed before we launched anything are never claimed
        // for a launch, whether or not the plan reuses them.
        let preexisting: HashSet<String> = self
            .compositor
            .clients()
            .map(|cs| cs.into_iter().map(|c| c.address).collect())
            .unwrap_or_default();

        // 1. Reused windows.
        for (i, item) in plan.items.iter().enumerate() {
            let Action::Reuse {
                address,
                commands,
                ambiguous,
                ..
            } = &item.action
            else {
                continue;
            };
            reports[i].address = Some(address.clone());
            if *ambiguous {
                reports[i]
                    .warnings
                    .push("multiple candidate windows; picked one".into());
            }
            if commands.is_empty() {
                continue;
            }
            match self.apply(commands) {
                Ok(()) => {
                    info!(event = "move", window = %item.label, address = %address, "placed existing window");
                    reports[i].outcome = Outcome::Placed;
                    placed.push((i, address.clone()));
                }
                Err(e) => {
                    warn!(event = "failure", window = %item.label, error = %e, "placing existing window failed");
                    reports[i].outcome = Outcome::Failed;
                    reports[i].detail = Some(e);
                }
            }
        }

        // 2. Launches.
        let mut pending: Vec<Pending> = Vec::new();
        for (i, item) in plan.items.iter().enumerate() {
            let Action::Launch { spec } = &item.action else {
                continue;
            };
            let cwd = spec.cwd.clone().filter(|d| d.is_dir());
            let cmd = Command::Exec {
                argv: spec.argv.clone(),
                cwd,
                workspace: Some(workspace_target(&item.placement.workspace)),
            };
            match self.apply(std::slice::from_ref(&cmd)) {
                Ok(()) => {
                    info!(event = "launch", window = %item.label, command = %shell_words::join(&spec.argv), "launched");
                    let record = snapshot
                        .windows
                        .iter()
                        .find(|r| r.key == item.key)
                        .cloned()
                        .expect("plan items come from this snapshot");
                    pending.push(Pending {
                        item: i,
                        record,
                        launched_at: Instant::now(),
                    });
                }
                Err(e) => {
                    warn!(event = "failure", window = %item.label, error = %e, "launch failed");
                    reports[i].outcome = Outcome::SpawnFailed;
                    reports[i].detail = Some(e);
                }
            }
        }

        // 3. Wait for launched windows and claim them.
        let mut claimed: HashSet<String> = preexisting;
        let deadline = Instant::now() + self.options.timeout;
        let mut dirty = !pending.is_empty();
        while !pending.is_empty() && Instant::now() < deadline {
            if dirty {
                dirty = false;
                if let Err(e) =
                    self.claim_new(plan, &mut pending, &mut claimed, &mut reports, &mut placed)
                {
                    warn!(event = "failure", error = %e, "querying windows failed");
                }
                if pending.is_empty() {
                    break;
                }
            }
            let tick = (Instant::now() + self.options.rescan).min(deadline);
            match events.next_before(tick) {
                Ok(Some(Event::OpenWindow { .. })) => dirty = true,
                Ok(Some(_)) => {}
                // Periodic rescan: some apps set their class after mapping.
                Ok(None) => dirty = true,
                Err(e) => {
                    warn!(event = "failure", error = %e, "event socket failed; polling instead");
                    std::thread::sleep(
                        self.options
                            .rescan
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                    dirty = true;
                }
            }
        }
        for p in pending {
            let item = &plan.items[p.item];
            warn!(event = "timeout", window = %item.label, waited_ms = p.launched_at.elapsed().as_millis() as u64, "window never appeared");
            reports[p.item].outcome = Outcome::TimedOut;
            reports[p.item].detail = Some(format!(
                "no matching window within {}s",
                self.options.timeout.as_secs()
            ));
        }

        // 4. Verify, re-applying once: some properties (e.g. position right after
        // floating) do not stick on the first try.
        if !placed.is_empty() {
            for round in 0..2 {
                std::thread::sleep(self.options.settle);
                let Ok(clients) = self.compositor.clients() else {
                    break;
                };
                let mut drift = false;
                for (i, address) in &placed {
                    let Some(c) = clients.iter().find(|c| &c.address == address) else {
                        continue;
                    };
                    let fix = plan.items[*i].placement.commands(c);
                    if fix.is_empty() {
                        continue;
                    }
                    drift = true;
                    if round == 0 {
                        let _ = self.apply(&fix);
                    } else {
                        let ops: Vec<String> = fix.iter().map(|c| c.to_string()).collect();
                        reports[*i]
                            .warnings
                            .push(format!("did not reach: {}", ops.join(", ")));
                    }
                }
                if !drift {
                    break;
                }
            }
        }

        // 5. Workspaces back on their monitors.
        let mut workspace_errors = Vec::new();
        if !plan.workspace_commands.is_empty() {
            match self.compositor.dispatch(&plan.workspace_commands) {
                Ok(results) => {
                    for (c, r) in plan.workspace_commands.iter().zip(results) {
                        if let Err(e) = r {
                            workspace_errors.push(format!("{c}: {e}"));
                        }
                    }
                }
                Err(e) => workspace_errors.push(e.to_string()),
            }
        }

        Report {
            snapshot: plan.snapshot.clone(),
            items: reports,
            workspace_errors,
            elapsed_ms: started.elapsed().as_millis(),
        }
    }

    fn claim_new(
        &self,
        plan: &Plan,
        pending: &mut Vec<Pending>,
        claimed: &mut HashSet<String>,
        reports: &mut [ItemReport],
        placed: &mut Vec<(usize, String)>,
    ) -> anyhow::Result<()> {
        let clients = self.compositor.clients()?;
        let fresh: Vec<ResolvedWindow> = self
            .discovery
            .windows(&clients)
            .into_iter()
            .filter(|w| !claimed.contains(&w.client.address) && w.excluded.is_none())
            .collect();
        if fresh.is_empty() {
            return Ok(());
        }
        let records: Vec<WindowRecord> = pending.iter().map(|p| p.record.clone()).collect();
        let assignment = matcher::assign(&records, &fresh);
        let mut done: Vec<usize> = Vec::new();
        for m in &assignment.matches {
            let p = &pending[m.expected];
            let w = &fresh[m.candidate];
            let item = &plan.items[p.item];
            claimed.insert(w.client.address.clone());
            info!(
                event = "match", window = %item.label, address = %w.client.address,
                strategy = %m.strategy, confidence = %m.confidence, "matched launched window"
            );
            let r = &mut reports[p.item];
            r.address = Some(w.client.address.clone());
            if m.ambiguous {
                r.warnings
                    .push("multiple candidate windows; picked one".into());
            }
            match self.place(&item.placement, &w.client) {
                Ok(()) => {
                    r.outcome = Outcome::Launched;
                    placed.push((p.item, w.client.address.clone()));
                }
                Err(e) => {
                    r.outcome = Outcome::Failed;
                    r.detail = Some(e);
                }
            }
            done.push(m.expected);
        }
        // New windows that belong to no pending item stay where they opened.
        for &ci in &assignment.unmatched_candidates {
            let c = &fresh[ci].client;
            claimed.insert(c.address.clone());
            info!(event = "duplicate", class = %c.class, address = %c.address, "unexpected new window left alone");
        }
        done.sort_unstable();
        for i in done.into_iter().rev() {
            pending.remove(i);
        }
        Ok(())
    }

    fn place(&self, placement: &Placement, client: &Client) -> Result<(), String> {
        self.apply(&placement.commands(client))
    }

    /// Runs commands; the first error (if any) is returned as text.
    fn apply(&self, commands: &[Command]) -> Result<(), String> {
        if commands.is_empty() {
            return Ok(());
        }
        let results = self
            .compositor
            .dispatch(commands)
            .map_err(|e| e.to_string())?;
        let errs: Vec<String> = commands
            .iter()
            .zip(results)
            .filter_map(|(c, r)| r.err().map(|e| format!("{c}: {e}")))
            .collect();
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; "))
        }
    }
}

fn initial_report(item: &PlanItem) -> ItemReport {
    let (outcome, detail) = match &item.action {
        Action::Reuse { .. } => (Outcome::Unchanged, None),
        // Replaced as soon as the launch is attempted.
        Action::Launch { .. } => (Outcome::TimedOut, None),
        Action::Unresolved { reason } => (Outcome::Unresolved, Some(reason.clone())),
        Action::Excluded { reason } => (Outcome::Excluded, Some(reason.clone())),
    };
    ItemReport {
        key: item.key,
        label: item.label.clone(),
        workspace: item.placement.workspace.name.clone(),
        outcome,
        address: None,
        detail,
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::resolve::{Confidence, LaunchSpec, LaunchVia};
    use crate::hyprland::models::{LiveState, WorkspaceRef};
    use crate::policy::exclusions::Exclusions;
    use crate::restore::matcher::Strategy;
    use crate::restore::planner::{PlanInput, Summary, label, plan};
    use crate::snapshot::capture::build_snapshot_at;
    use crate::snapshot::capture::tests::{fixture_discovery, fixture_live};
    use jiff::Timestamp;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// In-memory Hyprland: `Exec` spawns a window from a template (or none),
    /// window commands mutate state, and every spawn emits `openwindow`.
    type Spawner = Box<dyn Fn(&[String]) -> Option<Client>>;
    type Rejecter = Box<dyn Fn(&Command) -> Option<String>>;

    struct Fake {
        live: RefCell<LiveState>,
        events: Rc<RefCell<VecDeque<Event>>>,
        spawn: Spawner,
        next_addr: RefCell<u64>,
        log: RefCell<Vec<Command>>,
        /// `clients()` answers this many more times, then fails.
        clients_left: Cell<usize>,
        /// Window commands succeed without changing anything.
        frozen: Cell<bool>,
        /// Per-command error, checked before anything else.
        reject: Rejecter,
    }

    impl Compositor for Fake {
        fn clients(&self) -> anyhow::Result<Vec<Client>> {
            let left = self.clients_left.get();
            anyhow::ensure!(left > 0, "compositor went away");
            self.clients_left.set(left - 1);
            Ok(self.live.borrow().clients.clone())
        }
        fn live_state(&self) -> anyhow::Result<LiveState> {
            Ok(self.live.borrow().clone())
        }
        fn dispatch(&self, commands: &[Command]) -> anyhow::Result<Vec<Result<(), String>>> {
            let mut out = Vec::new();
            for c in commands {
                self.log.borrow_mut().push(c.clone());
                if let Command::MoveWorkspaceToMonitor { monitor, .. } = c
                    && monitor == "GONE"
                {
                    anyhow::bail!("socket closed");
                }
                if let Some(e) = (self.reject)(c) {
                    out.push(Err(e));
                    continue;
                }
                if self.frozen.get() && !matches!(c, Command::Exec { .. }) {
                    out.push(Ok(()));
                    continue;
                }
                let mut live = self.live.borrow_mut();
                let find = |a: &str, live: &mut LiveState| {
                    live.clients.iter_mut().position(|c| c.address == a)
                };
                let r = match c {
                    Command::Exec {
                        argv, workspace, ..
                    } => {
                        if argv[0] == "fail" {
                            Err("spawn error".into())
                        } else {
                            if let Some(mut cl) = (self.spawn)(argv) {
                                let mut n = self.next_addr.borrow_mut();
                                *n += 1;
                                cl.address = format!("0xnew{n}");
                                let ws = workspace.clone().unwrap_or_default();
                                cl.workspace = WorkspaceRef {
                                    id: ws.parse().unwrap_or(1),
                                    name: ws,
                                };
                                self.events.borrow_mut().push_back(Event::OpenWindow {
                                    address: cl.address.clone(),
                                    workspace: cl.workspace.name.clone(),
                                    class: cl.class.clone(),
                                    title: cl.title.clone(),
                                });
                                live.clients.push(cl);
                            }
                            Ok(())
                        }
                    }
                    Command::MoveToWorkspace { address, workspace } => {
                        match find(address, &mut live) {
                            Some(i) => {
                                live.clients[i].workspace = WorkspaceRef {
                                    id: workspace.parse().unwrap_or(1),
                                    name: workspace.clone(),
                                };
                                Ok(())
                            }
                            None => Err("no such window".into()),
                        }
                    }
                    Command::SetFloating { address, floating } => find(address, &mut live)
                        .map(|i| live.clients[i].floating = *floating)
                        .ok_or("x".into()),
                    Command::MoveExact { address, x, y } => find(address, &mut live)
                        .map(|i| live.clients[i].at = [*x, *y])
                        .ok_or("x".into()),
                    Command::ResizeExact { address, w, h } => find(address, &mut live)
                        .map(|i| live.clients[i].size = [*w, *h])
                        .ok_or("x".into()),
                    _ => Ok(()),
                };
                out.push(r);
            }
            Ok(out)
        }
    }

    struct FakeEvents(Rc<RefCell<VecDeque<Event>>>);

    impl EventSource for FakeEvents {
        fn next_before(&mut self, deadline: Instant) -> anyhow::Result<Option<Event>> {
            let next = self.0.borrow_mut().pop_front();
            if let Some(Event::CloseWindow { address }) = next {
                anyhow::bail!("event socket closed after {address}");
            }
            if let Some(e) = next {
                return Ok(Some(e));
            }
            std::thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(2)),
            );
            Ok(None)
        }
    }

    fn opts() -> ExecOptions {
        ExecOptions {
            timeout: Duration::from_millis(100),
            settle: Duration::from_millis(1),
            rescan: Duration::from_millis(5),
        }
    }

    /// Snapshot of the fixture desktop, then a "reboot" leaving `keep` clients.
    fn scenario(
        keep: impl Fn(&Client) -> bool,
        spawn: impl Fn(&[String]) -> Option<Client> + 'static,
    ) -> (Snapshot, Fake) {
        let live = fixture_live();
        let snap = build_snapshot_at(
            "s".into(),
            Timestamp::UNIX_EPOCH,
            &live,
            &fixture_discovery().windows(&live.clients),
        );
        let mut after = live.clone();
        after.clients.retain(|c| keep(c));
        let fake = Fake {
            live: RefCell::new(after),
            events: Rc::new(RefCell::new(VecDeque::new())),
            spawn: Box::new(spawn),
            next_addr: RefCell::new(0),
            log: RefCell::new(Vec::new()),
            clients_left: Cell::new(usize::MAX),
            frozen: Cell::new(false),
            reject: Box::new(|_| None),
        };
        (snap, fake)
    }

    /// Logs on, so the fields of `info!`/`warn!` events are evaluated too.
    fn logging() -> tracing::subscriber::DefaultGuard {
        tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::DEBUG)
                .with_test_writer()
                .finish(),
        )
    }

    fn run_plan(p: &Plan, snap: &Snapshot, fake: &Fake) -> Report {
        let _log = logging();
        let d = fixture_discovery();
        let ex = Executor {
            compositor: fake,
            discovery: &d,
            options: opts(),
        };
        ex.run(p, snap, &mut FakeEvents(fake.events.clone()))
    }

    fn execute(snap: &Snapshot, fake: &Fake, runnable: &dyn Fn(&str) -> bool) -> Report {
        let _log = logging();
        let d = fixture_discovery();
        let live = fake.live_state().unwrap();
        let wins = d.windows(&live.clients);
        let excl = Exclusions::new(&[], &[], true);
        let p = plan(&PlanInput {
            snapshot: snap,
            live: &live,
            windows: &wins,
            overrides: &[],
            exclusions: &excl,
            launch_wrapper: &[],
            runnable,
        });
        let ex = Executor {
            compositor: fake,
            discovery: &d,
            options: opts(),
        };
        ex.run(&p, snap, &mut FakeEvents(fake.events.clone()))
    }

    /// Spawns a window looking like the fixture client with this class.
    fn spawn_like(class: &'static str, pid: i32) -> impl Fn(&[String]) -> Option<Client> {
        move |_| {
            let mut c = fixture_live()
                .clients
                .into_iter()
                .find(|c| c.class == class)
                .unwrap();
            c.pid = pid;
            c.workspace = WorkspaceRef {
                id: 9,
                name: "9".into(),
            };
            Some(c)
        }
    }

    #[test]
    fn launches_matches_and_places_missing_window() {
        // Terminal gone after reboot; Chromium windows still there.
        let (snap, fake) = scenario(|c| c.class != "foot", spawn_like("foot", 147550));
        let r = execute(&snap, &fake, &|_| true);
        let foot = r.items.iter().find(|i| i.label == "foot").unwrap();
        assert_eq!(foot.outcome, Outcome::Launched, "{r:#?}");
        let placed = fake
            .live
            .borrow()
            .clients
            .iter()
            .find(|c| c.class == "foot")
            .unwrap()
            .workspace
            .id;
        assert_eq!(placed, 3);
        assert_eq!(r.failures(), 0);
        // Nothing else launched: no duplicates.
        let execs = fake
            .log
            .borrow()
            .iter()
            .filter(|c| matches!(c, Command::Exec { .. }))
            .count();
        assert_eq!(execs, 1);
    }

    #[test]
    fn second_restore_does_nothing() {
        let (snap, fake) = scenario(|c| c.class != "foot", spawn_like("foot", 147550));
        execute(&snap, &fake, &|_| true);
        fake.log.borrow_mut().clear();
        let r = execute(&snap, &fake, &|_| true);
        assert!(fake.log.borrow().is_empty(), "{:?}", fake.log.borrow());
        assert!(r.items.iter().all(|i| i.outcome == Outcome::Unchanged));
    }

    #[test]
    fn one_failure_does_not_stop_the_rest() {
        // Everything gone; WhatsApp never shows up, terminal spawns fine.
        let (snap, fake) = scenario(
            |_| false,
            |argv: &[String]| {
                if argv[0] == "foot" {
                    spawn_like("foot", 147550)(argv)
                } else {
                    None
                }
            },
        );
        let r = execute(&snap, &fake, &|_| true);
        let by = |l: &str| r.items.iter().find(|i| i.label == l).unwrap().outcome;
        assert_eq!(by("foot"), Outcome::Launched);
        assert_eq!(by("web.whatsapp.com"), Outcome::TimedOut);
        assert_eq!(r.failures(), 3); // whatsapp + 2 chromium windows
    }

    #[test]
    fn preexisting_unclaimed_windows_are_never_grabbed() {
        // A stray foot exists on ws 5 but the snapshot's foot... is matched to
        // it by the planner (reuse), so no launch and no duplicate.
        let (snap, fake) = scenario(|_| true, |_| None);
        fake.live.borrow_mut().clients[2].workspace = WorkspaceRef {
            id: 5,
            name: "5".into(),
        };
        let r = execute(&snap, &fake, &|_| true);
        let foot = r.items.iter().find(|i| i.label == "foot").unwrap();
        assert_eq!(foot.outcome, Outcome::Placed);
        assert!(
            !fake
                .log
                .borrow()
                .iter()
                .any(|c| matches!(c, Command::Exec { .. }))
        );
    }

    fn item(snap: &Snapshot, i: usize, action: Action) -> PlanItem {
        let r = &snap.windows[i];
        PlanItem {
            key: r.key,
            label: label(r),
            placement: Placement::of(r),
            action,
        }
    }

    fn plan_of(items: Vec<PlanItem>, workspace_commands: Vec<Command>) -> Plan {
        Plan {
            snapshot: "s".into(),
            items,
            workspace_commands,
            summary: Summary::default(),
        }
    }

    fn reuse(address: &str, commands: Vec<Command>, ambiguous: bool) -> Action {
        Action::Reuse {
            address: address.into(),
            strategy: Strategy::Identity,
            confidence: Confidence::High,
            ambiguous,
            commands,
        }
    }

    fn launch(program: &str) -> Action {
        Action::Launch {
            spec: LaunchSpec {
                argv: vec![program.into()],
                cwd: Some("/nonexistent/dir".into()),
                via: LaunchVia::Cmdline,
            },
        }
    }

    fn move_to(address: &str, ws: &str) -> Command {
        Command::MoveToWorkspace {
            address: address.into(),
            workspace: ws.into(),
        }
    }

    fn to_monitor(monitor: &str) -> Command {
        Command::MoveWorkspaceToMonitor {
            workspace: "1".into(),
            monitor: monitor.into(),
        }
    }

    #[test]
    fn default_options() {
        let o = ExecOptions::default();
        assert_eq!(o.timeout, Duration::from_secs(30));
        assert!(o.settle < o.rescan);
    }

    #[test]
    fn every_kind_of_failure_is_reported_per_item() {
        let (snap, mut fake) = scenario(|_| true, |_| None);
        fake.reject = Box::new(|c| {
            matches!(c, Command::MoveWorkspaceToMonitor { monitor, .. } if monitor == "HDMI-A-1")
                .then(|| "no such monitor".into())
        });
        let p = plan_of(
            vec![
                item(
                    &snap,
                    0,
                    reuse("0xgone", vec![move_to("0xgone", "2")], true),
                ),
                item(
                    &snap,
                    1,
                    Action::Unresolved {
                        reason: "why".into(),
                    },
                ),
                item(
                    &snap,
                    2,
                    Action::Excluded {
                        reason: "policy".into(),
                    },
                ),
                item(&snap, 3, launch("fail")),
            ],
            vec![to_monitor("DP-1"), to_monitor("HDMI-A-1")],
        );
        let r = run_plan(&p, &snap, &fake);
        let outcomes: Vec<Outcome> = r.items.iter().map(|i| i.outcome).collect();
        assert_eq!(
            outcomes,
            [
                Outcome::Failed,
                Outcome::Unresolved,
                Outcome::Excluded,
                Outcome::SpawnFailed
            ]
        );
        assert_eq!(
            r.items[0].detail.as_deref(),
            Some("move → workspace 2: no such window")
        );
        assert_eq!(
            r.items[0].warnings,
            ["multiple candidate windows; picked one"]
        );
        assert_eq!(r.items[1].detail.as_deref(), Some("why"));
        assert!(
            r.items[3]
                .detail
                .as_deref()
                .unwrap()
                .ends_with("spawn error")
        );
        assert_eq!(
            r.workspace_errors,
            ["workspace 1 → monitor HDMI-A-1: no such monitor"]
        );
        assert_eq!(r.failures(), 3);
    }

    #[test]
    fn unreachable_compositor_fails_workspace_commands() {
        let (snap, fake) = scenario(|_| true, |_| None);
        let r = run_plan(&plan_of(vec![], vec![to_monitor("GONE")]), &snap, &fake);
        assert_eq!(r.workspace_errors, ["socket closed"]);
    }

    #[test]
    fn placement_that_does_not_stick_is_retried_then_reported() {
        let (snap, fake) = scenario(|_| true, |_| None);
        fake.live.borrow_mut().clients[2].workspace = WorkspaceRef {
            id: 5,
            name: "5".into(),
        };
        fake.frozen.set(true);
        let r = execute(&snap, &fake, &|_| true);
        let foot = r.items.iter().find(|i| i.label == "foot").unwrap();
        assert_eq!(foot.outcome, Outcome::Placed);
        assert_eq!(foot.warnings, ["did not reach: move → workspace 3"]);
        let moves = fake
            .log
            .borrow()
            .iter()
            .filter(|c| matches!(c, Command::MoveToWorkspace { .. }))
            .count();
        assert_eq!(moves, 2, "applied, then re-applied once");
    }

    #[test]
    fn verification_tolerates_vanished_windows_and_a_lost_compositor() {
        let (snap, fake) = scenario(|_| true, |_| None);
        fake.frozen.set(true);
        let p = plan_of(
            vec![item(
                &snap,
                0,
                reuse("0xghost", vec![move_to("0xghost", "2")], false),
            )],
            vec![],
        );
        let r = run_plan(&p, &snap, &fake);
        assert_eq!(r.items[0].outcome, Outcome::Placed);
        assert!(r.items[0].warnings.is_empty());

        // Only the initial query succeeds.
        fake.clients_left.set(1);
        let r = run_plan(&p, &snap, &fake);
        assert_eq!(r.items[0].outcome, Outcome::Placed);
    }

    #[test]
    fn event_and_query_failures_fall_back_to_polling() {
        let (snap, fake) = scenario(|c| c.class != "foot", |_| None);
        fake.events.borrow_mut().extend([
            Event::Other {
                name: "workspace".into(),
                data: "1".into(),
            },
            Event::CloseWindow {
                address: "0x1".into(),
            },
        ]);
        fake.clients_left.set(1);
        let p = plan_of(vec![item(&snap, 2, launch("foot"))], vec![]);
        let r = run_plan(&p, &snap, &fake);
        assert_eq!(r.items[0].outcome, Outcome::TimedOut);
        assert!(fake.events.borrow().is_empty());
    }

    #[test]
    fn launched_window_that_cannot_be_placed_fails() {
        let (mut snap, mut fake) = scenario(|c| c.class != "foot", spawn_like("foot", 147550));
        snap.windows[2].floating = true;
        fake.reject =
            Box::new(|c| matches!(c, Command::SetFloating { .. }).then(|| "cannot float".into()));
        let r = execute(&snap, &fake, &|_| true);
        let foot = r.items.iter().find(|i| i.label == "foot").unwrap();
        assert_eq!(foot.outcome, Outcome::Failed);
        assert_eq!(foot.detail.as_deref(), Some("float: cannot float"));
    }

    #[test]
    fn indistinguishable_launches_are_flagged_and_strays_left_alone() {
        let (mut snap, fake) = scenario(
            |_| false,
            |argv: &[String]| match argv[0].as_str() {
                "foot" => spawn_like("foot", 147550)(argv),
                _ => {
                    let mut c = spawn_like("foot", 0)(argv).unwrap();
                    c.class = "stray".into();
                    c.initial_class = "stray".into();
                    Some(c)
                }
            },
        );
        for (key, ws) in [(10, "x"), (11, "y"), (12, "z")] {
            let mut r = snap.windows[2].clone();
            r.key = key;
            r.workspace = WorkspaceRef {
                id: 0,
                name: ws.into(),
            };
            snap.windows.push(r);
        }
        let p = plan_of(
            vec![
                item(&snap, 4, launch("foot")),
                item(&snap, 5, launch("foot")),
                item(&snap, 6, launch("stray")),
            ],
            vec![],
        );
        let r = run_plan(&p, &snap, &fake);
        let outcomes: Vec<Outcome> = r.items.iter().map(|i| i.outcome).collect();
        assert_eq!(
            outcomes,
            [Outcome::Launched, Outcome::Launched, Outcome::TimedOut]
        );
        for i in &r.items[..2] {
            assert!(
                i.warnings
                    .contains(&"multiple candidate windows; picked one".into())
            );
        }
    }

    #[test]
    fn launched_floating_window_gets_its_geometry() {
        let (mut snap, fake) = scenario(|c| c.class != "foot", spawn_like("foot", 147550));
        let foot = &mut snap.windows[2];
        foot.floating = true;
        foot.at = [10, 20];
        foot.size = [300, 200];
        let r = execute(&snap, &fake, &|_| true);
        assert_eq!(r.failures(), 0, "{r:#?}");
        let live = fake.live.borrow();
        let c = live.clients.iter().find(|c| c.class == "foot").unwrap();
        assert!(c.floating);
        assert_eq!((c.at, c.size), ([10, 20], [300, 200]));
    }

    #[test]
    fn unreachable_compositor_fails_the_window() {
        let (snap, fake) = scenario(|_| true, |_| None);
        let p = plan_of(
            vec![item(
                &snap,
                0,
                reuse("0x1", vec![to_monitor("GONE")], false),
            )],
            vec![],
        );
        let r = run_plan(&p, &snap, &fake);
        assert_eq!(r.items[0].outcome, Outcome::Failed);
        assert_eq!(r.items[0].detail.as_deref(), Some("socket closed"));
    }
}
