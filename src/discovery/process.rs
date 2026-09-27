//! Reads what `/proc/<pid>` says about a window's owner.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: i32,
    pub exe: Option<PathBuf>,
    /// Raw argv. Never persisted without passing through `policy::security`.
    pub cmdline: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// First line of `/proc/<pid>/cgroup` (unified hierarchy path).
    pub cgroup: Option<String>,
    /// cwd of the first child (a terminal's shell), when there is one.
    pub child_cwd: Option<PathBuf>,
    /// Allowlisted environment values; the rest of `environ` is never read into memory.
    /// Only set when `GIO_LAUNCHED_DESKTOP_FILE_PID` is this process (children
    /// inherit the variable from whatever launched their parent).
    pub env_desktop_file: Option<PathBuf>,
    pub env_flatpak_id: Option<String>,
}

pub fn read_process(pid: i32) -> ProcessInfo {
    read_process_at(Path::new("/proc"), pid)
}

pub fn read_process_at(proc_root: &Path, pid: i32) -> ProcessInfo {
    let dir = proc_root.join(pid.to_string());
    let cmdline = std::fs::read(dir.join("cmdline"))
        .map(|b| {
            b.split(|&c| c == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect()
        })
        .unwrap_or_default();
    let cgroup = std::fs::read_to_string(dir.join("cgroup"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("0::").map(str::to_string))
        });
    let child_cwd =
        std::fs::read_to_string(dir.join("task").join(pid.to_string()).join("children"))
            .ok()
            .and_then(|s| s.split_whitespace().next().map(str::to_string))
            .and_then(|child| std::fs::read_link(proc_root.join(child).join("cwd")).ok());
    let (env_desktop_file, env_flatpak_id) = read_env_allowlist(&dir.join("environ"), pid);
    ProcessInfo {
        pid,
        exe: std::fs::read_link(dir.join("exe")).ok(),
        cmdline,
        cwd: std::fs::read_link(dir.join("cwd")).ok(),
        cgroup,
        child_cwd,
        env_desktop_file: env_desktop_file.map(PathBuf::from),
        env_flatpak_id,
    }
}

fn read_env_allowlist(path: &Path, pid: i32) -> (Option<String>, Option<String>) {
    let Ok(bytes) = std::fs::read(path) else {
        return (None, None);
    };
    let mut desktop = None;
    let mut desktop_pid = None;
    let mut flatpak = None;
    for entry in bytes.split(|&c| c == 0) {
        let Ok(entry) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((k, v)) = entry.split_once('=') else {
            continue;
        };
        // The allowlist: every other variable is skipped unread.
        match k {
            "GIO_LAUNCHED_DESKTOP_FILE" => desktop = Some(v.to_string()),
            "GIO_LAUNCHED_DESKTOP_FILE_PID" => desktop_pid = v.parse::<i32>().ok(),
            "FLATPAK_ID" => flatpak = Some(v.to_string()),
            _ => continue,
        }
    }
    (desktop.filter(|_| desktop_pid == Some(pid)), flatpak)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_own_process() {
        let me = std::process::id() as i32;
        let info = read_process(me);
        assert_eq!(info.pid, me);
        assert!(info.exe.is_some());
        assert!(!info.cmdline.is_empty());
        assert!(info.cgroup.is_some());
    }

    #[test]
    fn missing_process_is_empty_not_error() {
        let info = read_process(i32::MAX);
        assert!(info.exe.is_none() && info.cmdline.is_empty());
    }

    #[test]
    fn reads_allowlisted_env_and_child_cwd_from_a_proc_tree() {
        let root = std::env::temp_dir().join(format!("hyprstate-proc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let me = root.join("100");
        std::fs::create_dir_all(me.join("task/100")).unwrap();
        std::fs::create_dir_all(root.join("101")).unwrap();
        std::fs::write(me.join("task/100/children"), "101 102\n").unwrap();
        std::os::unix::fs::symlink("/home/me/proj", root.join("101/cwd")).unwrap();
        let mut env = Vec::new();
        for e in [
            &b"PATH=/usr/bin"[..],
            b"NOT_A_PAIR",
            b"\xff\xfe=bad utf8",
            b"GIO_LAUNCHED_DESKTOP_FILE=/apps/foot.desktop",
            b"GIO_LAUNCHED_DESKTOP_FILE_PID=100",
            b"FLATPAK_ID=org.x.Y",
        ] {
            env.extend_from_slice(e);
            env.push(0);
        }
        std::fs::write(me.join("environ"), &env).unwrap();

        let info = read_process_at(&root, 100);
        assert_eq!(info.env_desktop_file, Some("/apps/foot.desktop".into()));
        assert_eq!(info.env_flatpak_id.as_deref(), Some("org.x.Y"));
        assert_eq!(info.child_cwd, Some("/home/me/proj".into()));

        // A process whose environment cannot be read has no launch hints.
        let info = read_process_at(&root, 101);
        assert_eq!(info.env_desktop_file, None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
