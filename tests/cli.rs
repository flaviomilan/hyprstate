//! End-to-end: the real binary against a fake Hyprland listening on the same
//! Unix sockets (`.socket.sock` for requests, `.socket2.sock` for events).

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

const SIG: &str = "testsig";
/// No process has this pid, so discovery falls back to the window class.
const NO_PID: i64 = 999_999_999;

#[derive(Default)]
struct State {
    version: Value,
    monitors: Value,
    workspaces: Value,
    active: Value,
    clients: Vec<Value>,
    /// Answer `ok` to the Lua no-op (Lua config) or reject it (hyprlang).
    lua: bool,
    dispatches: Vec<String>,
    subscribers: Vec<UnixStream>,
    /// Exec'd command line containing the key → window that opens.
    spawns: Vec<(String, Value)>,
    /// Dispatches containing any of these are answered with an error.
    reject: Vec<String>,
    /// Queries answered with garbage.
    garbage: Vec<String>,
    /// Bytes sent to every event subscriber when it connects.
    prelude: Vec<u8>,
    /// Launched windows appear this late (so the executor has to wait).
    spawn_delay: Duration,
    /// Launched windows open on workspace 1 whatever was asked.
    ignore_exec_workspace: bool,
    next: usize,
}

struct Hypr {
    root: PathBuf,
    state: Arc<Mutex<State>>,
}

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn fixture(name: &str) -> Value {
    let p = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn client(address: &str, class: &str, title: &str, ws: i64, pid: i64) -> Value {
    let mut c = fixture("clients")[2].clone();
    c["address"] = json!(address);
    c["class"] = json!(class);
    c["initialClass"] = json!(class);
    c["title"] = json!(title);
    c["initialTitle"] = json!(title);
    c["workspace"] = json!({ "id": ws, "name": ws.to_string() });
    c["pid"] = json!(pid);
    c
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let rest = &s[s.find(start)? + start.len()..];
    Some(&rest[..rest.find(end)?])
}

impl Hypr {
    fn new() -> Self {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("hs-cli-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home/.local/share/applications", "bin"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let desktop = |id: &str, exec: &str| {
            std::fs::write(
                root.join(format!("home/.local/share/applications/{id}.desktop")),
                format!("[Desktop Entry]\nType=Application\nName={id}\nExec={exec}\n"),
            )
            .unwrap();
        };
        desktop("foot", "foot");
        desktop(
            "WhatsApp",
            "omarchy-launch-webapp https://web.whatsapp.com/",
        );
        for exe in ["foot", "omarchy-launch-webapp"] {
            let p = root.join("bin").join(exe);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let mut pwa = client(
            "0xa1",
            "chrome-web.whatsapp.com__-Default",
            "web.whatsapp.com_/",
            2,
            NO_PID,
        );
        pwa["title"] = json!("WhatsApp");
        let state = State {
            version: fixture("version"),
            monitors: fixture("monitors"),
            workspaces: fixture("workspaces"),
            active: fixture("activeworkspace"),
            clients: vec![
                pwa,
                client("0xa2", "foot", "~/proj", 3, NO_PID),
                client("0xa3", "mystery", "?", 1, NO_PID),
                client("0xa4", "org.keepassxc.KeePassXC", "vault", 1, NO_PID),
            ],
            lua: true,
            spawns: vec![("foot".into(), client("", "foot", "~", 1, NO_PID))],
            ..Default::default()
        };
        let hypr = Self {
            root,
            state: Arc::new(Mutex::new(state)),
        };
        hypr.listen();
        hypr
    }

    fn instance(&self) -> PathBuf {
        self.root.join("run/hypr").join(SIG)
    }

    fn listen(&self) {
        std::fs::create_dir_all(self.instance()).unwrap();
        let requests = UnixListener::bind(self.instance().join(".socket.sock")).unwrap();
        let events = UnixListener::bind(self.instance().join(".socket2.sock")).unwrap();
        let state = self.state.clone();
        std::thread::spawn(move || {
            for mut conn in requests.incoming().flatten() {
                let mut buf = vec![0; 1 << 16];
                let n = conn.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                let reply = handle(&state, &req);
                let _ = conn.write_all(reply.as_bytes());
            }
        });
        let state = self.state.clone();
        std::thread::spawn(move || {
            for mut conn in events.incoming().flatten() {
                let mut s = state.lock().unwrap();
                let _ = conn.write_all(&s.prelude);
                s.subscribers.push(conn);
            }
        });
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_hyprstate"));
        c.args(args).env_clear();
        // Keep coverage instrumentation working in the child.
        for (k, v) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("LLVM_PROFILE") {
                c.env(k, v);
            }
        }
        c.env("HYPRLAND_INSTANCE_SIGNATURE", SIG)
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .env("XDG_STATE_HOME", self.root.join("home/.local/state"))
            .env("XDG_DATA_HOME", self.root.join("home/.local/share"))
            .env("XDG_DATA_DIRS", self.root.join("nowhere"))
            .env("PATH", self.root.join("bin"));
        c
    }

    fn run(&self, args: &[&str]) -> Run {
        Run::from(self.cmd(args).output().unwrap())
    }

    fn ok(&self, args: &[&str]) -> String {
        let r = self.run(args);
        assert_eq!(
            r.code, 0,
            "{args:?}\nstdout:\n{}\nstderr:\n{}",
            r.out, r.err
        );
        r.out
    }

    fn fails(&self, args: &[&str], code: i32) -> Run {
        let r = self.run(args);
        assert_eq!(
            r.code, code,
            "{args:?}\nstdout:\n{}\nstderr:\n{}",
            r.out, r.err
        );
        r
    }

    fn snapshots(&self) -> PathBuf {
        self.root.join("home/.local/state/hyprstate/snapshots")
    }

    fn worksets(&self) -> PathBuf {
        self.root.join("home/.config/hyprstate/worksets")
    }
}

impl Drop for Hypr {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

impl From<Output> for Run {
    fn from(o: Output) -> Self {
        Self {
            code: o.status.code().unwrap(),
            out: String::from_utf8(o.stdout).unwrap(),
            err: String::from_utf8(o.stderr).unwrap(),
        }
    }
}

fn handle(state: &Arc<Mutex<State>>, req: &str) -> String {
    let mut s = state.lock().unwrap();
    if let Some(query) = req.strip_prefix("j/") {
        if s.garbage.iter().any(|g| g == query) {
            return "not json".into();
        }
        return match query {
            "version" => s.version.to_string(),
            "monitors" => s.monitors.to_string(),
            "workspaces" => s.workspaces.to_string(),
            "activeworkspace" => s.active.to_string(),
            "clients" => Value::from(s.clients.clone()).to_string(),
            _ => "unknown request".into(),
        };
    }
    let Some(d) = req.strip_prefix("dispatch ") else {
        return "unknown request".into();
    };
    if d == "hl.dsp.no_op()" {
        return if s.lua { "ok" } else { "Invalid dispatcher" }.into();
    }
    s.dispatches.push(d.to_string());
    if s.reject.iter().any(|r| d.contains(r.as_str())) {
        return "error: rejected".into();
    }
    let exec = between(d, "exec_cmd(", ", {").or_else(|| d.strip_prefix("exec "));
    if let Some(line) = exec {
        let ws = between(d, "workspace = \"", " silent")
            .or_else(|| between(d, "[workspace ", " silent"))
            .unwrap_or("1")
            .to_string();
        let template = s
            .spawns
            .iter()
            .find(|(k, _)| line.contains(k.as_str()))
            .map(|(_, c)| c.clone());
        if let Some(mut c) = template {
            s.next += 1;
            let addr = format!("5eed{}", s.next);
            let ws = if s.ignore_exec_workspace {
                "1".to_string()
            } else {
                ws
            };
            c["address"] = json!(format!("0x{addr}"));
            c["workspace"] = json!({ "id": ws.parse::<i64>().unwrap_or(-1), "name": ws });
            let event =
                format!("openwindow>>{addr},{ws},{},{}\n", c["class"], c["title"]).replace('"', "");
            let delay = s.spawn_delay;
            let state = Arc::clone(state);
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                let subs: Vec<UnixStream> = {
                    let mut s = state.lock().unwrap();
                    s.clients.push(c);
                    s.subscribers
                        .iter()
                        .filter_map(|x| x.try_clone().ok())
                        .collect()
                };
                // Split mid-line: the reader must reassemble it.
                let (a, b) = event.split_at(6);
                for mut sub in &subs {
                    let _ = sub.write_all(a.as_bytes());
                }
                std::thread::sleep(Duration::from_millis(20));
                for mut sub in &subs {
                    let _ = sub.write_all(b.as_bytes());
                }
            });
        }
        return "ok".into();
    }
    let target = between(d, "workspace = \"", "\"")
        .or_else(|| between(d, "movetoworkspacesilent ", ","))
        .map(str::to_string);
    let address = between(d, "address:", "\"")
        .or_else(|| d.split("address:").nth(1))
        .map(str::to_string);
    if let (Some(ws), Some(addr)) = (target, address) {
        for c in &mut s.clients {
            if c["address"] == json!(addr) {
                c["workspace"] = json!({ "id": ws.parse::<i64>().unwrap_or(-1), "name": ws });
            }
        }
    }
    "ok".into()
}

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn snapshot_list_inspect_diff() {
    let h = Hypr::new();
    assert!(h.ok(&["list"]).starts_with("No snapshots in ~/"));
    assert_eq!(h.ok(&["list", "--json"]).trim(), "[]");

    let out = h.ok(&["snapshot", "--name", "a"]);
    assert!(
        out.starts_with("Saved snapshot a\n  3 windows on 3 workspaces (1 excluded by policy)"),
        "{out}"
    );
    let r = h.fails(&["snapshot", "--name", "a"], 1);
    assert!(r.err.contains("snapshot 'a' already exists"), "{}", r.err);
    let v: Value =
        serde_json::from_str(&h.ok(&["snapshot", "--name", "a", "--force", "--json"])).unwrap();
    assert_eq!(
        (
            v["name"].as_str(),
            v["windows"].as_u64(),
            v["excluded"].as_u64()
        ),
        (Some("a"), Some(3), Some(1))
    );
    assert!(h.ok(&["snapshot"]).starts_with("Saved snapshot 20"));

    write(&h.snapshots().join("broken.json"), "{");
    let r = h.run(&["list"]);
    assert_eq!(r.code, 0);
    assert!(r.out.starts_with("NAME"), "{}", r.out);
    assert!(r.err.contains("unreadable snapshot"), "{}", r.err);
    let v: Value = serde_json::from_str(&h.ok(&["list", "--json"])).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2);

    let out = h.ok(&["inspect"]);
    assert!(
        out.contains("Applications\n  foot\n  mystery\n  web.whatsapp.com\n"),
        "{out}"
    );
    let out = h.ok(&["inspect", "a", "--windows"]);
    assert!(out.contains("resolved:   desktop id = class"), "{out}");
    let v: Value = serde_json::from_str(&h.ok(&["inspect", "a", "--json"])).unwrap();
    assert_eq!(v["name"], "a");

    assert!(
        h.ok(&["diff", "a"])
            .starts_with("No differences between a and live")
    );
    h.state().clients[1]["workspace"] = json!({ "id": 5, "name": "5" });
    let v: Value = serde_json::from_str(&h.ok(&["diff", "a", "--json"])).unwrap();
    assert_eq!(v["changed"][0]["changes"][0]["to"], "5");
    h.ok(&["snapshot", "--name", "b"]);
    let out = h.ok(&["diff", "a", "b"]);
    assert!(out.contains("workspace: 3 → 5"), "{out}");
}

