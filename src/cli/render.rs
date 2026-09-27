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
