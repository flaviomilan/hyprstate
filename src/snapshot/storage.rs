//! `$XDG_STATE_HOME/hyprstate/snapshots/<name>.json`

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::model::{SCHEMA_VERSION, Snapshot};

pub struct Store {
    dir: PathBuf,
}

/// Snapshots that loaded, and files that did not (with why).
pub type Listing = (Vec<Listed>, Vec<(PathBuf, anyhow::Error)>);

#[derive(Debug, Clone)]
pub struct Listed {
    pub name: String,
    pub path: PathBuf,
    pub snapshot: Snapshot,
}

impl Store {
    pub fn default_dir() -> Result<PathBuf> {
        let base = match std::env::var_os("XDG_STATE_HOME") {
            Some(d) if !d.is_empty() => PathBuf::from(d),
            _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
                .join(".local/state"),
        };
        Ok(base.join("hyprstate").join("snapshots"))
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

    /// Atomic write (temp file + rename), readable only by the user.
    pub fn save(&self, snap: &Snapshot, overwrite: bool) -> Result<PathBuf> {
        validate_name(&snap.name)?;
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let path = self.dir.join(snap.file_name());
        if path.exists() && !overwrite {
            bail!(
                "snapshot '{}' already exists (use --force to overwrite)",
                snap.name
            );
        }
        let tmp = self.dir.join(format!(".{}.tmp", snap.name));
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .with_context(|| format!("writing {}", tmp.display()))?;
            serde_json::to_writer_pretty(&mut f, snap)?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// By name, or by path when the argument looks like one.
    pub fn load(&self, name_or_path: &str) -> Result<Snapshot> {
        let path = if name_or_path.contains('/') || name_or_path.ends_with(".json") {
            PathBuf::from(name_or_path)
        } else {
            self.dir.join(format!("{name_or_path}.json"))
        };
        if !path.exists() {
            bail!(
                "snapshot '{name_or_path}' not found in {}",
                self.dir.display()
            );
        }
        load_path(&path)
    }

    /// All readable snapshots, oldest first. Unreadable files are reported, not fatal.
    pub fn list(&self) -> Result<Listing> {
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((ok, bad)),
            Err(e) => return Err(e.into()),
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if path.extension().is_none_or(|e| e != "json") || name.starts_with('.') {
                continue;
            }
            match load_path(&path) {
                Ok(snapshot) => ok.push(Listed {
                    name,
                    path,
                    snapshot,
                }),
                Err(e) => bad.push((path, e)),
            }
        }
        ok.sort_by(|a, b| {
            a.snapshot
                .created_at
                .cmp(&b.snapshot.created_at)
                .then(a.name.cmp(&b.name))
        });
        Ok((ok, bad))
    }

    pub fn latest(&self) -> Result<Snapshot> {
        let (list, _) = self.list()?;
        list.into_iter()
            .next_back()
            .map(|l| l.snapshot)
            .with_context(|| {
                format!(
                    "no snapshots in {} (run `hyprstate snapshot`)",
                    self.dir.display()
                )
            })
    }
}

fn load_path(path: &Path) -> Result<Snapshot> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    let version = v
        .get("schema_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if version != SCHEMA_VERSION as u64 {
        bail!(
            "{}: unsupported schema_version {version} (expected {SCHEMA_VERSION})",
            path.display()
        );
    }
    serde_json::from_value(v).with_context(|| format!("parsing {}", path.display()))
}

pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with('.')
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        bail!("invalid snapshot name '{name}': use letters, digits, '-', '_' and '.'");
    }
    Ok(())
}

