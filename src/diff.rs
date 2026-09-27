//! Differences between two desktop states, paired with the restore matcher so
//! that `diff` and `restore` agree on which window is which.

use serde::Serialize;

use crate::restore::matcher;
use crate::restore::planner::label;
use crate::snapshot::model::{Snapshot, WindowRecord};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowRef {
    pub label: String,
    pub title: String,
    pub workspace: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldChange {
    pub field: &'static str,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Changed {
    pub window: WindowRef,
    pub changes: Vec<FieldChange>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Diff {
    pub from: String,
    pub to: String,
    /// Present in `to` only.
    pub added: Vec<WindowRef>,
    /// Present in `from` only.
    pub removed: Vec<WindowRef>,
    pub changed: Vec<Changed>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

fn wref(r: &WindowRecord) -> WindowRef {
    WindowRef {
        label: label(r),
        title: r.title.clone(),
        workspace: r.workspace.name.clone(),
    }
}

pub fn diff(a: &Snapshot, b: &Snapshot) -> Diff {
    let assignment = matcher::assign(&a.windows, &b.windows);
    let mut changed = Vec::new();
    for m in &assignment.matches {
        let (x, y) = (&a.windows[m.expected], &b.windows[m.candidate]);
        let mut changes = Vec::new();
        let mut field = |name, from: String, to: String| {
            if from != to {
                changes.push(FieldChange {
                    field: name,
                    from,
                    to,
                });
            }
        };
        field(
            "workspace",
            x.workspace.name.clone(),
            y.workspace.name.clone(),
        );
        field(
            "monitor",
            x.monitor.clone().unwrap_or_default(),
            y.monitor.clone().unwrap_or_default(),
        );
        field("floating", x.floating.to_string(), y.floating.to_string());
        field(
            "fullscreen",
            x.fullscreen.to_string(),
            y.fullscreen.to_string(),
        );
        if x.floating && y.floating {
            field(
                "position",
                format!("{},{}", x.at[0], x.at[1]),
                format!("{},{}", y.at[0], y.at[1]),
            );
            field(
                "size",
                format!("{}x{}", x.size[0], x.size[1]),
                format!("{}x{}", y.size[0], y.size[1]),
            );
        }
        if !changes.is_empty() {
            changed.push(Changed {
                window: wref(y),
                changes,
            });
        }
    }
    Diff {
        from: a.name.clone(),
        to: b.name.clone(),
        added: assignment
            .unmatched_candidates
            .iter()
            .map(|&i| wref(&b.windows[i]))
            .collect(),
        removed: assignment
            .unmatched_expected
            .iter()
            .map(|&i| wref(&a.windows[i]))
            .collect(),
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyprland::models::WorkspaceRef;
    use crate::snapshot::capture::build_snapshot_at;
    use crate::snapshot::capture::tests::{fixture_discovery, fixture_live};
    use jiff::Timestamp;

    #[test]
    fn detects_moves_additions_and_removals() {
        let live = fixture_live();
        let wins = fixture_discovery().windows(&live.clients);
        let a = build_snapshot_at("a".into(), Timestamp::UNIX_EPOCH, &live, &wins);
        assert!(diff(&a, &a).is_empty());

        let mut b = a.clone();
        b.name = "b".into();
        b.windows[2].workspace = WorkspaceRef {
            id: 1,
            name: "1".into(),
        }; // foot 3 → 1
        let whatsapp = b.windows.remove(0); // removed
        let mut extra = whatsapp.clone();
        extra.app.identity = crate::discovery::resolve::AppIdentity::Desktop {
            id: "discord".into(),
        };
        extra.class = "discord".into();
        extra.initial_class = "discord".into();
        b.windows.push(extra); // added

        let d = diff(&a, &b);
        assert_eq!(d.removed.len(), 1);
        assert_eq!(d.removed[0].label, "web.whatsapp.com");
        assert_eq!(d.added[0].label, "discord");
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].window.label, "foot");
        assert_eq!(
            d.changed[0].changes,
            vec![FieldChange {
                field: "workspace",
                from: "3".into(),
                to: "1".into()
            }]
        );
    }
}
