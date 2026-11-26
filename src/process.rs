//! Starting `xcl.exe` without a console window and keeping track of what it is doing.
//!
//! XCL's own log (`<data>/<title>/logs/xcl.log`) is written by XCL as always; what XCL prints
//! to its console goes to a file in the launcher's log folder instead of a window.

use std::collections::VecDeque;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime};

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
    /// The shader and pipeline work XCL logs while the game runs.
    pub shaders: RunShaders,
    /// XCL's own log of this run, read as it grows.
    xcl_log: PathBuf,
    launched: SystemTime,
    tail: LogTail,
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
        let xcl_log = paths::child_ci(
            &paths::child_ci(&game.data_dir(&data_root), "logs"),
            "xcl.log",
        );
        let launched = SystemTime::now();
        let child = spawn_logged(cmd, &output, false)?;
        Ok(Running {
            game_id: game.id.clone(),
            name: game.name.clone(),
            child,
            started: Instant::now(),
            output,
            shaders: RunShaders::default(),
            xcl_log,
            launched,
            tail: LogTail::default(),
        })
    }

    /// Takes in what XCL logged since the last poll; `Some` once the game has ended.
    pub fn poll(&mut self) -> Option<Finished> {
        // XCL moves the previous run's log to xcl.prev.log when it starts: until then the file
        // is the old one, which must not be read as this run's
        let current = std::fs::metadata(&self.xcl_log)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= self.launched);
        if current {
            for line in self.tail.read_new(&self.xcl_log) {
                self.shaders.parse_line(&line);
            }
        }
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
/// The compute pipelines are built on the GPU while their shaders are processed, so one counter
/// covers both.
pub struct Precompile {
    pub game_id: String,
    pub name: String,
    queue: VecDeque<PathBuf>,
    current: Option<(PathBuf, Child)>,
    pub output: PathBuf,
    pub used_only: bool,
    pub progress: PrecompileProgress,
    tail: LogTail,
}

