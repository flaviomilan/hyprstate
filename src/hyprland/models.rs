//! Raw shapes returned by Hyprland's `j/` IPC queries.
//!
//! Only the fields hyprstate reasons about are typed; everything else is kept
//! in `extra` so newer Hyprland fields survive a round trip untouched.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Client {
    pub address: String,
    #[serde(default = "yes")]
    pub mapped: bool,
    #[serde(default)]
    pub hidden: bool,
    pub at: [i32; 2],
    pub size: [i32; 2],
    pub workspace: WorkspaceRef,
    pub floating: bool,
    /// Monitor id (not name).
    pub monitor: i64,
    pub class: String,
    pub title: String,
    pub initial_class: String,
    pub initial_title: String,
    pub pid: i32,
    pub xwayland: bool,
    #[serde(default)]
    pub pinned: bool,
    /// 0 none, 1 maximized, 2 fullscreen (Hyprland `fullscreenstate` internal).
    #[serde(default)]
    pub fullscreen: u8,
    #[serde(default)]
    pub fullscreen_client: u8,
    #[serde(default)]
    pub grouped: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub stable_id: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl WorkspaceRef {
    /// Numbered workspaces are identified by id; named and special ones by
    /// name, since their (negative) ids are reassigned when recreated.
    pub fn same_as(&self, other: &WorkspaceRef) -> bool {
        if self.id > 0 || other.id > 0 {
            self.id == other.id
        } else {
            self.name == other.name
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: i64,
    pub name: String,
    pub monitor: String,
    #[serde(default, rename = "monitorID")]
    pub monitor_id: Option<i64>,
    pub windows: u32,
    #[serde(default)]
    pub hasfullscreen: bool,
    #[serde(default)]
    pub ispersistent: bool,
    #[serde(default)]
    pub tiled_layout: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Monitor {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub width: i32,
    pub height: i32,
    #[serde(default)]
    pub refresh_rate: f64,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    #[serde(default)]
    pub transform: i32,
    pub active_workspace: WorkspaceRef,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub version: String,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub tag: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Everything hyprstate needs to know about the compositor at one instant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveState {
    pub version: Version,
    pub monitors: Vec<Monitor>,
    pub workspaces: Vec<Workspace>,
    pub active_workspace: WorkspaceRef,
    pub clients: Vec<Client>,
}

impl LiveState {
    pub fn monitor_name(&self, id: i64) -> Option<&str> {
        self.monitors
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn parses_real_fixtures() {
        let clients: Vec<Client> = serde_json::from_str(&fixture("clients")).unwrap();
        assert_eq!(clients.len(), 4);
        let pwa = &clients[0];
        assert_eq!(pwa.initial_class, "chrome-web.whatsapp.com__-Default");
        assert_eq!(pwa.workspace.id, 2);
        assert!(pwa.extra.contains_key("focusHistoryID"));

        let ws: Vec<Workspace> = serde_json::from_str(&fixture("workspaces")).unwrap();
        assert!(ws.iter().any(|w| w.monitor == "DP-1"));
        let mons: Vec<Monitor> = serde_json::from_str(&fixture("monitors")).unwrap();
        assert_eq!(mons[0].name, "DP-1");
        let v: Version = serde_json::from_str(&fixture("version")).unwrap();
        assert_eq!(v.version, "0.56.2");
    }
}
