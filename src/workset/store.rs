//! `~/.config/hyprstate/worksets/<name>.toml` — user-editable, so they live
//! with the configuration rather than with captured state.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::Workset;
use crate::snapshot::storage::validate_name;

pub const TEMPLATE: &str = r#"# hyprstate workset — the desktop you want, not the one you had.
# Open it with: hyprstate workset open NAME [--dry-run]
#
# Each [[windows]] entry is one window:
#   workspace       = 1 | "special:scratch" | "web"
#   command         = "shell-style command line"  (~/ is expanded)
#   cwd             = "~/dir"   launch directory; an open window is only reused
#                               if it is in this directory (terminals)
#   class           = "..."     window class, when the command is not enough
#   title_contains  = "..."     only reuse an open window whose title has this
#   reuse           = false     always launch a new window
#   floating        = true      with position = [x, y] and size = [w, h]

# description = "What this workset is for"

# [[windows]]
# workspace = 1
# command = "code ~/projects/example"

# [[windows]]
# workspace = 2
# command = "foot"
# cwd = "~/projects/example"

# [[windows]]
# workspace = 3
# command = "omarchy-launch-webapp https://github.com/"
"#;

const HEADER: &str = "# hyprstate workset. Saved from the live desktop; edit freely.\n# Keys: see `hyprstate workset create` template or the README.\n\n";

pub struct WorksetStore {
    dir: PathBuf,
}

impl WorksetStore {
    pub fn default_dir() -> Result<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(d) if !d.is_empty() => PathBuf::from(d),
            _ => {
                PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config")
            }
        };
        Ok(base.join("hyprstate").join("worksets"))
    }

    pub fn open_default() -> Result<Self> {
        Ok(Self::new(Self::default_dir()?))
    }

    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        Ok(self.dir.join(format!("{name}.toml")))
    }

    pub fn load(&self, name: &str) -> Result<Workset> {
        let path = self.path(name)?;
        if !path.exists() {
            bail!("workset '{name}' not found (expected {})", path.display());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    /// New workset from the commented template. Never overwrites.
    pub fn create(&self, name: &str) -> Result<PathBuf> {
        let path = self.path(name)?;
        if path.exists() {
            bail!("workset '{name}' already exists: {}", path.display());
        }
        self.write(&path, TEMPLATE)?;
        Ok(path)
    }

    /// Writes a workset. An existing file is kept as `<name>.toml.bak`, since
    /// saving replaces hand-written comments.
    pub fn save(&self, name: &str, ws: &Workset) -> Result<(PathBuf, Option<PathBuf>)> {
        let path = self.path(name)?;
        let backup = if path.exists() {
            let bak = self.dir.join(format!("{name}.toml.bak"));
            std::fs::copy(&path, &bak)?;
            Some(bak)
        } else {
            None
        };
        let body = toml::to_string_pretty(ws)?;
        self.write(&path, &format!("{HEADER}{body}"))?;
        Ok((path, backup))
    }

    pub fn delete(&self, name: &str) -> Result<PathBuf> {
        let path = self.path(name)?;
        if !path.exists() {
            bail!("workset '{name}' not found");
        }
        std::fs::remove_file(&path)?;
        Ok(path)
    }

    /// Names with their parsed contents (or the parse error).
    pub fn list(&self) -> Result<Vec<(String, Result<Workset>)>> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(str::to_string))
            .filter(|n| validate_name(n).is_ok())
            .collect();
        names.sort();
        Ok(names
            .into_iter()
            .map(|n| {
                let ws = self.load(&n);
                (n, ws)
            })
            .collect())
    }

    fn write(&self, path: &Path, content: &str) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workset::{Entry, WorkspaceSpec};

    #[test]
    fn lifecycle() {
        let dir = std::env::temp_dir().join(format!("hyprstate-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = WorksetStore::new(dir.clone());

        store.create("boticario").unwrap();
        assert!(store.create("boticario").is_err());
        // The template parses and declares nothing.
        assert!(store.load("boticario").unwrap().windows.is_empty());

        let ws = Workset {
            description: Some("x".into()),
            windows: vec![Entry {
                workspace: WorkspaceSpec::Id(1),
                command: "foot".into(),
                cwd: None,
                class: None,
                title_contains: None,
                reuse: true,
                floating: false,
                position: None,
                size: None,
            }],
        };
        let (_, bak) = store.save("boticario", &ws).unwrap();
        assert!(bak.is_some());
        assert_eq!(store.load("boticario").unwrap(), ws);
        assert_eq!(store.list().unwrap().len(), 1);
        store.delete("boticario").unwrap();
        assert!(store.load("boticario").is_err());
        assert!(store.path("../x").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_and_unreadable_dirs() {
        let dir = std::env::temp_dir().join(format!("hyprstate-ws-err-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = WorksetStore::new(dir.clone());
        assert_eq!(store.dir(), dir);
        assert!(store.list().unwrap().is_empty());
        assert!(store.delete("x").is_err());
        let ws = Workset {
            description: None,
            windows: Vec::new(),
        };
        let (_, bak) = store.save("fresh", &ws).unwrap();
        assert!(bak.is_none());
        let file = dir.join("fresh.toml");
        assert!(WorksetStore::new(file).list().is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn filesystem_errors_name_the_path() {
        let dir = std::env::temp_dir().join(format!("hyprstate-ws-fs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("d.toml")).unwrap();
        let store = WorksetStore::new(dir.clone());
        let err = format!("{:#}", store.load("d").unwrap_err());
        assert!(err.starts_with("reading "), "{err}");
        std::fs::write(dir.join("file"), "").unwrap();
        let blocked = WorksetStore::new(dir.join("file/sub"));
        let err = format!("{:#}", blocked.create("x").unwrap_err());
        assert!(err.starts_with("creating "), "{err}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
