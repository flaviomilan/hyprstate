# hyprstate

Capture, inspect, diff and restore your Hyprland desktop.

You describe how the desktop should look (a snapshot), and `hyprstate` makes
it so, deterministically: it reuses windows that are already open, launches
the ones that are missing, and puts each one back on its workspace. Nothing is
duplicated, and running a restore twice does nothing the second time.

```
hyprstate snapshot [--name NAME] [--force]     save the current desktop
hyprstate list                                 saved snapshots
hyprstate inspect [SNAPSHOT] [--windows]       live desktop (default) or a snapshot
hyprstate restore [SNAPSHOT] [--dry-run]       default: most recent snapshot
hyprstate diff FROM [TO]                       default TO: live desktop

hyprstate workset create NAME                  new workset from a commented template
hyprstate workset save NAME [-w WORKSPACE]...  live desktop (or some workspaces) → workset
hyprstate workset open NAME [--dry-run]        reuse, launch and place its windows
hyprstate workset list | delete NAME
```

Every command takes `--json`. Logs go to stderr (`-v`, `-vv`, `--log-format json`).
`restore` exits with 0 when everything was restored, 2 when some windows
failed (the others are still restored), and 1 on errors.

Always start with `--dry-run`:

```
$ hyprstate restore --dry-run
Snapshot: before-reboot (2026-09-26 23:42)

Workspace 1
  ✓ code
  + foot  (launch: foot)

Workspace 2
  ✓ chromium  (move → workspace 2)
  ✗ linear.app
      reason: executable not found: omarchy-launch-webapp

Actions:
  launch: 1
  reuse: 2 (1 to move)
  unresolved: 1
```

## Worksets

A snapshot records the desktop you *had*. A workset declares a desktop you
*want*, and you open it on purpose ("work on recsys"). Worksets are
hand-editable TOML in `~/.config/hyprstate/worksets/NAME.toml`:

```toml
description = "Recsys at work"

[[windows]]
workspace = 1
command = "idea ~/projects/recsys"
class = "jetbrains-idea"          # helps recognise an already open window

[[windows]]
workspace = 2
command = "kitty"
cwd = "~/projects/recsys"         # launch here; only reuse a kitty that is here

[[windows]]
workspace = 3
command = "omarchy-launch-webapp https://linear.app/"

[[windows]]
workspace = "special:music"
command = "flatpak run com.spotify.Client"
floating = true
position = [100, 100]
size = [900, 600]
```

`open` goes through the same engine as `restore`. An already open window of
the same application is reused and moved instead of launching a duplicate,
and opening the same workset twice does nothing the second time. Because a
workset should never take over an unrelated window, an entry can narrow
which open windows count: `cwd` (the window's directory, for terminals),
`title_contains`, or `reuse = false` to always launch a new window. Windows
that are not in the workset are left alone. Entries without `floating = true`
are tiled.

`workset save` writes the current desktop in this format as a starting point.
It keeps the previous file as `NAME.toml.bak`, since saving replaces comments.

## How it works

```
Discovery ──► Snapshot ──► Planner ──► Plan ──► Executor
(read only)    (JSON)       (pure)              (only part that changes the desktop)
```

- **Discovery** reads Hyprland over its IPC sockets and works out which
  application owns each window. It uses, in order: Flatpak scope, Chromium web
  app class (PWAs share the browser's PID, so the class is what identifies
  them), the launched desktop file, the systemd scope, `StartupWMClass`, the
  desktop id, and finally the executable and (sanitized) command line.
  `inspect --windows` shows which signal was used and how confident it is.
- **Matching** pairs snapshot windows with live ones by application identity
  and initial class, using titles and workspace as tie-breakers. It never uses
  PID, because PIDs don't survive a restart. When several indistinguishable
  windows want different places, the match is marked ambiguous.
- **Planning** only emits what differs from the live state, which is why
  restores are idempotent.
- **Execution** subscribes to Hyprland events before launching, starts
  missing apps with `exec_cmd` directly on their target workspace, claims new
  windows as they appear, places them, and then checks the result. A failure
  in one window never stops the rest.

Snapshots live in `$XDG_STATE_HOME/hyprstate/snapshots/<name>.json` (mode 0600).

## Configuration

`~/.config/hyprstate/config.toml`. Every key is optional, and unknown keys are
an error.

```toml
[policy]
exclude = ["signal"]        # class, initial class, desktop or flatpak id
default_excludes = true     # 1Password, KeePassXC, Bitwarden, …

[[exclude]]
class = "org.keepassxc.KeePassXC"

# How to relaunch a window, overriding what was discovered.
[[windows]]
match.class = "foo"         # also: match.initial_class, match.title_contains
command = "foo --some-flag"
cwd = "~/projects/foo"

[security]
sensitive_args = "redact"   # or "reject"

[restore]
timeout = 30                # seconds to wait for launched windows
launch_wrapper = []         # e.g. ["uwsm", "app", "--"]
```

## Privacy

Excluded windows are never written to disk; only their count is kept. Window
contents, the clipboard and environment variables are never stored. Command
lines are the only free-form process data that gets saved, and they are
scanned first: `--token x`, `--api-key=…`, `PASSWORD=…`, URLs with credentials
or token-like query parameters, and long high-entropy strings are redacted.
A redacted command is never relaunched; add a `[[windows]]` override for it.

## Compatibility

Linux, a systemd user session, and Hyprland 0.55+. Hyprland 0.56's Lua
dispatchers (`hl.dsp.*`) have been verified. The classic dispatcher syntax
used by hyprlang-configured Hyprland is detected automatically and supported
on a best-effort basis.

Omarchy web apps work through their `.desktop` files and
`omarchy-launch-webapp`, and nothing in the core depends on Omarchy.

## Known limits

- Tiled layout geometry (splits, ratios, master/dwindle state) is not
  restored. Windows go back to the right workspace and tiled/floating state;
  floating windows also get their exact position and size back.
- Window groups are recorded but not rebuilt.
- Single-instance apps that open only one window per launch (e.g. Discord)
  cannot bring back a second window. That window is reported as timed out.

## Not in scope

These are deliberate non-goals, listed to prevent scope creep:

- restoring browser tabs, documents, scroll positions or other internal app state
- KDE, Sway or other compositors
- a GUI, TUI or Waybar module (a TUI may come later, as a client of the engine)
- a daemon (planned for later: auto-save and auto-restore)
- the `session` commands from the full CLI plan (not built yet)
- SQLite, cloud sync, clipboard management, or monitor management
- AI of any kind: the matching should stand on its own

## Development

```
cargo test          # unit tests plus executor tests against a fake compositor
cargo clippy --all-targets
```

`tests/fixtures/` contains real `hyprctl -j` output, with titles redacted.