/// `2026-09-26T23-42-10` in local time.
pub fn default_name() -> String {
    jiff::Zoned::now().strftime("%Y-%m-%dT%H-%M-%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::capture::build_snapshot_at;
    use crate::snapshot::capture::tests::{fixture_discovery, fixture_live};
    use jiff::Timestamp;

    fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hyprstate-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn save_list_latest_roundtrip() {
        let dir = tempdir("store");
        let store = Store::new(dir.clone());
        let live = fixture_live();
        let wins = fixture_discovery().windows(&live.clients);
        let a = build_snapshot_at(
            "a".into(),
            Timestamp::from_second(10).unwrap(),
            &live,
            &wins,
        );
        let b = build_snapshot_at(
            "b".into(),
            Timestamp::from_second(20).unwrap(),
            &live,
            &wins,
        );
        store.save(&b, false).unwrap();
        store.save(&a, false).unwrap();
        assert!(store.save(&a, false).is_err());
        store.save(&a, true).unwrap();
        assert_eq!(store.latest().unwrap().name, "b");
        assert_eq!(store.list().unwrap().0.len(), 2);
        assert_eq!(store.load("a").unwrap().windows.len(), 4);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("a.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn names() {
        assert!(validate_name("before-reboot").is_ok());
        assert!(validate_name("2026-09-26T23-42-10").is_ok());
        assert!(validate_name("../etc").is_err());
        assert!(validate_name(".hidden").is_err());
        assert_eq!(default_name().len(), 19);
    }

    #[test]
    fn errors_are_reported_not_fatal() {
        let dir = tempdir("errors");
        let store = Store::new(dir.clone());
        assert_eq!(store.dir(), dir);
        assert!(store.list().unwrap().0.is_empty());
        let err = store.latest().unwrap_err().to_string();
        assert!(err.starts_with("no snapshots in"), "{err}");
        assert!(
            store
                .load("nope")
                .unwrap_err()
                .to_string()
                .contains("not found")
        );

        let live = fixture_live();
        let snap = build_snapshot_at(
            "ok".into(),
            Timestamp::UNIX_EPOCH,
            &live,
            &fixture_discovery().windows(&live.clients),
        );
        store.save(&snap, false).unwrap();
        let err = store.save(&snap, false).unwrap_err().to_string();
        assert!(err.contains("'ok' already exists"), "{err}");
        std::fs::write(dir.join("old.json"), r#"{"schema_version": 0}"#).unwrap();
        std::fs::write(dir.join(".hidden.json"), "{}").unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        let (ok, bad) = store.list().unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(bad.len(), 1);
        assert!(format!("{:#}", bad[0].1).contains("unsupported schema_version 0"));

        // By path: anything with a slash or a .json suffix.
        let by_path = dir.join("ok.json");
        assert_eq!(store.load(by_path.to_str().unwrap()).unwrap().name, "ok");

        // A file where the directory should be.
        let file = dir.join("notes.txt");
        assert!(Store::new(file).list().is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn filesystem_errors_name_the_path() {
        let dir = tempdir("fs-errors");
        let store = Store::new(dir.clone());
        let live = fixture_live();
        let snap = build_snapshot_at(
            "x".into(),
            Timestamp::UNIX_EPOCH,
            &live,
            &fixture_discovery().windows(&live.clients),
        );
        fn err<T>(r: Result<T>) -> String {
            format!("{:#}", r.err().unwrap())
        }

        // The temp file's name is taken by a directory.
        std::fs::create_dir_all(dir.join(".x.tmp")).unwrap();
        assert!(err(store.save(&snap, false)).starts_with("writing "));
        // The store directory's place is taken by a file.
        std::fs::write(dir.join("file"), "").unwrap();
        let blocked = Store::new(dir.join("file/sub"));
        assert!(err(blocked.save(&snap, false)).starts_with("creating "));
        // A directory where a snapshot should be.
        std::fs::create_dir_all(dir.join("d.json")).unwrap();
        assert!(err(store.load("d")).starts_with("reading "));
        // Right schema version, wrong shape.
        std::fs::write(
            dir.join("v.json"),
            format!(r#"{{"schema_version": {SCHEMA_VERSION}}}"#),
        )
        .unwrap();
        assert!(err(store.load("v")).starts_with("parsing "));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
