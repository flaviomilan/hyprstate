//! Index of XDG desktop entries (`applications/*.desktop`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    /// Desktop file id, e.g. `org.gnome.Nautilus` (without `.desktop`).
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    /// `Exec=` split into argv with field codes removed.
    pub exec: Vec<String>,
    pub startup_wm_class: Option<String>,
    pub no_display: bool,
}

impl DesktopEntry {
    /// Program named by `Exec=`, skipping `env VAR=… ` prefixes.
    pub fn program(&self) -> Option<&str> {
        let mut it = self.exec.iter().map(String::as_str);
        let mut first = it.next()?;
        if first == "env" || first.ends_with("/env") {
            first = it.find(|a| !a.contains('='))?;
        }
        Some(first)
    }
}

pub fn parse_desktop_file(id: &str, path: &Path, content: &str) -> Option<DesktopEntry> {
    let mut in_main = false;
    let mut name = None;
    let mut exec = None;
    let mut wm_class = None;
    let mut no_display = false;
    let mut hidden = false;
    let mut is_app = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "Name" => name = Some(v.trim().to_string()),
            "Exec" => exec = Some(v.trim().to_string()),
            "StartupWMClass" => wm_class = Some(v.trim().to_string()),
            "NoDisplay" => no_display = v.trim() == "true",
            "Hidden" => hidden = v.trim() == "true",
            "Type" => is_app = v.trim() == "Application",
            _ => {}
        }
    }
    if hidden || !is_app {
        return None;
    }
    let exec = parse_exec(&exec?)?;
    Some(DesktopEntry {
        id: id.to_string(),
        path: path.to_path_buf(),
        name: name.unwrap_or_else(|| id.to_string()),
        exec,
        // Packagers sometimes ship unsubstituted placeholders like `@@startup_wm_class`.
        startup_wm_class: wm_class.filter(|c| !c.is_empty() && !c.contains('@')),
        no_display,
    })
}

/// Splits `Exec=` and drops field codes (`%f %F %u %U %i %c %k`); `%%` → `%`.
pub fn parse_exec(exec: &str) -> Option<Vec<String>> {
    let words = shell_words::split(exec).ok()?;
    let argv: Vec<String> = words
        .into_iter()
        .filter(|w| !(w.len() == 2 && w.starts_with('%') && w != "%%"))
        .map(|w| w.replace("%%", "%"))
        .collect();
    (!argv.is_empty()).then_some(argv)
}

#[derive(Debug, Clone, Default)]
pub struct DesktopIndex {
    entries: Vec<DesktopEntry>,
    by_id: HashMap<String, usize>,
}

impl DesktopIndex {
    pub fn from_entries(entries: Vec<DesktopEntry>) -> Self {
        let mut by_id = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            by_id.entry(e.id.to_ascii_lowercase()).or_insert(i);
        }
        Self { entries, by_id }
    }

    /// Scans `$XDG_DATA_HOME` then `$XDG_DATA_DIRS`; earlier dirs win.
    pub fn load() -> Self {
        let mut dirs = Vec::new();
        match std::env::var_os("XDG_DATA_HOME") {
            Some(d) => dirs.push(PathBuf::from(d)),
            None => {
                if let Some(h) = std::env::var_os("HOME") {
                    dirs.push(PathBuf::from(h).join(".local/share"));
                }
            }
        }
        let data_dirs = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        dirs.extend(data_dirs.split(':').map(PathBuf::from));
        Self::load_from(&dirs.iter().map(|d| d.join("applications")).collect::<Vec<_>>())
    }

    pub fn load_from(app_dirs: &[PathBuf]) -> Self {
        let mut entries = Vec::new();
        for dir in app_dirs {
            scan_dir(dir, dir, &mut entries);
        }
        Self::from_entries(entries)
    }

    pub fn entries(&self) -> &[DesktopEntry] {
        &self.entries
    }

    /// Case-insensitive lookup by desktop id.
    pub fn get(&self, id: &str) -> Option<&DesktopEntry> {
        let id = id.strip_suffix(".desktop").unwrap_or(id);
        self.by_id.get(&id.to_ascii_lowercase()).map(|&i| &self.entries[i])
    }

    pub fn by_path(&self, path: &Path) -> Option<&DesktopEntry> {
        self.entries.iter().find(|e| e.path == path)
    }

    pub fn by_wm_class(&self, class: &str) -> Option<&DesktopEntry> {
        self.entries
            .iter()
            .find(|e| e.startup_wm_class.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(class)))
    }

    /// Entries whose `Exec` program has this basename. Visible entries first,
    /// and among those the one with the shortest Exec (the "plain" launcher,
    /// not "New Incognito Window"-style actions).
    pub fn by_program(&self, basename: &str) -> Option<&DesktopEntry> {
        self.entries
            .iter()
            .filter(|e| {
                e.program()
                    .and_then(|p| Path::new(p).file_name())
                    .is_some_and(|b| b == basename)
            })
            .min_by_key(|e| (e.no_display, e.exec.len()))
    }
}

