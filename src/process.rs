//! Starting `xcl.exe` without a console window and keeping track of what it is doing.
//!
//! XCL's own log (`<data>/<title>/logs/xcl.log`) is written by XCL as always; what XCL prints
//! to its console goes to a file in the launcher's log folder instead of a window.

use std::collections::VecDeque;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::config::{AppConfig, LaunchSettings};
use crate::game::Game;
use crate::paths;

/// A command line for `xcl.exe` (wrapped in the Wine runner on Linux), with no console window.
fn xcl_command(cfg: &AppConfig) -> Result<Command> {
    let xcl = cfg
        .xcl_path
        .as_ref()
        .filter(|p| !p.as_os_str().is_empty())
        .context("the path to xcl.exe is not set (Settings)")?;
    if !xcl.is_file() {
        bail!("xcl.exe not found: {}", xcl.display());
    }
    let mut cmd = if cfg!(windows) {
        Command::new(xcl)
    } else {
        let runner = cfg.runner.trim();
        if runner.is_empty() {
            bail!("no runner set for xcl.exe (Settings, e.g. 'wine')");
        }
        let mut c = Command::new(runner);
        let runner_args = shlex::split(&cfg.runner_args)
            .context("the runner arguments have unbalanced quotes")?;
        c.args(runner_args).arg(xcl);
        c.env("WINEPREFIX", paths::wine_prefix(cfg));
        c
    };
    if let Some(root) = cfg
        .xcl_data_root
        .as_ref()
        .filter(|p| !p.as_os_str().is_empty())
    {
        cmd.env("XCL_DATA_ROOT", paths::to_xcl_path(root));
    }
    if let Some(dir) = xcl.parent() {
        cmd.current_dir(dir);
    }
    hide_console(&mut cmd);
    Ok(cmd)
}

#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_cmd: &mut Command) {}

/// Sends the command's output to `log` (truncated) and starts it.
fn spawn_logged(mut cmd: Command, log: &Path, append: bool) -> Result<Child> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let file = File::options()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(log)
        .with_context(|| format!("opening {}", log.display()))?;
    let err = file.try_clone()?;
    cmd.stdin(Stdio::null()).stdout(file).stderr(err);
    cmd.spawn()
        .with_context(|| format!("starting {:?}", cmd.get_program()))
}

/// Ends a process and everything it started (`xcl run` starts the game in a second process).
fn kill_tree(child: &mut Child, cfg: &AppConfig) {
    #[cfg(windows)]
    {
        let mut c = Command::new("taskkill");
        c.args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_console(&mut c);
        let _ = c.status();
    }
    let _ = child.kill();
    // Under Wine the game is a Windows process that killing the runner does not end:
    // stop the prefix's processes.
    if !cfg!(windows)
        && Path::new(cfg.runner.trim())
            .file_name()
            .is_some_and(|n| n.to_string_lossy().contains("wine"))
    {
        let _ = Command::new("wineserver")
            .arg("-k")
            .env("WINEPREFIX", paths::wine_prefix(cfg))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.wait();
}

/// A running game.
pub struct Running {
    pub game_id: String,
    pub name: String,
    child: Child,
    pub started: Instant,
    /// What XCL printed to its console.
    pub output: PathBuf,
}

/// How a process ended.
pub struct Finished {
    pub name: String,
    pub status: ExitStatus,
    pub output: PathBuf,
}

impl Running {
    /// Applies the game's settings (xcl.toml, environment) and starts `xcl run`.
    pub fn start(
        cfg: &AppConfig,
        game: &Game,
        settings: &LaunchSettings,
        logs: &Path,
    ) -> Result<Running> {
        let data_root =
            paths::xcl_data_root(cfg).context("cannot tell where XCL keeps its data")?;
        crate::xcl_toml::apply(&game.data_dir(&data_root).join("xcl.toml"), settings)?;
        let game_args = settings
            .split_game_args()
            .context("the game arguments have unbalanced quotes")?;

        let mut cmd = xcl_command(cfg)?;
        cmd.arg("run").arg(paths::to_xcl_path(&game.root));
        if !game_args.is_empty() {
            cmd.arg("--").args(game_args);
        }
        for (key, val) in settings.env_vars() {
            match val {
                Some(v) => cmd.env(key, v),
                None => cmd.env_remove(key),
            };
        }
        let output = logs.join(format!("{}.log", game.title));
        let child = spawn_logged(cmd, &output, false)?;
        Ok(Running {
            game_id: game.id.clone(),
            name: game.name.clone(),
            child,
            started: Instant::now(),
            output,
        })
    }

    /// `Some` once the game has ended.
    pub fn poll(&mut self) -> Option<Finished> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(Finished {
                name: self.name.clone(),
                status,
                output: self.output.clone(),
            }),
            _ => None,
        }
    }

    pub fn stop(&mut self, cfg: &AppConfig) {
        kill_tree(&mut self.child, cfg);
    }
}

