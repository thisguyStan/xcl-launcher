//! Cache cleaning: what can be deleted, how big it is, and deleting what the user picked.
//!
//! Only caches, temporary files and old logs are offered. Saves, the overlay (the game's own
//! writes), persistent local storage, `xcl.toml` and the current logs are never touched.

use std::path::{Path, PathBuf};

use crate::config::AppConfig;
use crate::game::Game;
use crate::paths;

/// Folders XCL's own tests leave in the data root.
const TEST_LEFTOVER_PREFIXES: [&str; 3] = ["xcl-multi-exe-", "xcl-no-exe-", "xcl-self-copy-"];

#[derive(Debug, Clone)]
pub struct Target {
    pub group: Group,
    pub label: String,
    /// What it is and what deleting it means.
    pub hint: &'static str,
    /// Files and folders deleted when this target is cleaned.
    pub paths: Vec<PathBuf>,
    pub bytes: u64,
    pub files: u64,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Shaders,
    Games,
    Other,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::Shaders => "Shader cache (all games)",
            Group::Games => "Per-game caches",
            Group::Other => "Other",
        }
    }
}

/// Everything that can be cleaned, with sizes. Slow on a big shader cache: run it off the UI thread.
pub fn find_targets(cfg: &AppConfig, games: &[Game], launcher_logs: &Path) -> Vec<Target> {
    let mut out = Vec::new();
    let mut add = |group, label: String, hint, paths: Vec<PathBuf>| {
        let (bytes, files) = paths
            .iter()
            .map(|p| size_of(p))
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        if !paths.is_empty() {
            out.push(Target {
                group,
                label,
                hint,
                paths,
                bytes,
                files,
                selected: false,
            });
        }
    };

    if let Some(dxil) = paths::shader_cache(cfg) {
        let compiled = children(&dxil, |p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("dxil"))
        });
        add(
            Group::Shaders,
            "Compiled shaders (DXIL)".into(),
            "Recompiled on the next run of each game, which then starts slower.",
            compiled,
        );
        let translated = paths::child_ci(&dxil, "translated");
        add(
            Group::Shaders,
            "Translated shaders".into(),
            "RDNA2 → DXIL translations; redone when needed.",
            children(&translated, |_| true),
        );
        let driver = paths::child_ci(&dxil, "driver");
        add(
            Group::Shaders,
            "Driver pipeline cache".into(),
            "Pipelines built by the GPU driver, including precompiled ones. Rebuilt on demand.",
            children(&driver, |_| true),
        );
        let used = paths::child_ci(&dxil, "used-pipelines.txt");
        add(
            Group::Shaders,
            "List of used pipelines".into(),
            "Which pipelines earlier runs used; 'Precompile (used only)' needs it.",
            existing([used]),
        );
    }

    if let Some(root) = paths::xcl_data_root(cfg) {
        for (title_dir, name) in title_dirs(&root, games) {
            let cache = paths::child_ci(&title_dir, "cache");
            add(
                Group::Games,
                format!("{name}: runtime cache"),
                "Boot record and helper files XCL recreates on every start.",
                children(&cache, |_| true),
            );
            let shaders = paths::child_ci(&title_dir, "shaders");
            add(
                Group::Games,
                format!("{name}: dumped shaders"),
                "Console shaders saved for debugging.",
                children(&shaders, |_| true),
            );
            let temp = paths::child_ci(&title_dir, "temp");
            add(
                Group::Games,
                format!("{name}: temporary files (T:)"),
                "The game's temporary drive.",
                children(&temp, |_| true),
            );
            let logs = paths::child_ci(&title_dir, "logs");
            add(
                Group::Games,
                format!("{name}: previous run's log"),
                "xcl.prev.log and report.prev.json; the last run's log stays.",
                existing([
                    paths::child_ci(&logs, "xcl.prev.log"),
                    paths::child_ci(&logs, "report.prev.json"),
                ]),
            );
        }
        let leftovers = children(&root, |p| {
            p.is_dir()
                && p.file_name().is_some_and(|n| {
                    TEST_LEFTOVER_PREFIXES
                        .iter()
                        .any(|pre| n.to_string_lossy().starts_with(pre))
                })
        });
        add(
            Group::Other,
            "Leftover folders from XCL's tests".into(),
            "xcl-multi-exe-*, xcl-no-exe-*, xcl-self-copy-* in XCL's data folder.",
            leftovers,
        );
    }

    add(
        Group::Other,
        "Launcher console logs".into(),
        "What XCL printed during launches from this launcher.",
        children(launcher_logs, std::path::Path::is_file),
    );
    out
}