/// Progress of a [`Precompile`] after a poll.
pub enum PrecompileState {
    Working,
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
            progress: PrecompileProgress::new(packs.len()),
            queue: packs.into(),
            current: None,
            output: logs.join(format!("{}.precompile.log", game.title)),
            used_only,
            tail: LogTail::default(),
        }
    }

    /// Advances the job: reads XCL's progress from its output and starts the next pack when
    /// the current one is done.
    pub fn poll(&mut self, cfg: &AppConfig) -> PrecompileState {
        if let Some((pack, child)) = &mut self.current {
            let exited = child.try_wait();
            // read after try_wait: a pack that has just ended has written all of its output
            for line in self.tail.read_new(&self.output) {
                self.progress.parse_line(&line);
            }
            match exited {
                Ok(None) => return PrecompileState::Working,
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
        let first = self.progress.pack == 0;
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
                if first {
                    // the log was just truncated
                    self.tail = LogTail::default();
                }
                self.progress.start_pack(&pack);
                self.current = Some((pack, child));
                PrecompileState::Working
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

/// The shader and pipeline work of a running game, as XCL logs it in `xcl.log`:
///
/// - `the pipelines of N shader sets an earlier run used are built ahead of their first use`
///   once at startup, then `K pipelines built ahead of their first use` at K = 1, 2, 4, 8 …
///   (XCL logs powers of two only, so K is a lower bound).
/// - `driver compile start #I` / `driver compile finish #I` around each large pipeline
///   (smaller ones are not logged unless `XCL_PSO_TRACE=1`).
/// - `compute PSO with the translated console CS (…)` for each compute pipeline made.
///
/// Before that, XCL prepares the shaders in the game window before the title starts
/// ([`Preparation`]).
#[derive(Debug, Default, Clone)]
pub struct RunShaders {
    /// The shader preparation before the title starts, once XCL has logged it.
    pub preparation: Option<Preparation>,
    /// Pipelines an earlier run used, which XCL builds ahead at startup.
    pub warmup_total: Option<usize>,
    /// At least this many of them are built.
    pub warmup_built: usize,
    /// Pipelines built so far in this run (compute ones, and graphics ones large enough to be logged).
    pub built: usize,
    /// Large pipelines in the driver's compiler right now.
    compiling: std::collections::HashSet<u64>,
}

impl RunShaders {
    fn parse_line(&mut self, line: &str) {
        if let Some(i) = line.find("Shader preparation") {
            self.preparation
                .get_or_insert_with(Preparation::default)
                .parse(line[i..].trim_end(), log_seconds(line));
            return;
        }
        let Some(msg) = line.split_once("D3D12.X: ").map(|(_, m)| m.trim()) else {
            return;
        };
        // the title's own work begins once preparation has ended
        if let Some(p) = &mut self.preparation {
            p.end.get_or_insert(PreparationEnd::Done);
        }
        if let Some(rest) = msg.strip_prefix("the pipelines of ") {
            if rest.contains("built ahead of their first use") {
                self.warmup_total = rest.split_whitespace().next().and_then(|n| n.parse().ok());
            }
        } else if let Some(n) = msg
            .strip_suffix(" pipelines built ahead of their first use")
            .and_then(|n| n.parse::<usize>().ok())
        {
            self.warmup_built = self.warmup_built.max(n);
        } else if let Some(rest) = msg.strip_prefix("driver compile start #") {
            if let Some(id) = compile_id(rest) {
                self.compiling.insert(id);
            }
        } else if let Some(rest) = msg.strip_prefix("driver compile finish #") {
            if let Some(id) = compile_id(rest) {
                self.compiling.remove(&id);
            }
            // compute pipelines are counted by their own line below
            if rest.contains("graphics PSO") {
                self.built += 1;
            }
        } else if msg.starts_with("compute PSO with the translated console CS") {
            self.built += 1;
        }
    }

    /// Large pipelines the driver is compiling right now.
    pub fn compiling(&self) -> usize {
        self.compiling.len()
    }

    /// The shader preparation, while it is still going.
    pub fn preparing(&self) -> Option<&Preparation> {
        self.preparation.as_ref().filter(|p| p.end.is_none())
    }

    /// The share of the startup warm-up known to be done (a lower bound), while there is one.
    pub fn warmup_fraction(&self) -> Option<f32> {
        let total = self.warmup_total.filter(|&t| t > 0)?;
        Some((self.warmup_built as f32 / total as f32).min(1.0))
    }
}

/// The step of XCL's shader preparation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PreparationStep {
    /// Translating the console shaders (`records translated`).
    #[default]
    Translating,
    /// Building compute pipelines in worker processes (`records checked for pipelines`).
    Compute,
    /// Building the graphics pipelines earlier runs used (`graphics pipelines built`).
    Graphics,
}

/// How XCL's shader preparation ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparationEnd {
    Done,
    /// `XCL_PREPARE=0`
    LeftOut,
    Failed(String),
}

/// XCL's shader preparation before the title starts (XCL's `nvidia-dev` branch), from the
/// `Shader preparation: …` lines in `xcl.log`. XCL logs the count every 5 seconds:
/// `D of T records translated after S s`, `D of T records checked for pipelines by N workers
/// after S s`, then `D of T graphics pipelines built by N workers after S s`.
#[derive(Debug, Default, Clone)]
pub struct Preparation {
    pub step: PreparationStep,
    pub done: usize,
    pub total: usize,
    /// Worker processes of the current step (0 while translating).
    pub workers: usize,
    /// Seconds since preparation began, as XCL logged them.
    pub seconds: f64,
    /// When the count last moved, in the same seconds.
    moved_at: f64,
    /// `only the N shaders earlier runs used`.
    pub only_used: Option<usize>,
    /// How long the compute pipelines took, once done.
    pub compute_seconds: Option<u64>,
    /// `graphics pipelines done in …: N made, M failed; …`, once done.
    pub graphics_summary: Option<String>,
    pub end: Option<PreparationEnd>,
}

impl Preparation {
    /// `msg` starts at "Shader preparation"; `at` is the line's time stamp in the log.
    fn parse(&mut self, msg: &str, at: Option<f64>) {
        if let Some(at) = at {
            self.seconds = self.seconds.max(at);
        }
        if msg.contains("Shader preparation cancelled")
            || msg.contains("Shader preparation stopped")
        {
            self.end = Some(PreparationEnd::Failed(msg.to_string()));
            return;
        }
        if msg.starts_with("Shader preparation left out") {
            self.end = Some(PreparationEnd::LeftOut);
            return;
        }
        let Some(rest) = msg.strip_prefix("Shader preparation: ") else {
            return;
        };
        if let Some(n) = rest.strip_prefix("only the ") {
            self.only_used = leading_number(n);
        } else if let Some(w) = rest.strip_prefix("compute pipelines in ") {
            self.set_step(PreparationStep::Compute, leading_number(w));
        } else if let Some(s) = rest.strip_prefix("compute pipelines done in ") {
            self.compute_seconds = leading_number(s).map(|n| n as u64);
        } else if let Some(s) = rest
            .strip_prefix("the ")
            .filter(|s| s.contains("graphics pipelines earlier runs used"))
        {
            let workers = s.split_once(", in ").and_then(|(_, w)| leading_number(w));
            self.set_step(PreparationStep::Graphics, workers);
            self.total = leading_number(s).unwrap_or(self.total);
        } else if let Some(s) = rest.strip_prefix("graphics pipelines done in ") {
            self.graphics_summary =
                Some(s.split_once("; driver").map_or(s, |(a, _)| a).to_string());
            self.end = Some(PreparationEnd::Done);
        } else if rest.starts_with("graphics pipelines left out")
            || rest.starts_with("no graphics pipelines")
        {
            self.end = Some(PreparationEnd::Done);
        } else if let Some((counts, tail)) = rest.split_once(" after ") {
            // "D of T <what>" + "S s"
            let mut words = counts.split_whitespace();
            let (Some(done), Some("of"), Some(total)) = (
                words.next().and_then(|n| n.parse().ok()),
                words.next(),
                words.next().and_then(|n| n.parse().ok()),
            ) else {
                return;
            };
            let what: Vec<&str> = words.collect();
            let workers = what
                .iter()
                .position(|&w| w == "by")
                .and_then(|i| what.get(i + 1))
                .and_then(|n| n.parse().ok());
            let step = if what.first() == Some(&"graphics") {
                PreparationStep::Graphics
            } else if what.contains(&"checked") {
                PreparationStep::Compute
            } else {
                PreparationStep::Translating
            };
            self.set_step(step, workers);
            if done != self.done || total != self.total {
                self.moved_at = self.seconds;
            }
            self.done = done;
            self.total = total;
            if let Some(s) = leading_number(tail) {
                self.seconds = self.seconds.max(s as f64);
            }
        }
    }

    fn set_step(&mut self, step: PreparationStep, workers: Option<usize>) {
        if step != self.step {
            self.step = step;
            self.done = 0;
            self.moved_at = self.seconds;
        }
        if let Some(w) = workers {
            self.workers = w;
        }
    }

    /// The share of the current step that is done.
    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.done as f32 / self.total as f32).min(1.0))
    }

    /// Seconds the count has stood still: XCL is on its biggest pipelines.
    pub fn still_for(&self) -> f64 {
        self.seconds - self.moved_at
    }
}

