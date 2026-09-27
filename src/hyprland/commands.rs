//! Typed dispatcher commands, rendered per Hyprland config dialect.
//!
//! Hyprland with a Lua config (0.56+) evaluates `dispatch <lua>`; with a
//! hyprlang config it takes the classic `dispatch name args` syntax.

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    /// `hl.dsp.*` (verified against Hyprland 0.56.2).
    Lua,
    /// Classic dispatchers (best effort; pre-Lua Hyprland).
    Legacy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    MoveToWorkspace {
        address: String,
        workspace: String,
    },
    SetFloating {
        address: String,
        floating: bool,
    },
    MoveExact {
        address: String,
        x: i32,
        y: i32,
    },
    ResizeExact {
        address: String,
        w: i32,
        h: i32,
    },
    SetPinned {
        address: String,
        pinned: bool,
    },
    Fullscreen {
        address: String,
        internal: u8,
        client: u8,
    },
    MoveWorkspaceToMonitor {
        workspace: String,
        monitor: String,
    },
    Exec {
        argv: Vec<String>,
        cwd: Option<PathBuf>,
        workspace: Option<String>,
    },
}

impl fmt::Display for Command {
    /// Human-readable form for plans and logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MoveToWorkspace { workspace, .. } => write!(f, "move → workspace {workspace}"),
            Self::SetFloating { floating: true, .. } => write!(f, "float"),
            Self::SetFloating {
                floating: false, ..
            } => write!(f, "tile"),
            Self::MoveExact { x, y, .. } => write!(f, "position {x},{y}"),
            Self::ResizeExact { w, h, .. } => write!(f, "size {w}x{h}"),
            Self::SetPinned { pinned, .. } => {
                write!(f, "{}", if *pinned { "pin" } else { "unpin" })
            }
            Self::Fullscreen { internal, .. } => write!(f, "fullscreen state {internal}"),
            Self::MoveWorkspaceToMonitor { workspace, monitor } => {
                write!(f, "workspace {workspace} → monitor {monitor}")
            }
            Self::Exec { argv, .. } => write!(f, "exec {}", shell_words::join(argv)),
        }
    }
}