fn scan_dir(root: &Path, dir: &Path, out: &mut Vec<DesktopEntry>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            scan_dir(root, &path, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "desktop") {
            continue;
        }
        // Desktop file id: path relative to applications/, '/' → '-'.
        let Ok(rel) = path.strip_prefix(root) else { continue };
        let id = rel.to_string_lossy().trim_end_matches(".desktop").replace('/', "-");
        if out.iter().any(|e| e.id == id) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        if let Some(entry) = parse_desktop_file(&id, &path, &content) {
            out.push(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn entry(id: &str, content: &str) -> DesktopEntry {
        parse_desktop_file(id, Path::new(&format!("/x/{id}.desktop")), content).unwrap()
    }

    #[test]
    fn parses_exec_and_ignores_actions() {
        let e = entry(
            "chromium",
            "[Desktop Entry]\nType=Application\nName=Chromium\nExec=/usr/bin/chromium %U\n\
             StartupWMClass=@@startup_wm_class\n[Desktop Action new-private]\nExec=/usr/bin/chromium --incognito\n",
        );
        assert_eq!(e.exec, vec!["/usr/bin/chromium"]);
        assert_eq!(e.startup_wm_class, None);
        assert_eq!(e.name, "Chromium");
    }

    #[test]
    fn exec_quoting_and_env_prefix() {
        let e = entry(
            "x",
            "[Desktop Entry]\nType=Application\nExec=env FOO=1 \"/opt/My App/app\" --x %F 100%%\n",
        );
        assert_eq!(e.exec, vec!["env", "FOO=1", "/opt/My App/app", "--x", "100%"]);
        assert_eq!(e.program(), Some("/opt/My App/app"));
    }

    #[test]
    fn hidden_and_non_apps_are_skipped() {
        let p = Path::new("/x");
        assert!(parse_desktop_file("a", p, "[Desktop Entry]\nType=Link\nExec=x\n").is_none());
        assert!(
            parse_desktop_file("a", p, "[Desktop Entry]\nType=Application\nHidden=true\nExec=x\n")
                .is_none()
        );
    }

    #[test]
    fn index_lookups() {
        let idx = DesktopIndex::from_entries(vec![
            entry("chromium", "[Desktop Entry]\nType=Application\nExec=/usr/bin/chromium %U\n"),
            entry("foot", "[Desktop Entry]\nType=Application\nExec=foot\nStartupWMClass=foot\n"),
            entry("footclient", "[Desktop Entry]\nType=Application\nExec=footclient\n"),
        ]);
        assert_eq!(idx.get("Chromium.desktop").unwrap().id, "chromium");
        assert_eq!(idx.by_wm_class("FOOT").unwrap().id, "foot");
        assert_eq!(idx.by_program("chromium").unwrap().id, "chromium");
        assert!(idx.by_program("firefox").is_none());
    }
}