/// Precompiling a game's shaders: `xcl shaders <pack> --precompile --only cs --pipelines` for
/// each shader pack of the game, one after another (as `precompile-shaders-xcl-*.ps1` does).
pub struct Precompile {
    pub game_id: String,
    pub name: String,
    queue: VecDeque<PathBuf>,
    current: Option<(PathBuf, Child)>,
    pub total: usize,
    pub output: PathBuf,
    pub used_only: bool,
}

/// Progress of a [`Precompile`] after a poll.
pub enum PrecompileState {
    Working { done: usize, pack: PathBuf },
    Done,
    Failed(String),
}

impl Precompile {
    /// Finds the game's shader packs (`shaders.rpf`, anywhere below its folder).
    pub fn find_packs(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![(root.to_path_buf(), 0u32)];
        while let Some((dir, depth)) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let Ok(t) = e.file_type() else { continue };
                if t.is_dir() && depth < 8 {
                    stack.push((e.path(), depth + 1));
                } else if t.is_file()
                    && e.file_name()
                        .to_string_lossy()
                        .eq_ignore_ascii_case("shaders.rpf")
                {
                    found.push(e.path());
                }
            }
        }
        found.sort();
        found
    }

    pub fn new(game: &Game, packs: Vec<PathBuf>, used_only: bool, logs: &Path) -> Precompile {
        Precompile {
            game_id: game.id.clone(),
            name: game.name.clone(),
            total: packs.len(),
            queue: packs.into(),
            current: None,
            output: logs.join(format!("{}.precompile.log", game.title)),
            used_only,
        }
    }

    /// Advances the job: starts the next pack when the current one is done.
    pub fn poll(&mut self, cfg: &AppConfig) -> PrecompileState {
        if let Some((pack, child)) = &mut self.current {
            match child.try_wait() {
                Ok(None) => {
                    return PrecompileState::Working {
                        done: self.total - self.queue.len() - 1,
                        pack: pack.clone(),
                    };
                }
                Ok(Some(status)) if status.success() => self.current = None,
                Ok(Some(status)) => {
                    return PrecompileState::Failed(format!(
                        "xcl ended with {status} on {}",
                        pack.display()
                    ));
                }
                Err(e) => return PrecompileState::Failed(e.to_string()),
            }
        }
        let Some(pack) = self.queue.pop_front() else {
            return PrecompileState::Done;
        };
        let first = self.total - self.queue.len() == 1;
        let started = xcl_command(cfg).and_then(|mut cmd| {
            cmd.arg("shaders").arg(paths::to_xcl_path(&pack)).args([
                "--precompile",
                "--only",
                "cs",
                "--pipelines",
            ]);
            if self.used_only {
                cmd.arg("--used-only");
            }
            spawn_logged(cmd, &self.output, !first)
        });
        match started {
            Ok(child) => {
                self.current = Some((pack.clone(), child));
                PrecompileState::Working {
                    done: self.total - self.queue.len() - 1,
                    pack,
                }
            }
            Err(e) => PrecompileState::Failed(format!("{e:#}")),
        }
    }

    pub fn stop(&mut self, cfg: &AppConfig) {
        self.queue.clear();
        if let Some((_, child)) = &mut self.current {
            kill_tree(child, cfg);
        }
        self.current = None;
    }
}

/// True when an `xcl.exe` is running anywhere on the system (also ones not started here).
pub fn xcl_is_running() -> bool {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let Ok(sys) = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    ) else {
        return false;
    };
    sys.processes().values().any(|p| {
        let name = p.name().to_string_lossy().to_ascii_lowercase();
        name == "xcl.exe" || name == "xcl"
    })
}