#[test]
fn restore_relaunches_and_places() {
    let h = Hypr::new();
    h.ok(&["snapshot", "--name", "before"]);
    // "Reboot": the terminal is gone, the PWA moved.
    {
        let mut s = h.state();
        s.clients
            .retain(|c| c["class"] != "foot" && c["class"] != "mystery");
        s.clients[0]["workspace"] = json!({ "id": 4, "name": "4" });
        s.prelude = b"workspace>>1\nnot an event\n".to_vec();
        s.spawn_delay = Duration::from_millis(150);
    }
    let out = h.ok(&["restore", "--dry-run"]);
    assert!(out.contains("  + foot  (launch: foot)"), "{out}");
    assert!(
        out.contains("  ✓ web.whatsapp.com  (move → workspace 2)"),
        "{out}"
    );
    assert!(
        out.contains("  ✗ mystery\n      reason: owning process not found"),
        "{out}"
    );
    let v: Value =
        serde_json::from_str(&h.ok(&["restore", "before", "--dry-run", "--json"])).unwrap();
    assert_eq!(v["summary"]["launch"], 1);

    // The unresolvable window makes it a partial restore: exit code 2.
    let r = h.fails(&["restore", "--timeout", "5", "-v"], 2);
    assert!(r.out.contains("  ✓ foot  (launched)"), "{}", r.out);
    assert!(r.out.contains("  ✓ web.whatsapp.com  (moved)"), "{}", r.out);
    assert!(r.err.contains("launched"), "{}", r.err);
    let s = h.state();
    assert!(
        s.dispatches
            .iter()
            .any(|d| d.starts_with("hl.dsp.exec_cmd(\"foot\"")),
        "{:?}",
        s.dispatches
    );
    let foot = s.clients.iter().find(|c| c["class"] == "foot").unwrap();
    assert_eq!(foot["workspace"]["name"], "3");
    drop(s);

    // Excluding the mystery window leaves nothing to fail; restoring again
    // changes nothing.
    let cfg = h.root.join("home/.config/hyprstate/config.toml");
    write(&cfg, "[policy]\nexclude = [\"mystery\"]\n");
    let v: Value = serde_json::from_str(&h.ok(&["restore", "--json"])).unwrap();
    let outcomes: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, ["unchanged", "unchanged", "excluded"]);
    assert!(
        h.ok(&["restore", "--dry-run"])
            .contains("Desktop already matches the snapshot.")
    );
}