/// Lua string literal.
fn lua(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Shell command line for `exec`, with optional `cd`.
fn shell_line(argv: &[String], cwd: &Option<PathBuf>) -> String {
    let cmd = shell_words::join(argv);
    match cwd {
        Some(d) => format!(
            "cd {} && exec {cmd}",
            shell_words::quote(&d.to_string_lossy())
        ),
        None => cmd,
    }
}

impl Command {
    /// Legacy dispatchers without a window argument need focus first.
    pub fn render(&self, dialect: Dialect) -> Vec<String> {
        match dialect {
            Dialect::Lua => vec![self.render_lua()],
            Dialect::Legacy => self.render_legacy(),
        }
    }

    fn render_lua(&self) -> String {
        let win = |a: &str| format!("window = {}", lua(&format!("address:{a}")));
        match self {
            Self::MoveToWorkspace { address, workspace } => format!(
                "hl.dsp.window.move({{ workspace = {}, follow = false, {} }})",
                lua(workspace),
                win(address)
            ),
            Self::SetFloating { address, floating } => format!(
                "hl.dsp.window.float({{ action = \"{}\", {} }})",
                if *floating { "enable" } else { "disable" },
                win(address)
            ),
            Self::MoveExact { address, x, y } => {
                format!(
                    "hl.dsp.window.move({{ x = {x}, y = {y}, {} }})",
                    win(address)
                )
            }
            Self::ResizeExact { address, w, h } => {
                format!(
                    "hl.dsp.window.resize({{ x = {w}, y = {h}, {} }})",
                    win(address)
                )
            }
            Self::SetPinned { address, pinned } => format!(
                "hl.dsp.window.pin({{ action = \"{}\", {} }})",
                if *pinned { "enable" } else { "disable" },
                win(address)
            ),
            Self::Fullscreen {
                address,
                internal,
                client,
            } => format!(
                "hl.dsp.window.fullscreen_state({{ internal = {internal}, client = {client}, {} }})",
                win(address)
            ),
            Self::MoveWorkspaceToMonitor { workspace, monitor } => format!(
                "hl.dsp.workspace.move({{ workspace = {}, monitor = {} }})",
                lua(workspace),
                lua(monitor)
            ),
            Self::Exec {
                argv,
                cwd,
                workspace,
            } => {
                let line = lua(&shell_line(argv, cwd));
                match workspace {
                    Some(ws) => format!(
                        "hl.dsp.exec_cmd({line}, {{ workspace = {} }})",
                        lua(&format!("{ws} silent"))
                    ),
                    None => format!("hl.dsp.exec_cmd({line})"),
                }
            }
        }
    }

    fn render_legacy(&self) -> Vec<String> {
        match self {
            Self::MoveToWorkspace { address, workspace } => {
                vec![format!(
                    "movetoworkspacesilent {workspace},address:{address}"
                )]
            }
            Self::SetFloating { address, floating } => vec![format!(
                "{} address:{address}",
                if *floating { "setfloating" } else { "settiled" }
            )],
            Self::MoveExact { address, x, y } => {
                vec![format!("movewindowpixel exact {x} {y},address:{address}")]
            }
            Self::ResizeExact { address, w, h } => {
                vec![format!("resizewindowpixel exact {w} {h},address:{address}")]
            }
            // `pin` toggles; the planner only emits this when the state differs.
            Self::SetPinned { address, .. } => vec![format!("pin address:{address}")],
            Self::Fullscreen {
                address,
                internal,
                client,
            } => vec![
                format!("focuswindow address:{address}"),
                format!("fullscreenstate {internal} {client}"),
            ],
            Self::MoveWorkspaceToMonitor { workspace, monitor } => {
                vec![format!("moveworkspacetomonitor {workspace} {monitor}")]
            }
            Self::Exec {
                argv,
                cwd,
                workspace,
            } => {
                let line = shell_line(argv, cwd);
                vec![match workspace {
                    Some(ws) => format!("exec [workspace {ws} silent] {line}"),
                    None => format!("exec {line}"),
                }]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0x58e4d02c5480";

    #[test]
    fn lua_matches_verified_syntax() {
        let c = Command::MoveToWorkspace {
            address: A.into(),
            workspace: "special:magic".into(),
        };
        assert_eq!(
            c.render(Dialect::Lua),
            vec![
                r#"hl.dsp.window.move({ workspace = "special:magic", follow = false, window = "address:0x58e4d02c5480" })"#
            ]
        );
        let c = Command::Fullscreen {
            address: A.into(),
            internal: 1,
            client: 0,
        };
        assert_eq!(
            c.render(Dialect::Lua)[0],
            r#"hl.dsp.window.fullscreen_state({ internal = 1, client = 0, window = "address:0x58e4d02c5480" })"#
        );
    }

    #[test]
    fn exec_is_quoted_for_shell_and_lua() {
        let c = Command::Exec {
            argv: vec![
                "omarchy-launch-webapp".into(),
                "https://x.com/a b\"c".into(),
            ],
            cwd: Some("/home/me/my proj".into()),
            workspace: Some("2".into()),
        };
        assert_eq!(
            c.render(Dialect::Lua)[0],
            r#"hl.dsp.exec_cmd("cd '/home/me/my proj' && exec omarchy-launch-webapp 'https://x.com/a b\"c'", { workspace = "2 silent" })"#
        );
        assert_eq!(
            c.render(Dialect::Legacy)[0],
            r#"exec [workspace 2 silent] cd '/home/me/my proj' && exec omarchy-launch-webapp 'https://x.com/a b"c'"#
        );
    }

    fn all() -> Vec<Command> {
        vec![
            Command::MoveToWorkspace {
                address: A.into(),
                workspace: "2".into(),
            },
            Command::SetFloating {
                address: A.into(),
                floating: true,
            },
            Command::SetFloating {
                address: A.into(),
                floating: false,
            },
            Command::MoveExact {
                address: A.into(),
                x: 10,
                y: 20,
            },
            Command::ResizeExact {
                address: A.into(),
                w: 300,
                h: 200,
            },
            Command::SetPinned {
                address: A.into(),
                pinned: true,
            },
            Command::SetPinned {
                address: A.into(),
                pinned: false,
            },
            Command::Fullscreen {
                address: A.into(),
                internal: 2,
                client: 2,
            },
            Command::MoveWorkspaceToMonitor {
                workspace: "name:web".into(),
                monitor: "DP-1".into(),
            },
            Command::Exec {
                argv: vec!["echo".into(), "a\\b\n\r\0".into()],
                cwd: None,
                workspace: None,
            },
        ]
    }

    #[test]
    fn every_command_renders_in_both_dialects() {
        let human: Vec<String> = all().iter().map(ToString::to_string).collect();
        assert_eq!(
            human,
            [
                "move → workspace 2",
                "float",
                "tile",
                "position 10,20",
                "size 300x200",
                "pin",
                "unpin",
                "fullscreen state 2",
                "workspace name:web → monitor DP-1",
                "exec echo 'a\\b\n\r\0'",
            ]
        );
        let lua: Vec<String> = all()
            .iter()
            .map(|c| c.render(Dialect::Lua)[0].clone())
            .collect();
        assert_eq!(
            lua,
            [
                r#"hl.dsp.window.move({ workspace = "2", follow = false, window = "address:0x58e4d02c5480" })"#,
                r#"hl.dsp.window.float({ action = "enable", window = "address:0x58e4d02c5480" })"#,
                r#"hl.dsp.window.float({ action = "disable", window = "address:0x58e4d02c5480" })"#,
                r#"hl.dsp.window.move({ x = 10, y = 20, window = "address:0x58e4d02c5480" })"#,
                r#"hl.dsp.window.resize({ x = 300, y = 200, window = "address:0x58e4d02c5480" })"#,
                r#"hl.dsp.window.pin({ action = "enable", window = "address:0x58e4d02c5480" })"#,
                r#"hl.dsp.window.pin({ action = "disable", window = "address:0x58e4d02c5480" })"#,
                r#"hl.dsp.window.fullscreen_state({ internal = 2, client = 2, window = "address:0x58e4d02c5480" })"#,
                r#"hl.dsp.workspace.move({ workspace = "name:web", monitor = "DP-1" })"#,
                r#"hl.dsp.exec_cmd("echo 'a\\b\n\r\0'")"#,
            ]
        );
        let legacy: Vec<Vec<String>> = all().iter().map(|c| c.render(Dialect::Legacy)).collect();
        assert_eq!(
            legacy.concat(),
            [
                "movetoworkspacesilent 2,address:0x58e4d02c5480",
                "setfloating address:0x58e4d02c5480",
                "settiled address:0x58e4d02c5480",
                "movewindowpixel exact 10 20,address:0x58e4d02c5480",
                "resizewindowpixel exact 300 200,address:0x58e4d02c5480",
                "pin address:0x58e4d02c5480",
                "pin address:0x58e4d02c5480",
                "focuswindow address:0x58e4d02c5480",
                "fullscreenstate 2 2",
                "moveworkspacetomonitor name:web DP-1",
                "exec echo 'a\\b\n\r\0'",
            ]
        );
    }
}