/// `[  397.288][45356][INFO ] …` → 397.288.
fn log_seconds(line: &str) -> Option<f64> {
    line.strip_prefix('[')?
        .split_once(']')?
        .0
        .trim()
        .parse()
        .ok()
}

/// `1302 shaders …` → 1302.
fn leading_number(s: &str) -> Option<usize> {
    s.trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// `1172: compute PSO, …` → 1172.
fn compile_id(rest: &str) -> Option<u64> {
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// How long the shader count may stand still before the progress says XCL is on large shaders.
const STALL: Duration = Duration::from_secs(30);

/// How far a [`Precompile`] is, read from what `xcl shaders --precompile` prints:
/// `precompiling N shaders into …` when a pack starts, `  D/N (T s)` every 200 shaders, and a
/// summary (`compiled …`, `pipelines made on the GPU …`) when it ends.
#[derive(Debug, Clone)]
pub struct PrecompileProgress {
    /// The pack being worked on, counting from 1 (0 before the first one starts).
    pub pack: usize,
    pub packs: usize,
    pub pack_name: String,
    /// Shaders of the current pack done so far.
    pub done: usize,
    /// Shaders in the current pack, once XCL has said.
    pub total: Option<usize>,
    pub started: Instant,
    /// XCL's summary lines of the finished packs.
    pub summary: Vec<String>,
    /// Pipelines built on the GPU in the current pack (`  pipelines: N made, M refused`, every 25).
    pub pipelines_made: usize,
    pub pipelines_refused: usize,
    /// When the shader count last moved: XCL reports it every 200 shaders, and the biggest
    /// programs can take minutes each.
    pub last_count: Instant,
}

impl PrecompileProgress {
    pub fn new(packs: usize) -> Self {
        PrecompileProgress {
            pack: 0,
            packs,
            pack_name: String::new(),
            done: 0,
            total: None,
            started: Instant::now(),
            summary: Vec::new(),
            pipelines_made: 0,
            pipelines_refused: 0,
            last_count: Instant::now(),
        }
    }

    fn start_pack(&mut self, pack: &Path) {
        self.pack += 1;
        self.pack_name = pack
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.done = 0;
        self.total = None;
        self.pipelines_made = 0;
        self.pipelines_refused = 0;
        self.last_count = Instant::now();
    }

    fn parse_line(&mut self, line: &str) {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("precompiling ") {
            if let Some(n) = rest.split_whitespace().next().and_then(|n| n.parse().ok()) {
                self.total = Some(n);
                self.done = 0;
            }
        } else if line.starts_with("compiled ") {
            if let Some(n) = self.total {
                self.done = n;
            }
            self.summary.push(line.to_owned());
        } else if let Some(rest) = line.strip_prefix("pipelines: ") {
            // "150 made, 0 refused"
            let mut numbers = rest
                .split(|c: char| !c.is_ascii_digit())
                .filter_map(|n| n.parse().ok());
            if let (Some(made), Some(refused)) = (numbers.next(), numbers.next()) {
                self.pipelines_made = made;
                self.pipelines_refused = refused;
            }
        } else if line.starts_with("pipelines made on the GPU") {
            self.summary.push(line.to_owned());
        } else if let Some((done, total)) = line
            .split_whitespace()
            .next()
            .and_then(|t| t.split_once('/'))
            .and_then(|(d, n)| Some((d.parse::<usize>().ok()?, n.parse::<usize>().ok()?)))
        {
            if done != self.done {
                self.last_count = Instant::now();
            }
            self.done = done;
            self.total = Some(total);
        }
    }

    /// The share of the whole job that is done (0–1), once it can be told.
    pub fn fraction(&self) -> Option<f32> {
        if self.packs == 0 || self.pack == 0 {
            return None;
        }
        let finished_packs = (self.pack - 1) as f32;
        let in_pack = match self.total {
            Some(0) => 1.0,
            Some(n) => (self.done as f32 / n as f32).min(1.0),
            None if self.pack > 1 => 0.0,
            None => return None,
        };
        Some((finished_packs + in_pack) / self.packs as f32)
    }

    /// True when the shader count has not moved for a while: XCL is on its biggest programs.
    pub fn on_large_shaders(&self) -> bool {
        self.total.is_some() && self.last_count.elapsed() > STALL
    }

    /// An estimate of the time left, from the pace so far. `None` until there is enough to go by,
    /// and while XCL is on large shaders (the pace says nothing then).
    pub fn remaining(&self) -> Option<Duration> {
        let f = self.fraction()?;
        let elapsed = self.started.elapsed();
        if !(0.01..1.0).contains(&f) || elapsed < Duration::from_secs(5) || self.on_large_shaders()
        {
            return None;
        }
        Some(elapsed.mul_f32((1.0 - f) / f))
    }
}

/// Reads the lines a growing log file gets, from where the last read stopped.
#[derive(Default)]
struct LogTail {
    pos: u64,
    partial: String,
}

impl LogTail {
    fn read_new(&mut self, path: &Path) -> Vec<String> {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = File::open(path) else {
            return Vec::new();
        };
        let mut buf = Vec::new();
        if f.seek(SeekFrom::Start(self.pos)).is_err() || f.read_to_end(&mut buf).is_err() {
            return Vec::new();
        }
        self.pos += buf.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&buf));
        // only whole lines: the rest waits for the next read
        let Some(end) = self.partial.rfind(['\n', '\r']) else {
            return Vec::new();
        };
        let complete: String = self.partial.drain(..=end).collect();
        complete
            .split(['\n', '\r'])
            .filter(|l| !l.trim().is_empty())
            .map(str::to_owned)
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What `xcl shaders --precompile --only cs --pipelines` prints for one pack.
    const OUTPUT: &[&str] = &[
        r"precompiling 26698 shaders into C:\Users\x\AppData\Local\xcl\dxil (30 compile threads; 4 memory windows)",
        "  200/26698 (3 s)",
        "  13400/26698 (95 s)",
        "compiled 120, already cached 3000, errors 0, skipped (GS – paired at pipeline creation) 0, time 190.0 s",
        "pipelines made on the GPU 118, refused 2",
        "pipelines loaded from the driver cache 3000",
    ];

    #[test]
    fn follows_xcl_progress_lines() {
        let mut p = PrecompileProgress::new(2);
        assert_eq!(p.fraction(), None);
        p.start_pack(Path::new("a/shaders.rpf"));
        assert_eq!(p.fraction(), None);
        p.parse_line(OUTPUT[0]);
        assert_eq!((p.done, p.total), (0, Some(26698)));
        p.parse_line(OUTPUT[2]);
        assert_eq!(p.done, 13400);
        p.parse_line("  pipelines: 175 made, 2 refused");
        assert_eq!((p.pipelines_made, p.pipelines_refused), (175, 2));
        assert!((p.fraction().unwrap() - 0.25).abs() < 0.01);
        for l in &OUTPUT[3..] {
            p.parse_line(l);
        }
        assert_eq!(p.done, 26698);
        assert_eq!(p.summary.len(), 2);
        assert!((p.fraction().unwrap() - 0.5).abs() < 0.001);
        p.start_pack(Path::new("b/shaders.rpf"));
        assert!((p.fraction().unwrap() - 0.5).abs() < 0.001);
    }

    #[test]
    fn follows_shader_work_in_xcl_log() {
        let log = [
            "[    6.525][27384][INFO ] D3D12.X: 1 driver pipelines stand in for the pipelines not built yet",
            "[    6.525][27384][INFO ] D3D12.X: the pipelines of 787 shader sets an earlier run used are built ahead of their first use",
            "[    6.544][51116][INFO ] D3D12.X: 2 pipelines built ahead of their first use",
            "[    8.743][37940][INFO ] D3D12.X: 512 pipelines built ahead of their first use",
            "[    9.975][38844][INFO ] D3D12.X: compute PSO with the translated console CS (473776 B DXIL, 114 ms, the driver's blob of an earlier build)",
            "[  921.182][48028][INFO ] D3D12.X: driver compile start #1164: graphics PSO, 305900 B DXIL, record 0123456789abcdef, driver blob absent",
            "[ 1007.055][48652][INFO ] D3D12.X: driver compile start #1172: compute PSO, 349204 B DXIL, record fedcba9876543210, driver blob absent",
            "[  927.674][48028][INFO ] D3D12.X: driver compile finish #1164: graphics PSO, 305900 B DXIL, 6492 ms, huge, ungated here",
        ];
        let mut s = RunShaders::default();
        for l in log {
            s.parse_line(l);
        }
        assert_eq!(s.warmup_total, Some(787));
        assert_eq!(s.warmup_built, 512);
        assert!((s.warmup_fraction().unwrap() - 512.0 / 787.0).abs() < 1e-6);
        assert_eq!(s.built, 2);
        assert_eq!(s.compiling(), 1);
        s.parse_line("[ 1009.000][48652][INFO ] D3D12.X: driver compile finish #1172: compute PSO, 349204 B DXIL, 900 ms, gated (waited 3 ms)");
        assert_eq!(s.compiling(), 0);
        assert_eq!(
            s.built, 2,
            "compute pipelines are counted once, by their own line"
        );
    }

    #[test]
    fn follows_shader_preparation() {
        let log = [
            "[    0.005][ 5092][INFO ] Shader preparation: only the 1302 shaders earlier runs used (XCL_PREPARE_ALL=1: every record)",
            "[    0.051][ 5092][INFO ] Shader preparation window 0x3d0ac8",
            "[    5.092][42396][INFO ] Shader preparation: 23843 of 26503 records translated after 5 s",
        ];
        let mut s = RunShaders::default();
        for l in log {
            s.parse_line(l);
        }
        let p = s.preparing().unwrap();
        assert_eq!(p.only_used, Some(1302));
        assert_eq!(
            (p.step, p.done, p.total),
            (PreparationStep::Translating, 23843, 26503)
        );
        for l in [
            "[    5.477][45356][INFO ] Shader preparation: compute pipelines in 20 worker processes, those an earlier run used",
            "[   45.302][42396][INFO ] Shader preparation: 25806 of 26503 records checked for pipelines by 20 workers after 45 s",
            "[   50.309][42396][INFO ] Shader preparation: 25806 of 26503 records checked for pipelines by 20 workers after 50 s",
            "[  186.440][42396][INFO ] Shader preparation: 25806 of 26503 records checked for pipelines by 20 workers after 186 s",
        ] {
            s.parse_line(l);
        }
        let p = s.preparing().unwrap();
        assert_eq!(
            (p.step, p.done, p.workers),
            (PreparationStep::Compute, 25806, 20)
        );
        assert!((p.still_for() - 141.0).abs() < 1.0, "{}", p.still_for());
        for l in [
            "[  397.288][45356][INFO ] Shader preparation: compute pipelines done in 392 s (0 of 20 workers failed)",
            "[  397.355][45356][INFO ] Shader preparation: 2 of 147 graphics pipelines left out (their shaders are not in the title's shader files)",
            "[  397.394][45356][INFO ] Shader preparation: the 145 graphics pipelines earlier runs used, in 20 worker processes",
            "[  403.032][42396][INFO ] Shader preparation: 64 of 145 graphics pipelines built by 20 workers after 402 s",
        ] {
            s.parse_line(l);
        }
        let p = s.preparing().unwrap();
        assert_eq!(
            (p.step, p.done, p.total),
            (PreparationStep::Graphics, 64, 145)
        );
        assert_eq!(p.compute_seconds, Some(392));
        s.parse_line("[  440.292][45356][INFO ] Shader preparation: graphics pipelines done in 43 s: 145 made, 0 failed; driver blobs: 56 from disk, 89 kept (0 of 20 workers failed)");
        assert!(s.preparing().is_none());
        let p = s.preparation.as_ref().unwrap();
        assert_eq!(p.end, Some(PreparationEnd::Done));
        assert_eq!(
            p.graphics_summary.as_deref(),
            Some("43 s: 145 made, 0 failed")
        );

        let mut s = RunShaders::default();
        s.parse_line("[    0.010][ 1][INFO ] Shader preparation left out (XCL_PREPARE=0): the title builds what it uses as it goes");
        assert_eq!(s.preparation.unwrap().end, Some(PreparationEnd::LeftOut));
    }

    #[test]
    fn reads_only_complete_new_lines() {
        let path =
            std::env::temp_dir().join(format!("xcl-launcher-tail-{}.log", std::process::id()));
        std::fs::write(&path, "precompiling 10 shaders\n  2/10 (1 s)\n  4/1").unwrap();
        let mut tail = LogTail::default();
        assert_eq!(
            tail.read_new(&path),
            vec!["precompiling 10 shaders", "  2/10 (1 s)"]
        );
        let mut f = File::options().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut f, b"0 (2 s)\r\n").unwrap();
        drop(f);
        assert_eq!(tail.read_new(&path), vec!["  4/10 (2 s)"]);
        std::fs::remove_file(&path).unwrap();
    }
}