#[test]
fn legacy_dialect_and_broken_event_stream() {
    let h = Hypr::new();
    h.ok(&["snapshot", "--name", "s"]);
    {
        let mut s = h.state();
        s.lua = false;
        s.clients.retain(|c| c["class"] != "foot");
        // Not UTF-8: the event reader errors and the executor polls instead.
        s.prelude = b"\xff\xfe\n".to_vec();
        s.spawn_delay = Duration::from_millis(150);
        s.ignore_exec_workspace = true;
        s.reject.push("movetoworkspacesilent".into());
    }
    let cfg = h.root.join("cfg.toml");
    write(
        &cfg,
        "[policy]\nexclude = [\"mystery\"]\n[restore]\nlaunch_wrapper = [\"foot\"]\n",
    );
    let r = h.fails(
        &[
            "--config",
            cfg.to_str().unwrap(),
            "restore",
            "--log-format",
            "json",
            "-vv",
        ],
        2,
    );
    assert!(r.out.contains("could not place"), "{}", r.out);
    assert!(r.err.contains("\"event\":\"failure\""), "{}", r.err);
    let s = h.state();
    assert!(
        s.dispatches
            .iter()
            .any(|d| d.starts_with("exec [workspace 3 silent] foot foot")),
        "{:?}",
        s.dispatches
    );
}

