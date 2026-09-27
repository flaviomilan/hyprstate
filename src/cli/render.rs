//! Human-readable output. Machine-readable output is `--json`.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

use crate::diff::Diff;
use crate::restore::executor::{Outcome, Report};
use crate::restore::planner::{Action, Plan, label};
use crate::snapshot::model::{Snapshot, WindowRecord};
use crate::snapshot::storage::Listed;
use crate::workset::Workset;

/// Sort key putting numbered workspaces first, then named, then special.
fn ws_order(id: i64, name: &str) -> (u8, i64, String) {
    if id > 0 {
        (0, id, String::new())
    } else if name.starts_with("special") {
        (2, 0, name.to_string())
    } else {
        (1, 0, name.to_string())
    }
}

fn home_relative(p: &Path) -> String {
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(h) if p.starts_with(&h) => format!("~/{}", p.strip_prefix(&h).unwrap().display()),
        _ => p.display().to_string(),
    }
}

fn local_time(ts: &jiff::Timestamp) -> String {
    ts.to_zoned(jiff::tz::TimeZone::system())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

pub fn saved(s: &Snapshot, path: &Path) -> String {
    let mut o = String::new();
    let wss = s
        .windows
        .iter()
        .map(|w| &w.workspace.name)
        .collect::<std::collections::HashSet<_>>()
        .len();
    writeln!(o, "Saved snapshot {}", s.name).unwrap();
    write!(o, "  {} windows on {} workspaces", s.windows.len(), wss).unwrap();
    if s.excluded_count > 0 {
        write!(o, " ({} excluded by policy)", s.excluded_count).unwrap();
    }
    writeln!(o, "\n  {}", home_relative(path)).unwrap();
    o
}

pub fn list(list: &[Listed], dir: &Path) -> String {
    if list.is_empty() {
        return format!("No snapshots in {}\n", home_relative(dir));
    }
    let w = list.iter().map(|l| l.name.len()).max().unwrap_or(4).max(4);
    let mut o = format!("{:<w$}  {:<16}  WINDOWS\n", "NAME", "CREATED");
    for l in list {
        writeln!(
            o,
            "{:<w$}  {:<16}  {}",
            l.name,
            local_time(&l.snapshot.created_at),
            l.snapshot.windows.len()
        )
        .unwrap();
    }
    o
}

fn by_workspace<'a, T>(
    items: impl Iterator<Item = (i64, &'a str, T)>,
) -> BTreeMap<(u8, i64, String), (String, Vec<T>)> {
    let mut m: BTreeMap<(u8, i64, String), (String, Vec<T>)> = BTreeMap::new();
    for (id, name, t) in items {
        m.entry(ws_order(id, name))
            .or_insert_with(|| (name.to_string(), Vec::new()))
            .1
            .push(t);
    }
    m
}

pub fn inspect(s: &Snapshot, windows: bool) -> String {
    let mut o = String::new();
    writeln!(o, "Hyprland\n  version: {}\n", s.hyprland.version).unwrap();
    writeln!(o, "Monitors").unwrap();
    for m in &s.monitors {
        writeln!(
            o,
            "  {} {}x{} @ {},{} (scale {})",
            m.name, m.width, m.height, m.x, m.y, m.scale
        )
        .unwrap();
    }
    let groups = by_workspace(
        s.windows
            .iter()
            .map(|w| (w.workspace.id, w.workspace.name.as_str(), w)),
    );
    writeln!(o, "\nWorkspaces").unwrap();
    for (name, ws) in groups.values() {
        let n = ws.len();
        writeln!(o, "  {name} → {n} window{}", if n == 1 { "" } else { "s" }).unwrap();
    }
    if s.excluded_count > 0 {
        writeln!(o, "  ({} excluded by policy)", s.excluded_count).unwrap();
    }
    if !windows {
        let mut apps: BTreeMap<String, usize> = BTreeMap::new();
        for w in &s.windows {
            *apps.entry(label(w)).or_default() += 1;
        }
        writeln!(o, "\nApplications").unwrap();
        for (app, n) in apps {
            if n > 1 {
                writeln!(o, "  {app} ×{n}").unwrap();
            } else {
                writeln!(o, "  {app}").unwrap();
            }
        }
        return o;
    }
    for (name, ws) in groups.values() {
        writeln!(o, "\nWorkspace {name}").unwrap();
        for w in ws {
            window_detail(&mut o, w);
        }
    }
    o
}

fn window_detail(o: &mut String, w: &WindowRecord) {
    let a = &w.app;
    writeln!(o, "  {}  \"{}\"", label(w), truncate(&w.title, 60)).unwrap();
    writeln!(
        o,
        "    class:      {} (initial {})",
        w.class, w.initial_class
    )
    .unwrap();
    let mut state = vec![if w.floating { "floating" } else { "tiled" }.to_string()];
    if w.floating {
        state.push(format!(
            "{}x{} @ {},{}",
            w.size[0], w.size[1], w.at[0], w.at[1]
        ));
    }
    if w.fullscreen > 0 {
        state.push(format!("fullscreen {}", w.fullscreen));
    }
    if w.pinned {
        state.push("pinned".into());
    }
    if w.xwayland {
        state.push("xwayland".into());
    }
    writeln!(o, "    state:      {}", state.join(", ")).unwrap();
    writeln!(o, "    app:        {}", a.identity).unwrap();
    writeln!(o, "    resolved:   {}", a.source).unwrap();
    writeln!(o, "    confidence: {}", a.confidence).unwrap();
    match (&a.launch, &a.launch_problem) {
        (Some(l), _) => writeln!(o, "    launch:     {}", shell_words::join(&l.argv)).unwrap(),
        (None, Some(p)) => writeln!(o, "    launch:     ✗ {p}").unwrap(),
        (None, None) => writeln!(o, "    launch:     ✗ unknown").unwrap(),
    }
    if let Some(cwd) = &a.cwd {
        writeln!(o, "    cwd:        {}", home_relative(cwd)).unwrap();
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

pub fn plan(p: &Plan, s: &Snapshot) -> String {
    let mut o = match p.snapshot.strip_prefix("workset:") {
        Some(name) => format!("Workset: {name}\n"),
        None => format!("Snapshot: {} ({})\n", p.snapshot, local_time(&s.created_at)),
    };
    let groups = by_workspace(p.items.iter().map(|i| {
        (
            i.placement.workspace.id,
            i.placement.workspace.name.as_str(),
            i,
        )
    }));
    for (name, items) in groups.values() {
        writeln!(o, "\nWorkspace {name}").unwrap();
        for it in items {
            match &it.action {
                Action::Reuse {
                    commands,
                    ambiguous,
                    confidence,
                    strategy,
                    ..
                } => {
                    let mark = if *ambiguous { "⚠" } else { "✓" };
                    if commands.is_empty() {
                        writeln!(o, "  {mark} {}", it.label).unwrap();
                    } else {
                        let ops: Vec<String> = commands.iter().map(|c| c.to_string()).collect();
                        writeln!(o, "  {mark} {}  ({})", it.label, ops.join(", ")).unwrap();
                    }
                    if *ambiguous {
                        writeln!(o, "      reason: multiple candidate windows").unwrap();
                    }
                    if *confidence == crate::discovery::resolve::Confidence::Low {
                        writeln!(o, "      match: {strategy}, confidence {confidence}").unwrap();
                    }
                }
                Action::Launch { spec } => {
                    writeln!(
                        o,
                        "  + {}  (launch: {})",
                        it.label,
                        shell_words::join(&spec.argv)
                    )
                    .unwrap();
                }
                Action::Unresolved { reason } => {
                    writeln!(o, "  ✗ {}\n      reason: {reason}", it.label).unwrap();
                }
                Action::Excluded { reason } => {
                    writeln!(o, "  - {}  (skipped: {reason})", it.label).unwrap();
                }
            }
        }
    }
    if !p.workspace_commands.is_empty() {
        writeln!(o, "\nWorkspaces").unwrap();
        for c in &p.workspace_commands {
            writeln!(o, "  {c}").unwrap();
        }
    }
    let s = &p.summary;
    writeln!(o, "\nActions:").unwrap();
    writeln!(o, "  launch: {}", s.launch).unwrap();
    writeln!(o, "  reuse: {} ({} to move)", s.reuse, s.to_move).unwrap();
    writeln!(o, "  unresolved: {}", s.unresolved).unwrap();
    if s.ambiguous > 0 {
        writeln!(o, "  ambiguous: {}", s.ambiguous).unwrap();
    }
    if s.excluded > 0 {
        writeln!(o, "  excluded: {}", s.excluded).unwrap();
    }
    if s.untouched > 0 {
        writeln!(o, "  other windows left alone: {}", s.untouched).unwrap();
    }
    if p.is_noop() {
        let what = if p.snapshot.starts_with("workset:") {
            "workset"
        } else {
            "snapshot"
        };
        writeln!(o, "\nDesktop already matches the {what}.").unwrap();
    }
    o
}

pub fn report(r: &Report) -> String {
    let mut o = format!("Restored {}\n", r.snapshot);
    let groups = by_workspace(
        r.items
            .iter()
            .map(|i| (i.workspace.parse().unwrap_or(0), i.workspace.as_str(), i)),
    );
    for (name, items) in groups.values() {
        writeln!(o, "\nWorkspace {name}").unwrap();
        for it in items {
            let (mark, what) = match it.outcome {
                Outcome::Unchanged => ("✓", ""),
                Outcome::Placed => ("✓", "moved"),
                Outcome::Launched => ("✓", "launched"),
                Outcome::SpawnFailed => ("✗", "launch failed"),
                Outcome::TimedOut => ("✗", "window did not appear"),
                Outcome::Failed => ("✗", "could not place"),
                Outcome::Unresolved => ("✗", "unresolved"),
                Outcome::Excluded => ("-", "excluded"),
            };
            let mark = if mark == "✓" && !it.warnings.is_empty() {
                "⚠"
            } else {
                mark
            };
            if what.is_empty() {
                writeln!(o, "  {mark} {}", it.label).unwrap();
            } else {
                writeln!(o, "  {mark} {}  ({what})", it.label).unwrap();
            }
            if let Some(d) = &it.detail {
                writeln!(o, "      reason: {d}").unwrap();
            }
            for w in &it.warnings {
                writeln!(o, "      warning: {w}").unwrap();
            }
        }
    }
    for e in &r.workspace_errors {
        writeln!(o, "\n⚠ workspace: {e}").unwrap();
    }
    let count = |f: fn(Outcome) -> bool| r.items.iter().filter(|i| f(i.outcome)).count();
    writeln!(o, "\nResult:").unwrap();
    writeln!(o, "  launched: {}", count(|x| x == Outcome::Launched)).unwrap();
    writeln!(
        o,
        "  reused: {}",
        count(|x| matches!(x, Outcome::Unchanged | Outcome::Placed))
    )
    .unwrap();
    writeln!(o, "  failed: {}", r.failures()).unwrap();
    writeln!(o, "  time: {:.1}s", r.elapsed_ms as f64 / 1000.0).unwrap();
    o
}

pub fn diff(d: &Diff) -> String {
    if d.is_empty() {
        return format!("No differences between {} and {}\n", d.from, d.to);
    }
    let mut o = format!("{} → {}\n", d.from, d.to);
    let adds = d.added.iter().map(|w| (w, '+'));
    let rems = d.removed.iter().map(|w| (w, '-'));
    let groups = by_workspace(adds.chain(rems).map(|(w, c)| {
        (
            w.workspace.parse().unwrap_or(0),
            w.workspace.as_str(),
            (w, c),
        )
    }));
    for (name, items) in groups.values() {
        writeln!(o, "\nWorkspace {name}").unwrap();
        for (w, c) in items {
            writeln!(o, "  {c} {}  \"{}\"", w.label, truncate(&w.title, 50)).unwrap();
        }
    }
    for ch in &d.changed {
        writeln!(
            o,
            "\n{}  \"{}\"",
            ch.window.label,
            truncate(&ch.window.title, 50)
        )
        .unwrap();
        for f in &ch.changes {
            writeln!(o, "  {}: {} → {}", f.field, f.from, f.to).unwrap();
        }
    }
    o
}

pub fn workset_saved(
    name: &str,
    ws: &Workset,
    path: &Path,
    backup: Option<&Path>,
    skipped: &[String],
) -> String {
    let n = ws.windows.len();
    let mut o = format!(
        "Saved workset {name} ({n} window{})\n  {}\n",
        if n == 1 { "" } else { "s" },
        home_relative(path)
    );
    if let Some(b) = backup {
        writeln!(o, "  previous version: {}", home_relative(b)).unwrap();
    }
    if !skipped.is_empty() {
        writeln!(o, "\nNot saved (no way to relaunch):").unwrap();
        for s in skipped {
            writeln!(o, "  ✗ {s}").unwrap();
        }
    }
    o
}

pub fn worksets(list: &[(String, anyhow::Result<Workset>)], dir: &Path) -> String {
    if list.is_empty() {
        return format!(
            "No worksets in {}\n(create one: hyprstate workset create NAME, or save the desktop: hyprstate workset save NAME)\n",
            home_relative(dir)
        );
    }
    let w = list.iter().map(|(n, _)| n.len()).max().unwrap_or(4).max(4);
    let mut o = format!("{:<w$}  WINDOWS  DESCRIPTION\n", "NAME");
    for (name, ws) in list {
        match ws {
            Ok(ws) => writeln!(
                o,
                "{name:<w$}  {:<7}  {}",
                ws.windows.len(),
                ws.description.as_deref().unwrap_or("")
            )
            .unwrap(),
            Err(e) => writeln!(o, "{name:<w$}  ✗ invalid: {e:#}").unwrap(),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{Changed, FieldChange, WindowRef};
    use crate::discovery::resolve::{AppIdentity, Confidence, LaunchSpec, LaunchVia};
    use crate::hyprland::commands::Command;
    use crate::restore::executor::ItemReport;
    use crate::restore::matcher::Strategy;
    use crate::restore::planner::{Placement, PlanItem, Summary};
    use crate::snapshot::capture::build_snapshot_at;
    use crate::snapshot::capture::tests::{fixture_discovery, fixture_live};
    use crate::workset::{Entry, WorkspaceSpec};
    use jiff::Timestamp;
    use std::path::PathBuf;

    fn snapshot() -> Snapshot {
        let live = fixture_live();
        let wins = fixture_discovery().windows(&live.clients);
        build_snapshot_at("s".into(), Timestamp::UNIX_EPOCH, &live, &wins)
    }

    fn home() -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").unwrap())
    }

    #[test]
    fn workspaces_sort_numbered_named_special() {
        let mut v = [
            ws_order(-98, "special:magic"),
            ws_order(0, "web"),
            ws_order(2, "2"),
        ];
        v.sort();
        assert_eq!(v[0].1, 2);
        assert_eq!(v[1].2, "web");
        assert_eq!(v[2].2, "special:magic");
    }

    #[test]
    fn paths_under_home_are_shortened() {
        assert_eq!(home_relative(&home().join("x/y")), "~/x/y");
        assert_eq!(home_relative(Path::new("/opt/x")), "/opt/x");
    }

    #[test]
    fn saved_and_listed_snapshots() {
        let mut s = snapshot();
        s.excluded_count = 2;
        let out = saved(&s, Path::new("/tmp/s.json"));
        assert!(
            out.contains("4 windows on 3 workspaces (2 excluded by policy)"),
            "{out}"
        );

        let dir = home().join(".local/state/hyprstate/snapshots");
        assert!(list(&[], &dir).starts_with("No snapshots in ~/"));
        let listed = Listed {
            name: "before-reboot".into(),
            path: dir.join("before-reboot.json"),
            snapshot: s,
        };
        let out = list(&[listed], &dir);
        assert!(out.starts_with("NAME           CREATED"), "{out}");
        assert!(out.lines().nth(1).unwrap().ends_with("  4"), "{out}");
    }

    #[test]
    fn inspect_summary_and_window_details() {
        let mut s = snapshot();
        s.excluded_count = 1;
        let out = inspect(&s, false);
        assert!(out.contains("  1 → 2 windows\n"), "{out}");
        assert!(out.contains("  2 → 1 window\n"), "{out}");
        assert!(out.contains("(1 excluded by policy)"), "{out}");
        assert!(out.contains("  chromium ×2\n"), "{out}");
        assert!(out.contains("  foot\n"), "{out}");

        let w = &mut s.windows;
        w[0].floating = true;
        w[0].fullscreen = 1;
        w[0].pinned = true;
        w[0].xwayland = true;
        w[0].title = "x".repeat(80);
        w[1].app.launch = None;
        w[1].app.launch_problem = Some("redacted".into());
        w[3].app.launch = None;
        w[3].app.launch_problem = None;
        w[2].app.cwd = Some(home().join("proj"));
        let out = inspect(&s, true);
        assert!(out.contains("floating, "), "{out}");
        assert!(out.contains(", fullscreen 1, pinned, xwayland"), "{out}");
        assert!(out.contains(&format!("\"{}…\"", "x".repeat(59))), "{out}");
        assert!(out.contains("launch:     ✗ redacted"), "{out}");
        assert!(out.contains("launch:     ✗ unknown"), "{out}");
        assert!(out.contains("launch:     foot"), "{out}");
        assert!(out.contains("cwd:        ~/proj"), "{out}");
        assert!(
            out.contains("app:        webapp:https://web.whatsapp.com/"),
            "{out}"
        );
    }

    fn plan_item(s: &Snapshot, i: usize, action: Action) -> PlanItem {
        PlanItem {
            key: s.windows[i].key,
            label: label(&s.windows[i]),
            placement: Placement::of(&s.windows[i]),
            action,
        }
    }

    fn reuse(commands: Vec<Command>, ambiguous: bool, confidence: Confidence) -> Action {
        Action::Reuse {
            address: "0x1".into(),
            strategy: Strategy::Class,
            confidence,
            ambiguous,
            commands,
        }
    }

    #[test]
    fn plans_show_every_action() {
        let s = snapshot();
        let p = Plan {
            snapshot: "s".into(),
            items: vec![
                plan_item(&s, 0, reuse(vec![], false, Confidence::High)),
                plan_item(
                    &s,
                    1,
                    reuse(
                        vec![Command::SetPinned {
                            address: "0x1".into(),
                            pinned: true,
                        }],
                        true,
                        Confidence::Low,
                    ),
                ),
                plan_item(
                    &s,
                    2,
                    Action::Launch {
                        spec: LaunchSpec {
                            argv: vec!["foot".into()],
                            cwd: None,
                            via: LaunchVia::Desktop,
                        },
                    },
                ),
                plan_item(
                    &s,
                    3,
                    Action::Unresolved {
                        reason: "gone".into(),
                    },
                ),
                plan_item(
                    &s,
                    3,
                    Action::Excluded {
                        reason: "policy".into(),
                    },
                ),
            ],
            workspace_commands: vec![Command::MoveWorkspaceToMonitor {
                workspace: "1".into(),
                monitor: "DP-2".into(),
            }],
            summary: Summary {
                reuse: 2,
                launch: 1,
                unresolved: 1,
                excluded: 1,
                ambiguous: 1,
                to_move: 1,
                untouched: 3,
            },
        };
        let out = plan(&p, &s);
        for want in [
            "Snapshot: s (",
            "  ✓ web.whatsapp.com\n",
            "  ⚠ chromium  (pin)\n      reason: multiple candidate windows\n      match: class, confidence LOW\n",
            "  + foot  (launch: foot)\n",
            "  ✗ chromium\n      reason: gone\n",
            "  - chromium  (skipped: policy)\n",
            "\nWorkspaces\n  workspace 1 → monitor DP-2\n",
            "  reuse: 2 (1 to move)\n",
            "  ambiguous: 1\n  excluded: 1\n  other windows left alone: 3\n",
        ] {
            assert!(out.contains(want), "missing {want:?} in\n{out}");
        }
        assert!(!out.contains("already matches"));
    }

    #[test]
    fn noop_plans_say_so() {
        let s = snapshot();
        let mut p = Plan {
            snapshot: "s".into(),
            items: vec![],
            workspace_commands: vec![],
            summary: Summary::default(),
        };
        assert!(plan(&p, &s).ends_with("Desktop already matches the snapshot.\n"));
        p.snapshot = "workset:dev".into();
        let out = plan(&p, &s);
        assert!(out.starts_with("Workset: dev\n"), "{out}");
        assert!(
            out.ends_with("Desktop already matches the workset.\n"),
            "{out}"
        );
    }

    #[test]
    fn reports_show_every_outcome() {
        let outcomes = [
            Outcome::Unchanged,
            Outcome::Placed,
            Outcome::Launched,
            Outcome::SpawnFailed,
            Outcome::TimedOut,
            Outcome::Failed,
            Outcome::Unresolved,
            Outcome::Excluded,
        ];
        let items = outcomes
            .iter()
            .enumerate()
            .map(|(i, &outcome)| ItemReport {
                key: i as u32,
                label: format!("app{i}"),
                workspace: if i == 7 {
                    "special:x".into()
                } else {
                    "1".into()
                },
                outcome,
                address: None,
                detail: (outcome == Outcome::Failed).then(|| "boom".into()),
                warnings: if i == 1 { vec!["drift".into()] } else { vec![] },
            })
            .collect();
        let r = Report {
            snapshot: "s".into(),
            items,
            workspace_errors: vec!["no monitor".into()],
            elapsed_ms: 1500,
        };
        let out = report(&r);
        for want in [
            "  ✓ app0\n",
            "  ⚠ app1  (moved)\n      warning: drift\n",
            "  ✓ app2  (launched)\n",
            "  ✗ app3  (launch failed)\n",
            "  ✗ app4  (window did not appear)\n",
            "  ✗ app5  (could not place)\n      reason: boom\n",
            "  ✗ app6  (unresolved)\n",
            "Workspace special:x\n  - app7  (excluded)\n",
            "⚠ workspace: no monitor\n",
            "  launched: 1\n  reused: 2\n  failed: 4\n  time: 1.5s\n",
        ] {
            assert!(out.contains(want), "missing {want:?} in\n{out}");
        }
    }

    #[test]
    fn diffs() {
        let w = |label: &str, ws: &str| WindowRef {
            label: label.into(),
            title: "t".into(),
            workspace: ws.into(),
        };
        let mut d = Diff {
            from: "a".into(),
            to: "b".into(),
            ..Default::default()
        };
        assert_eq!(diff(&d), "No differences between a and b\n");
        d.added.push(w("discord", "2"));
        d.removed.push(w("slack", "special:chat"));
        d.changed.push(Changed {
            window: w("foot", "3"),
            changes: vec![FieldChange {
                field: "workspace",
                from: "1".into(),
                to: "3".into(),
            }],
        });
        let out = diff(&d);
        assert!(out.starts_with("a → b\n"), "{out}");
        assert!(out.contains("Workspace 2\n  + discord  \"t\"\n"), "{out}");
        assert!(
            out.contains("Workspace special:chat\n  - slack  \"t\"\n"),
            "{out}"
        );
        assert!(out.contains("foot  \"t\"\n  workspace: 1 → 3\n"), "{out}");
    }

    fn workset(n: usize) -> Workset {
        let e = Entry {
            workspace: WorkspaceSpec::Id(1),
            command: "foot".into(),
            cwd: None,
            class: None,
            title_contains: None,
            reuse: true,
            floating: false,
            position: None,
            size: None,
        };
        Workset {
            description: Some("dev".into()),
            windows: vec![e; n],
        }
    }

    #[test]
    fn saved_worksets() {
        let p = Path::new("/w/dev.toml");
        let out = workset_saved("dev", &workset(1), p, None, &[]);
        assert_eq!(out, "Saved workset dev (1 window)\n  /w/dev.toml\n");
        let out = workset_saved(
            "dev",
            &workset(2),
            p,
            Some(Path::new("/w/dev.toml.bak")),
            &["signal on workspace 4: redacted".into()],
        );
        assert!(out.starts_with("Saved workset dev (2 windows)\n"), "{out}");
        assert!(
            out.contains("  previous version: /w/dev.toml.bak\n"),
            "{out}"
        );
        assert!(
            out.contains("  ✗ signal on workspace 4: redacted\n"),
            "{out}"
        );
    }

    #[test]
    fn listed_worksets() {
        let dir = Path::new("/w");
        assert!(worksets(&[], dir).starts_with("No worksets in /w\n"));
        let list = vec![
            ("dev".to_string(), Ok(workset(2))),
            ("broken".to_string(), Err(anyhow::anyhow!("bad toml"))),
        ];
        let out = worksets(&list, dir);
        assert!(out.contains("dev     2        dev\n"), "{out}");
        assert!(out.contains("broken  ✗ invalid: bad toml\n"), "{out}");
    }

    #[test]
    fn unused_identity_kinds_still_label() {
        let mut s = snapshot();
        s.windows[0].app.identity = AppIdentity::Class { class: "k".into() };
        assert!(inspect(&s, false).contains("  k\n"));
    }
}
