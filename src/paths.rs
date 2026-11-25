//! Where things are: the launcher's own files, XCL's data folders, and how a host path is
//! handed to `xcl.exe` (on Linux it runs under Wine and needs a Windows path).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::config::AppConfig;

/// The launcher's configuration file and log folder.
pub struct LauncherDirs {
    pub config_file: PathBuf,
    pub logs: PathBuf,
}

impl LauncherDirs {
    pub fn new() -> Self {
        // portable use (and tests): a configuration file of one's own, logs next to it
        if let Some(file) = std::env::var_os("XCL_LAUNCHER_CONFIG").filter(|f| !f.is_empty()) {
            let file = PathBuf::from(file);
            let logs = file.parent().unwrap_or(Path::new(".")).join("logs");
            return LauncherDirs {
                config_file: file,
                logs,
            };
        }
        match directories::ProjectDirs::from("", "", "xcl-launcher") {
            Some(d) => LauncherDirs {
                config_file: d.config_dir().join("config.toml"),
                logs: d.data_local_dir().join("logs"),
            },
            // no home folder: keep everything next to the executable
            None => {
                let base = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(Path::to_path_buf))
                    .unwrap_or_else(|| PathBuf::from("."));
                LauncherDirs {
                    config_file: base.join("xcl-launcher.toml"),
                    logs: base.join("xcl-launcher-logs"),
                }
            }
        }
    }
}

/// The Wine prefix the launcher uses (Linux): the configured one, `$WINEPREFIX`, else `~/.wine`.
pub fn wine_prefix(cfg: &AppConfig) -> PathBuf {
    if let Some(p) = cfg
        .wine_prefix
        .as_ref()
        .filter(|p| !p.as_os_str().is_empty())
    {
        return p.clone();
    }
    if let Some(p) = std::env::var_os("WINEPREFIX").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    directories::BaseDirs::new()
        .map_or_else(|| PathBuf::from(".wine"), |d| d.home_dir().join(".wine"))
}

/// `%LOCALAPPDATA%` as XCL sees it: on Linux, the one inside the Wine prefix.
fn local_app_data(cfg: &AppConfig) -> Option<PathBuf> {
    if cfg!(windows) {
        let _ = cfg;
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
        let users = wine_prefix(cfg).join("drive_c").join("users");
        let home = child_ci(&users, &user);
        Some(child_ci(&child_ci(&home, "AppData"), "Local"))
    }
}

/// XCL's data root: the configured `XCL_DATA_ROOT`, else `%LOCALAPPDATA%\XCL`. Every title gets
/// a folder in it.
pub fn xcl_data_root(cfg: &AppConfig) -> Option<PathBuf> {
    if let Some(p) = cfg
        .xcl_data_root
        .as_ref()
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Some(p.clone());
    }
    local_app_data(cfg).map(|l| child_ci(&l, "XCL"))
}

/// XCL's shader cache (`%LOCALAPPDATA%\xcl\dxil`; not moved by `XCL_DATA_ROOT`).
pub fn shader_cache(cfg: &AppConfig) -> Option<PathBuf> {
    if let Some(var) = cfg
        .defaults
        .env
        .iter()
        .find(|v| v.name.trim() == "XCL_DXIL_CACHE" && !v.value.trim().is_empty())
    {
        return Some(from_xcl_path(cfg, var.value.trim()));
    }
    local_app_data(cfg).map(|l| child_ci(&child_ci(&l, "xcl"), "dxil"))
}

/// `base/name`, matching an existing entry regardless of case. Windows and Wine treat names
/// case-insensitively, a Linux file system does not: XCL may have made `xcl` or `XCL`.
pub fn child_ci(base: &Path, name: &str) -> PathBuf {
    let exact = base.join(name);
    if cfg!(windows) || exact.exists() {
        return exact;
    }
    std::fs::read_dir(base)
        .ok()
        .and_then(|rd| {
            rd.flatten()
                .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        })
        .map(|e| e.path())
        .unwrap_or(exact)
}

/// A host path as `xcl.exe` must be given it. Under Wine the host's `/` is the `Z:` drive.
pub fn to_xcl_path(path: &Path) -> OsString {
    if cfg!(windows) {
        return path.as_os_str().to_owned();
    }
    let s = path.to_string_lossy();
    if s.starts_with('/') {
        format!("Z:{}", s.replace('/', "\\")).into()
    } else {
        s.replace('/', "\\").into()
    }
}

/// A Windows path written in a setting, as a host path (the reverse of [`to_xcl_path`]):
/// `Z:\home\…` → `/home/…`, `C:\…` → the prefix's `drive_c`.
pub fn from_xcl_path(cfg: &AppConfig, path: &str) -> PathBuf {
    if cfg!(windows) {
        return PathBuf::from(path);
    }
    let p = path.replace('\\', "/");
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        let rest = p[2..].trim_start_matches('/');
        return match bytes[0].to_ascii_lowercase() {
            b'z' => PathBuf::from("/").join(rest),
            d => wine_prefix(cfg)
                .join(format!("drive_{}", d as char))
                .join(rest),
        };
    }
    PathBuf::from(p)
}

/// The `xcl.exe` to use when none is configured: one next to the launcher.
pub fn guess_xcl() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let p = dir.join("xcl.exe");
    p.is_file().then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(windows))]
    fn unix_paths_become_z_drive_paths() {
        assert_eq!(
            to_xcl_path(Path::new("/home/a/Game")),
            OsString::from("Z:\\home\\a\\Game")
        );
        assert_eq!(
            from_xcl_path(&AppConfig::default(), "Z:\\home\\a"),
            PathBuf::from("/home/a")
        );
    }

    #[test]
    #[cfg(windows)]
    fn windows_paths_are_unchanged() {
        assert_eq!(
            to_xcl_path(Path::new(r"D:\Games\X")),
            OsString::from(r"D:\Games\X")
        );
    }
}