/// Replaces the event socket with one that sends `bytes` to each subscriber,
/// then hangs up (`close`) or keeps the connection open and silent.
fn serve_events(h: &Hypr, bytes: &'static [u8], close: bool) {
    let path = h.instance().join(".socket2.sock");
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        let mut open = Vec::new();
        for mut conn in listener.incoming().flatten() {
            let _ = conn.write_all(bytes);
            if !close {
                open.push(conn);
            }
        }
    });
}

#[test]
fn event_socket_failures() {
    let h = Hypr::new();
    h.ok(&["snapshot", "--name", "s"]);
    {
        let mut s = h.state();
        s.clients.retain(|c| c["class"] != "foot");
        s.spawn_delay = Duration::from_millis(150);
    }
    let cfg = h.root.join("cfg.toml");
    write(&cfg, "[policy]\nexclude = [\"mystery\"]\n");
    let c = cfg.to_str().unwrap();

    std::fs::remove_file(h.instance().join(".socket2.sock")).unwrap();
    let r = h.fails(&["--config", c, "restore"], 1);
    assert!(r.err.contains("connecting to Hyprland events"), "{}", r.err);

    // The stream ends (cleanly, or mid-line): the executor polls instead.
    for bytes in [&b""[..], b"openwindow>>"] {
        serve_events(&h, bytes, true);
        let r = h.run(&["--config", c, "restore", "--timeout", "2"]);
        assert_eq!(r.code, 0, "{}\n{}", r.out, r.err);
        assert!(r.out.contains("  ✓ foot  (launched)"), "{}", r.out);
        assert!(r.err.contains("event socket failed"), "{}", r.err);
        h.state().clients.retain(|c| c["class"] != "foot");
    }

    // Silence until the deadline: the window never shows up.
    serve_events(&h, b"", false);
    h.state().spawns.clear();
    let r = h.fails(&["--config", c, "restore", "--timeout", "1"], 2);
    assert!(
        r.out.contains("  ✗ foot  (window did not appear)"),
        "{}",
        r.out
    );
}

