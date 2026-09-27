pub mod desktop;
pub mod flatpak;
pub mod process;
pub mod resolve;
pub mod systemd;
pub mod webapp;

use std::path::PathBuf;

/// First executable named `name` on `$PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_executable(&p).then_some(p);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_accepts_paths() {
        assert_eq!(which("/bin/sh"), Some(PathBuf::from("/bin/sh")));
        assert_eq!(which("/nonexistent/hyprstate"), None);
        assert_eq!(which("/etc/passwd"), None);
        assert!(which("sh").is_some());
        assert_eq!(which("hyprstate-definitely-not-installed"), None);
    }
}
