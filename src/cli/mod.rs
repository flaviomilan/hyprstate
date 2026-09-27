//! Command-line interface: argument parsing and command wiring.

mod render;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use crate::config::Config;
use crate::diff::diff;
use crate::discovery::which;
use crate::hyprland::events::HyprEvents;
use crate::hyprland::ipc::{Compositor, HyprSocket};
use crate::restore::executor::{ExecOptions, Executor};
use crate::restore::planner::{Plan, PlanInput, plan};
use crate::snapshot::capture::{Discovery, ResolvedWindow, build_snapshot};
use crate::snapshot::model::Snapshot;
use crate::snapshot::storage::{Store, default_name};
use crate::workset::store::WorksetStore;
use crate::workset::{Workset, WorkspaceSpec};

#[derive(Parser)]
#[command(
    name = "hyprstate",
    version,
    about = "Capture, inspect, diff and restore Hyprland desktops"
)]
pub struct Cli {
    /// Config file (default: ~/.config/hyprstate/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Log format on stderr
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Text)]
    log_format: LogFormat,
    /// More logs (-v info, -vv debug)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
enum Cmd {
    /// Save the current desktop as a snapshot
    Snapshot {
        /// Snapshot name (default: local timestamp)
        #[arg(long)]
        name: Option<String>,
        /// Overwrite an existing snapshot with the same name
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// List saved snapshots
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show the live desktop, or a saved snapshot
    Inspect {
        /// Snapshot name or path (default: live desktop)
        snapshot: Option<String>,
        /// Per-window application resolution and confidence
        #[arg(long)]
        windows: bool,
        #[arg(long)]
        json: bool,
    },
    /// Restore a snapshot (default: the most recent)
    Restore {
        snapshot: Option<String>,
        /// Show the plan without changing anything
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
        /// Seconds to wait for launched windows (default: config, 30)
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Named desired desktops you open on purpose
    #[command(subcommand)]
    Workset(WorksetCmd),
    /// Compare two snapshots, or a snapshot with the live desktop
    Diff {
        from: String,
        /// Snapshot to compare against (default: live desktop)
        to: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum WorksetCmd {
    /// Create an empty workset from a commented template
    Create { name: String },
    /// Save the live desktop (or some workspaces) as a workset
    Save {
        name: String,
        /// Only these workspaces (repeatable): 3, "special:chat", "web"
        #[arg(short, long = "workspace", value_name = "WORKSPACE")]
        workspaces: Vec<String>,
    },
    /// Open a workset: reuse, launch and place its windows
    Open {
        name: String,
        /// Show the plan without changing anything
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
        /// Seconds to wait for launched windows (default: config, 30)
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// List worksets
    List {
        #[arg(long)]
        json: bool,
    },
    /// Delete a workset file
    Delete { name: String },
}

/// Exit codes: 0 ok, 1 error, 2 restore finished with failed windows.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.log_format, cli.verbose);
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn init_logging(format: LogFormat, verbose: u8) {
    let level = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("HYPRSTATE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(format!("hyprstate={level}")));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    match format {
        LogFormat::Text => builder.without_time().with_target(false).init(),
        LogFormat::Json => builder.json().flatten_event(true).init(),
    }
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// Live desktop, resolved.
struct Live {
    socket: HyprSocket,
    snapshot: Snapshot,
    windows: Vec<ResolvedWindow>,
    state: crate::hyprland::models::LiveState,
}

fn capture_live(discovery: &Discovery, name: String) -> Result<Live> {
    let socket = HyprSocket::from_env()?;
    let state = socket.live_state()?;
    let windows = discovery.windows(&state.clients);
    let snapshot = build_snapshot(name, &state, &windows);
    Ok(Live {
        socket,
        snapshot,
        windows,
        state,
    })
}

fn run(cli: Cli) -> Result<ExitCode> {
    let cfg = Config::load(cli.config.as_deref())?;
    let store = Store::open_default()?;
    match cli.command {
        Cmd::Snapshot { name, force, json } => {
            let discovery = Discovery::from_system(&cfg);
            let live = capture_live(&discovery, name.unwrap_or_else(default_name))?;
            let path = store.save(&live.snapshot, force)?;
            if json {
                print_json(
                    &serde_json::json!({ "name": live.snapshot.name, "path": path, "windows": live.snapshot.windows.len(), "excluded": live.snapshot.excluded_count }),
                )?;
            } else {
                print!("{}", render::saved(&live.snapshot, &path));
            }
        }
        Cmd::List { json } => {
            let (list, bad) = store.list()?;
            for (p, e) in &bad {
                tracing::warn!(path = %p.display(), error = %e, "unreadable snapshot");
            }
            if json {
                let rows: Vec<_> = list
                    .iter()
                    .map(|l| serde_json::json!({ "name": l.name, "created_at": l.snapshot.created_at, "windows": l.snapshot.windows.len(), "path": l.path }))
                    .collect();
                print_json(&rows)?;
            } else {
                print!("{}", render::list(&list, store.dir()));
            }
        }
        Cmd::Inspect {
            snapshot,
            windows,
            json,
        } => {
            let snap = match snapshot {
                Some(s) => store.load(&s)?,
                None => capture_live(&Discovery::from_system(&cfg), "live".into())?.snapshot,
            };
            if json {
                print_json(&snap)?;
            } else {
                print!("{}", render::inspect(&snap, windows));
            }
        }
        Cmd::Diff { from, to, json } => {
            let a = store.load(&from)?;
            let b = match to {
                Some(t) => store.load(&t)?,
                None => capture_live(&Discovery::from_system(&cfg), "live".into())?.snapshot,
            };
            let d = diff(&a, &b);
            if json {
                print_json(&d)?;
            } else {
                print!("{}", render::diff(&d));
            }
        }
        Cmd::Restore {
            snapshot,
            dry_run,
            json,
            timeout,
        } => {
            let snap = match snapshot {
                Some(s) => store.load(&s)?,
                None => store.latest()?,
            };
            let discovery = Discovery::from_system(&cfg);
            return apply(&cfg, &discovery, &snap, dry_run, json, timeout);
        }
        Cmd::Workset(cmd) => return workset(&cfg, cmd),
    }
    Ok(ExitCode::SUCCESS)
}

/// Plans `snap` against the live desktop, then prints the plan (`dry_run`) or
/// carries it out. Shared by `restore` and `workset open`.
fn apply(
    cfg: &Config,
    discovery: &Discovery,
    snap: &Snapshot,
    dry_run: bool,
    json: bool,
    timeout: Option<u64>,
) -> Result<ExitCode> {
    let live = capture_live(discovery, "live".into())?;
    let exclusions = cfg.exclusions();
    let runnable = |p: &str| which(p).is_some();
    let the_plan: Plan = plan(&PlanInput {
        snapshot: snap,
        live: &live.state,
        windows: &live.windows,
        overrides: &cfg.windows,
        exclusions: &exclusions,
        launch_wrapper: &cfg.restore.launch_wrapper,
        runnable: &runnable,
    });
    if dry_run {
        if json {
            print_json(&the_plan)?;
        } else {
            print!("{}", render::plan(&the_plan, snap));
        }
        return Ok(ExitCode::SUCCESS);
    }
    // Subscribe before launching anything so no window is missed.
    let mut events = HyprEvents::connect().context("connecting to Hyprland events")?;
    let executor = Executor {
        compositor: &live.socket,
        discovery,
        options: ExecOptions {
            timeout: Duration::from_secs(timeout.unwrap_or(cfg.restore.timeout)),
            ..Default::default()
        },
    };
    let report = executor.run(&the_plan, snap, &mut events);
    if json {
        print_json(&report)?;
    } else {
        print!("{}", render::report(&report));
    }
    Ok(if report.failures() > 0 {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

fn workset(cfg: &Config, cmd: WorksetCmd) -> Result<ExitCode> {
    let store = WorksetStore::open_default()?;
    match cmd {
        WorksetCmd::Create { name } => {
            let path = store.create(&name)?;
            println!("Created workset {name}\n  {}", path.display());
            println!("Edit it, then: hyprstate workset open {name} --dry-run");
        }
        WorksetCmd::Save { name, workspaces } => {
            store.path(&name)?;
            let only: Vec<WorkspaceSpec> = workspaces
                .iter()
                .map(|w| WorkspaceSpec::Name(w.clone()))
                .collect();
            let discovery = Discovery::from_system(cfg);
            let live = capture_live(&discovery, "live".into())?;
            let (mut ws, skipped) = Workset::from_live(&live.windows, &only);
            if ws.windows.is_empty() {
                anyhow::bail!("nothing to save: no windows on the selected workspaces");
            }
            // Keep a hand-written description across saves.
            if let Ok(old) = store.load(&name) {
                ws.description = old.description;
            }
            let (path, backup) = store.save(&name, &ws)?;
            print!(
                "{}",
                render::workset_saved(&name, &ws, &path, backup.as_deref(), &skipped)
            );
        }
        WorksetCmd::Open {
            name,
            dry_run,
            json,
            timeout,
        } => {
            let ws = store.load(&name)?;
            if ws.windows.is_empty() {
                anyhow::bail!(
                    "workset '{name}' has no windows; edit {}",
                    store.path(&name)?.display()
                );
            }
            let discovery = Discovery::from_system(cfg);
            let snap = ws.to_snapshot(&name, &discovery.index);
            return apply(cfg, &discovery, &snap, dry_run, json, timeout);
        }
        WorksetCmd::List { json } => {
            let list = store.list()?;
            if json {
                let rows: Vec<_> = list
                    .iter()
                    .map(|(n, ws)| match ws {
                        Ok(ws) => serde_json::json!({ "name": n, "description": ws.description, "windows": ws.windows.len() }),
                        Err(e) => serde_json::json!({ "name": n, "error": format!("{e:#}") }),
                    })
                    .collect();
                print_json(&rows)?;
            } else {
                print!("{}", render::worksets(&list, store.dir()));
            }
        }
        WorksetCmd::Delete { name } => {
            let path = store.delete(&name)?;
            println!("Deleted workset {name} ({})", path.display());
        }
    }
    Ok(ExitCode::SUCCESS)
}
