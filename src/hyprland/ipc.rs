//! Request socket (`.socket.sock`). One connection per request, as Hyprland
//! closes the socket after replying.

use std::cell::OnceCell;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;

use super::commands::{Command, Dialect};
use super::models::{Client, LiveState, Monitor, Version, Workspace, WorkspaceRef};

/// Everything that talks to the compositor goes through this trait so that
/// the executor can be tested against a fake.
pub trait Compositor {
    fn clients(&self) -> Result<Vec<Client>>;
    fn live_state(&self) -> Result<LiveState>;
    /// Runs commands in order. One result per command; a failed command does
    /// not stop the others. `Err` only when the compositor is unreachable.
    fn dispatch(&self, commands: &[Command]) -> Result<Vec<Result<(), String>>>;
}

pub fn instance_dir() -> Result<PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .context("HYPRLAND_INSTANCE_SIGNATURE is not set; is Hyprland running?")?;
    let runtime = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(runtime).join("hypr").join(sig))
}

pub struct HyprSocket {
    path: PathBuf,
    dialect: OnceCell<Dialect>,
}

impl HyprSocket {
    pub fn from_env() -> Result<Self> {
        let path = instance_dir()?.join(".socket.sock");
        if !path.exists() {
            bail!("Hyprland socket not found at {}", path.display());
        }
        Ok(Self {
            path,
            dialect: OnceCell::new(),
        })
    }

    pub fn request(&self, msg: &str) -> Result<String> {
        let mut stream = UnixStream::connect(&self.path)
            .with_context(|| format!("connecting to {}", self.path.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.write_all(msg.as_bytes())?;
        let mut out = String::new();
        stream.read_to_string(&mut out)?;
        Ok(out)
    }

    /// Lua-config Hyprland answers `ok` to a Lua no-op; hyprlang rejects it.
    pub fn dialect(&self) -> Dialect {
        *self
            .dialect
            .get_or_init(|| match self.request("dispatch hl.dsp.no_op()") {
                Ok(r) if r.trim() == "ok" => Dialect::Lua,
                _ => Dialect::Legacy,
            })
    }

    fn json<T: DeserializeOwned>(&self, query: &str) -> Result<T> {
        let raw = self.request(&format!("j/{query}"))?;
        serde_json::from_str(&raw).with_context(|| format!("parsing reply to j/{query}"))
    }
}

impl Compositor for HyprSocket {
    fn clients(&self) -> Result<Vec<Client>> {
        self.json("clients")
    }

    fn live_state(&self) -> Result<LiveState> {
        let version: Version = self.json("version")?;
        let monitors: Vec<Monitor> = self.json("monitors")?;
        let workspaces: Vec<Workspace> = self.json("workspaces")?;
        let active_workspace: WorkspaceRef = self.json("activeworkspace")?;
        let clients: Vec<Client> = self.json("clients")?;
        Ok(LiveState {
            version,
            monitors,
            workspaces,
            active_workspace,
            clients,
        })
    }

    fn dispatch(&self, commands: &[Command]) -> Result<Vec<Result<(), String>>> {
        let dialect = self.dialect();
        // One request per raw dispatch: `[[BATCH]]` splits on `;`, which may
        // legitimately appear inside an exec'd command line.
        commands
            .iter()
            .map(|c| {
                let mut errs = Vec::new();
                for raw in c.render(dialect) {
                    let reply = self.request(&format!("dispatch {raw}"))?;
                    let reply = reply.trim();
                    if reply != "ok" {
                        errs.push(reply.to_string());
                    }
                }
                Ok(if errs.is_empty() {
                    Ok(())
                } else {
                    Err(errs.join("; "))
                })
            })
            .collect()
    }
}