#[test]
fn worksets() {
    let h = Hypr::new();
    assert!(h.ok(&["workset", "list"]).starts_with("No worksets in ~/"));
    let out = h.ok(&["workset", "create", "dev"]);
    assert!(out.contains("Created workset dev"), "{out}");
    let r = h.fails(&["workset", "create", "dev"], 1);
    assert!(r.err.contains("already exists"), "{}", r.err);
    let r = h.fails(&["workset", "open", "dev"], 1);
    assert!(
        r.err.contains("workset 'dev' has no windows; edit"),
        "{}",
        r.err
    );

    let r = h.fails(&["workset", "save", "empty", "-w", "9"], 1);
    assert!(r.err.contains("nothing to save"), "{}", r.err);
    let out = h.ok(&["workset", "save", "dev", "-w", "1", "-w", "2", "-w", "3"]);
    assert!(out.contains("Saved workset dev (2 windows)"), "{out}");
    assert!(out.contains("previous version: ~/"), "{out}");
    assert!(
        out.contains("✗ mystery on workspace 1: owning process not found"),
        "{out}"
    );

    // A hand-written description survives the next save.
    let path = h.worksets().join("dev.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("description = \"Daily\"\n{text}")).unwrap();
    h.ok(&["workset", "save", "dev"]);
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("description = \"Daily\"")
    );

    write(&h.worksets().join("broken.toml"), "windows = 3");
    let out = h.ok(&["workset", "list"]);
    assert!(out.contains("dev     2        Daily"), "{out}");
    assert!(out.contains("broken  ✗ invalid"), "{out}");
    let v: Value = serde_json::from_str(&h.ok(&["workset", "list", "--json"])).unwrap();
    assert!(v[0]["error"].is_string() && v[1]["windows"] == 2, "{v}");

    h.state().clients.retain(|c| c["class"] != "foot");
    assert!(
        h.ok(&["workset", "open", "dev", "--dry-run"])
            .starts_with("Workset: dev\n")
    );
    let v: Value =
        serde_json::from_str(&h.ok(&["workset", "open", "dev", "--dry-run", "--json"])).unwrap();
    assert_eq!(v["summary"]["launch"], 1);
    let out = h.ok(&["workset", "open", "dev", "--timeout", "5"]);
    assert!(out.contains("  ✓ foot  (launched)"), "{out}");
    let v: Value = serde_json::from_str(&h.ok(&["workset", "open", "dev", "--json"])).unwrap();
    assert!(
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["outcome"] == "unchanged"),
        "{v}"
    );

    // `~` alone stays as it is when there is no $HOME to expand it to.
    std::fs::write(
        &path,
        "[[windows]]\nworkspace = 1\ncommand = \"foot ~\"\nreuse = false\n",
    )
    .unwrap();
    let r = Run::from(
        h.cmd(&["workset", "open", "dev", "--dry-run", "--json"])
            .env_remove("HOME")
            .output()
            .unwrap(),
    );
    assert!(r.out.contains("\"~\""), "{}\n{}", r.out, r.err);

    assert!(
        h.ok(&["workset", "delete", "dev"])
            .starts_with("Deleted workset dev")
    );
    h.fails(&["workset", "delete", "dev"], 1);
    h.fails(&["workset", "save", "../x"], 1);
}

