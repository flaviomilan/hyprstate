//! On-disk snapshot format (JSON, `schema_version` 1).

use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::discovery::resolve::AppResolution;
use crate::hyprland::models::WorkspaceRef;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub name: String,
    pub created_at: Timestamp,
    pub hyprland: HyprlandInfo,
    pub monitors: Vec<MonitorRecord>,
    pub workspaces: Vec<WorkspaceRecord>,
    pub active_workspace: WorkspaceRef,
    pub windows: Vec<WindowRecord>,
    /// Windows skipped by policy. Only the count is kept.
    pub excluded_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyprlandInfo {
    pub version: String,
    pub commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorRecord {
    pub name: String,
    pub description: String,
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub active_workspace: WorkspaceRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub id: i64,
    pub name: String,
    pub monitor: String,
    pub windows: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiled_layout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowRecord {
    /// Stable index inside this snapshot.
    pub key: u32,
    pub class: String,
    pub initial_class: String,
    pub title: String,
    pub initial_title: String,
    pub workspace: WorkspaceRef,
    /// Monitor name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    pub at: [i32; 2],
    pub size: [i32; 2],
    pub floating: bool,
    pub fullscreen: u8,
    pub pinned: bool,
    pub xwayland: bool,
    /// Windows sharing a group index were tabbed together (recorded only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u32>,
    /// Informational: meaningless after the compositor restarts.
    pub pid: i32,
    pub address: String,
    pub app: AppResolution,
    /// Extra conditions an existing window must meet to be reused for this
    /// one. Set by worksets; snapshots never need it.
    #[serde(default, skip_serializing_if = "Require::is_empty")]
    pub require: Require,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Require {
    /// The window's working directory (a terminal's shell) must be this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// The window's title must contain this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_contains: Option<String>,
    /// Never reuse an already open window: always launch a new one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub always_launch: bool,
}

impl Require {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Dispatcher argument addressing a workspace (`3`, `name:web`, `special:magic`).
pub fn workspace_target(ws: &WorkspaceRef) -> String {
    if ws.id > 0 {
        ws.id.to_string()
    } else if ws.name.starts_with("special") {
        ws.name.clone()
    } else {
        format!("name:{}", ws.name)
    }
}

impl Snapshot {
    pub fn file_name(&self) -> PathBuf {
        PathBuf::from(format!("{}.json", self.name))
    }
}