/// XCL's per-title folders (those with an `xcl.toml`), named after the game when it is known.
fn title_dirs(root: &Path, games: &[Game]) -> Vec<(PathBuf, String)> {
    let mut dirs: Vec<(PathBuf, String)> = children(root, |p| {
        p.is_dir() && paths::child_ci(p, "xcl.toml").is_file()
    })
    .into_iter()
    .map(|dir| {
        let folder = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = games
            .iter()
            .find(|g| g.title.eq_ignore_ascii_case(&folder))
            .map(|g| g.name.clone())
            .unwrap_or(folder);
        (dir, name)
    })
    .collect();
    dirs.sort_by_cached_key(|(_, n)| n.to_lowercase());
    dirs
}

/// Deletes the selected targets. Returns the bytes freed and the errors.
pub fn clean(targets: &[Target]) -> (u64, Vec<String>) {
    let mut freed = 0;
    let mut errors = Vec::new();
    for t in targets.iter().filter(|t| t.selected) {
        for p in &t.paths {
            let (bytes, _) = size_of(p);
            let result = if p.is_dir() {
                std::fs::remove_dir_all(p)
            } else {
                std::fs::remove_file(p)
            };
            match result {
                Ok(()) => freed += bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => errors.push(format!("{}: {e}", p.display())),
            }
        }
    }
    (freed, errors)
}

fn children(dir: &Path, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| keep(p)).collect();
    v.sort();
    v
}

fn existing<const N: usize>(paths: [PathBuf; N]) -> Vec<PathBuf> {
    paths.into_iter().filter(|p| p.exists()).collect()
}

/// (bytes, files) of a file or a folder tree. Symbolic links are not followed.
fn size_of(path: &Path) -> (u64, u64) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return (0, 0);
    };
    if !meta.is_dir() {
        return (meta.len(), 1);
    }
    let mut total = (0, 0);
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => stack.push(e.path()),
                Ok(m) => total = (total.0 + m.len(), total.1 + 1),
                Err(_) => {}
            }
        }
    }
    total
}

/// "1.5 GB", "320 KB".
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn finds_and_cleans_title_caches_but_keeps_saves() {
        let root = std::env::temp_dir().join(format!("xcl-launcher-clean-{}", std::process::id()));
        let title = root.join("Studio.Game");
        for d in ["cache", "saves", "logs"] {
            std::fs::create_dir_all(title.join(d)).unwrap();
        }
        std::fs::write(title.join("xcl.toml"), "").unwrap();
        std::fs::write(title.join("cache").join("boot.json"), "{}").unwrap();
        std::fs::write(title.join("saves").join("slot"), "save").unwrap();
        std::fs::write(title.join("logs").join("xcl.log"), "log").unwrap();
        std::fs::write(title.join("logs").join("xcl.prev.log"), "old").unwrap();
        std::fs::create_dir_all(root.join("xcl-no-exe-1-2")).unwrap();

        let cfg = AppConfig {
            xcl_data_root: Some(root.clone()),
            ..Default::default()
        };
        let mut targets = find_targets(&cfg, &[], &root.join("none"));
        let games: Vec<_> = targets.iter().filter(|t| t.group == Group::Games).collect();
        assert_eq!(games.len(), 2, "{targets:#?}");
        assert!(targets.iter().any(|t| t.label.starts_with("Leftover")));
        for t in &mut targets {
            t.selected = t.group != Group::Shaders;
        }
        let (_, errors) = clean(&targets);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(!title.join("cache").join("boot.json").exists());
        assert!(!title.join("logs").join("xcl.prev.log").exists());
        assert!(title.join("logs").join("xcl.log").exists());
        assert!(title.join("saves").join("slot").exists());
        assert!(title.join("xcl.toml").exists());
        assert!(!root.join("xcl-no-exe-1-2").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