#[test]
fn environment_and_compositor_errors() {
    let h = Hypr::new();
    let err = |c: &mut Command| Run::from(c.output().unwrap());

    let r = err(h
        .cmd(&["inspect"])
        .env_remove("HYPRLAND_INSTANCE_SIGNATURE"));
    assert!(
        r.err.contains("HYPRLAND_INSTANCE_SIGNATURE is not set"),
        "{}",
        r.err
    );
    let r = err(h.cmd(&["inspect"]).env_remove("XDG_RUNTIME_DIR"));
    assert!(r.err.contains("XDG_RUNTIME_DIR is not set"), "{}", r.err);
    let r = err(h
        .cmd(&["inspect"])
        .env("HYPRLAND_INSTANCE_SIGNATURE", "other"));
    assert!(r.err.contains("Hyprland socket not found"), "{}", r.err);
    let r = err(h
        .cmd(&["list"])
        .env_remove("HOME")
        .env("XDG_STATE_HOME", ""));
    assert!(r.err.contains("HOME is not set"), "{}", r.err);
    let r = err(h.cmd(&["list"]).env("XDG_STATE_HOME", ""));
    assert!(
        r.out.starts_with("No snapshots in ~/.local/state/"),
        "{}",
        r.out
    );
    let r = err(h
        .cmd(&["workset", "list"])
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME"));
    assert!(r.err.contains("HOME is not set"), "{}", r.err);

    // Fallbacks to $HOME, the default data dirs and a HYPRSTATE_LOG filter.
    let r = err(h
        .cmd(&["inspect"])
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("XDG_DATA_DIRS", "")
        .env("HYPRSTATE_LOG", "debug"));
    assert_eq!(r.code, 0, "{}", r.err);
    let r = err(h
        .cmd(&["inspect"])
        .env_remove("XDG_DATA_HOME")
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME"));
    assert_eq!(r.code, 0, "{}", r.err);

    h.state().garbage.push("monitors".into());
    let r = h.fails(&["inspect"], 1);
    assert!(r.err.contains("parsing reply to j/monitors"), "{}", r.err);

    // A socket file nobody listens on.
    let dead = h.root.join("run/hypr/dead");
    std::fs::create_dir_all(&dead).unwrap();
    drop(UnixListener::bind(dead.join(".socket.sock")).unwrap());
    let r = err(h
        .cmd(&["inspect"])
        .env("HYPRLAND_INSTANCE_SIGNATURE", "dead"));
    assert!(r.err.contains("connecting to"), "{}", r.err);

    // Found through $HOME, and a typo is an error rather than ignored.
    write(
        &h.root.join("home/.config/hyprstate/config.toml"),
        "bogus = 1",
    );
    let r = err(h.cmd(&["list"]).env_remove("XDG_CONFIG_HOME"));
    assert!(r.err.contains("parsing"), "{}", r.err);
}
