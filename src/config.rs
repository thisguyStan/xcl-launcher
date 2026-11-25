//! The launcher's own configuration: application settings, library folders, manually added
//! games, and the launch settings (application defaults plus per-game overrides).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Smallest and largest tile edge in logical points.
pub const TILE_SIZE_RANGE: std::ops::RangeInclusive<f32> = 96.0..=320.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// `xcl.exe`. Empty until the user picks it (or one is found next to the launcher).
    pub xcl_path: Option<PathBuf>,
    /// Linux: the program that runs `xcl.exe` (`wine`, a Proton `proton` script, `umu-run` …).
    pub runner: String,
    /// Linux: extra arguments put between the runner and `xcl.exe` (shell-style quoting).
    pub runner_args: String,
    /// Linux: the Wine prefix (`WINEPREFIX`). Empty: `$WINEPREFIX`, else `~/.wine`.
    pub wine_prefix: Option<PathBuf>,
    /// XCL's data folder (`XCL_DATA_ROOT`). Empty: XCL's default (`%LOCALAPPDATA%\XCL`).
    pub xcl_data_root: Option<PathBuf>,
    /// Folders whose game folders are listed automatically.
    pub library_dirs: Vec<PathBuf>,
    /// Games added one by one (their package folder).
    pub games: Vec<PathBuf>,
    /// Launch settings for every game; each game can override them.
    pub defaults: LaunchSettings,
    /// Per-game overrides, keyed by [`crate::game::Game::id`].
    pub game_settings: BTreeMap<String, LaunchSettings>,
    /// Tile edge in logical points.
    pub tile_size: f32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            xcl_path: None,
            runner: "wine".into(),
            runner_args: String::new(),
            wine_prefix: None,
            xcl_data_root: None,
            library_dirs: Vec::new(),
            games: Vec::new(),
            defaults: LaunchSettings::default(),
            game_settings: BTreeMap::new(),
            tile_size: 180.0,
        }
    }
}

impl AppConfig {
    /// Loads the configuration; a missing file gives the defaults.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("reading {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Saves the configuration: written to a temporary file first, so a crash never leaves
    /// half a file behind.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// The settings a game is launched with: its overrides on top of the defaults.
    pub fn effective_settings(&self, game_id: &str) -> LaunchSettings {
        match self.game_settings.get(game_id) {
            Some(game) => game.over(&self.defaults),
            None => self.defaults.clone(),
        }
    }
}

/// Settings for running a game. Every field is optional: `None` leaves XCL's own default (in
/// the application defaults) or uses the application default (in a game's overrides).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LaunchSettings {
    // --- written into the title's xcl.toml ---
    pub window_width: Option<u32>,
    pub window_height: Option<u32>,
    pub window_pos: Option<WindowPos>,
    pub gamertag: Option<String>,
    pub keyboard_mouse: Option<bool>,
    pub mouse_sensitivity: Option<f32>,

    // --- passed as environment variables ---
    /// `false`: `XCL_NETWORK=0`, the title runs without a network.
    pub network: Option<bool>,
    pub skip_bink: Option<SkipBink>,
    /// `XCL_MUTE_TTS`.
    pub mute_tts: Option<bool>,
    pub frame_limit: Option<FrameLimit>,
    pub device_type: Option<DeviceType>,
    pub raytracing: Option<Raytracing>,
    /// `XCL_ADAPTER`: "auto", a number or part of the GPU's name.
    pub adapter: Option<String>,
    /// `false`: `XCL_PLAYSTATION_PAD=0`.
    pub playstation_pad: Option<bool>,
    /// Arguments for the game itself (shell-style quoting).
    pub game_args: Option<String>,
    /// Any other environment variables. A game's entries are added to the defaults' ones
    /// (and replace those of the same name).
    pub env: Vec<EnvVar>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

impl EnvVar {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        EnvVar {
            name: name.into(),
            value: value.into(),
        }
    }
}

impl LaunchSettings {
    /// `self` (overrides) on top of `base`.
    pub fn over(&self, base: &LaunchSettings) -> LaunchSettings {
        fn pick<T: Clone>(a: &Option<T>, b: &Option<T>) -> Option<T> {
            a.clone().or_else(|| b.clone())
        }
        let mut env: Vec<EnvVar> = base
            .env
            .iter()
            .filter(|b| !self.env.iter().any(|o| o.name.trim() == b.name.trim()))
            .cloned()
            .collect();
        env.extend(self.env.iter().cloned());
        LaunchSettings {
            window_width: pick(&self.window_width, &base.window_width),
            window_height: pick(&self.window_height, &base.window_height),
            window_pos: pick(&self.window_pos, &base.window_pos),
            gamertag: pick(&self.gamertag, &base.gamertag),
            keyboard_mouse: pick(&self.keyboard_mouse, &base.keyboard_mouse),
            mouse_sensitivity: pick(&self.mouse_sensitivity, &base.mouse_sensitivity),
            network: pick(&self.network, &base.network),
            skip_bink: pick(&self.skip_bink, &base.skip_bink),
            mute_tts: pick(&self.mute_tts, &base.mute_tts),
            frame_limit: pick(&self.frame_limit, &base.frame_limit),
            device_type: pick(&self.device_type, &base.device_type),
            raytracing: pick(&self.raytracing, &base.raytracing),
            adapter: pick(&self.adapter, &base.adapter),
            playstation_pad: pick(&self.playstation_pad, &base.playstation_pad),
            game_args: pick(&self.game_args, &base.game_args),
            env,
        }
    }

    /// True when nothing is set (a game without overrides).
    pub fn is_empty(&self) -> bool {
        *self == LaunchSettings::default()
    }

    /// Environment variables for `xcl run`: `Some(value)` sets one, `None` removes it from the
    /// inherited environment (so a setting switched on is not undone by the user's shell).
    pub fn env_vars(&self) -> Vec<(String, Option<String>)> {
        let mut out: Vec<(String, Option<String>)> = Vec::new();
        let mut put = |k: &str, v: Option<String>| out.push((k.to_owned(), v));
        if let Some(on) = self.network {
            put("XCL_NETWORK", (!on).then(|| "0".into()));
        }
        if let Some(b) = self.skip_bink {
            put("XCL_SKIP_BINK", Some(b.env_value().into()));
        }
        if let Some(on) = self.mute_tts {
            put("XCL_MUTE_TTS", on.then(|| "1".into()));
        }
        if let Some(f) = self.frame_limit {
            put("XCL_FRAME_INTERVAL_US", Some(f.env_value()));
        }
        if let Some(d) = self.device_type {
            put("XCL_DEVICE_TYPE", Some(d.xcl_name().into()));
        }
        if let Some(r) = self.raytracing {
            put("XCL_RAYTRACING", Some(r.env_value().into()));
        }
        if let Some(a) = self
            .adapter
            .as_ref()
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
        {
            put("XCL_ADAPTER", Some(a.into()));
        }
        if let Some(on) = self.playstation_pad {
            put("XCL_PLAYSTATION_PAD", (!on).then(|| "0".into()));
        }
        for var in &self.env {
            let name = var.name.trim();
            if !name.is_empty() {
                put(name, Some(var.value.clone()));
            }
        }
        out
    }

    /// The game's own arguments, split like a shell would. `None` when the quoting is broken.
    pub fn split_game_args(&self) -> Option<Vec<String>> {
        match self.game_args.as_deref().map(str::trim) {
            None | Some("") => Some(Vec::new()),
            Some(args) => shlex::split(args),
        }
    }
}

/// Where XCL puts its window (`window_pos` in xcl.toml).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowPos {
    /// Windows chooses.
    Auto,
    /// Centred on the primary monitor.
    Center,
    /// The window's top-left corner in screen coordinates.
    At { x: i32, y: i32 },
}

impl WindowPos {
    pub fn xcl_value(self) -> String {
        match self {
            WindowPos::Auto => "auto".into(),
            WindowPos::Center => "center".into(),
            WindowPos::At { x, y } => format!("{x},{y}"),
        }
    }
}

/// `XCL_SKIP_BINK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipBink {
    Off,
    /// Skips the movies that do not loop (logos and intros).
    Intros,
    /// Also skips looping movies.
    All,
}

impl SkipBink {
    pub const ALL: [SkipBink; 3] = [SkipBink::Off, SkipBink::Intros, SkipBink::All];

    fn env_value(self) -> &'static str {
        match self {
            SkipBink::Off => "0",
            SkipBink::Intros => "1",
            SkipBink::All => "all",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SkipBink::Off => "Play all movies",
            SkipBink::Intros => "Skip intro movies",
            SkipBink::All => "Skip all movies",
        }
    }
}

/// `XCL_FRAME_INTERVAL_US`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameLimit {
    Uncapped,
    /// An upper bound on the frame rate.
    Fps(u32),
}

impl FrameLimit {
    fn env_value(self) -> String {
        match self {
            FrameLimit::Uncapped => "uncapped".into(),
            // XCL takes microseconds and never goes below 1000 (1000 fps)
            FrameLimit::Fps(fps) => (1_000_000 / fps.clamp(1, 1000)).to_string(),
        }
    }
}

/// `XCL_DEVICE_TYPE`: the console the title sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceType {
    SeriesX,
    SeriesS,
}

impl DeviceType {
    pub const ALL: [DeviceType; 2] = [DeviceType::SeriesX, DeviceType::SeriesS];

    fn xcl_name(self) -> &'static str {
        match self {
            DeviceType::SeriesX => "anaconda",
            DeviceType::SeriesS => "lockhart",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DeviceType::SeriesX => "Xbox Series X",
            DeviceType::SeriesS => "Xbox Series S",
        }
    }
}

/// `XCL_RAYTRACING`: the ray-tracing tier the title is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Raytracing {
    Auto,
    Tier1_1,
    Tier1_0,
    Off,
}

impl Raytracing {
    pub const ALL: [Raytracing; 4] = [
        Raytracing::Auto,
        Raytracing::Tier1_1,
        Raytracing::Tier1_0,
        Raytracing::Off,
    ];

    fn env_value(self) -> &'static str {
        match self {
            Raytracing::Auto => "auto",
            Raytracing::Tier1_1 => "tier1_1",
            Raytracing::Tier1_0 => "tier1_0",
            Raytracing::Off => "off",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Raytracing::Auto => "GPU's own tier",
            Raytracing::Tier1_1 => "Tier 1.1",
            Raytracing::Tier1_0 => "Tier 1.0",
            Raytracing::Off => "Off",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_win_and_env_maps_merge() {
        let base = LaunchSettings {
            window_width: Some(1920),
            network: Some(true),
            env: vec![EnvVar::new("A", "1"), EnvVar::new("B", "1")],
            ..Default::default()
        };
        let game = LaunchSettings {
            network: Some(false),
            env: vec![EnvVar::new("B", "2")],
            ..Default::default()
        };
        let eff = game.over(&base);
        assert_eq!(eff.window_width, Some(1920));
        assert_eq!(eff.network, Some(false));
        assert_eq!(eff.env, vec![EnvVar::new("A", "1"), EnvVar::new("B", "2")]);
    }

    #[test]
    fn env_vars_follow_xcl_conventions() {
        let s = LaunchSettings {
            network: Some(false),
            mute_tts: Some(false),
            frame_limit: Some(FrameLimit::Fps(120)),
            device_type: Some(DeviceType::SeriesS),
            ..Default::default()
        };
        let env = s.env_vars();
        assert!(env.contains(&("XCL_NETWORK".into(), Some("0".into()))));
        assert!(env.contains(&("XCL_MUTE_TTS".into(), None)));
        assert!(env.contains(&("XCL_FRAME_INTERVAL_US".into(), Some("8333".into()))));
        assert!(env.contains(&("XCL_DEVICE_TYPE".into(), Some("lockhart".into()))));
    }

    #[test]
    fn config_roundtrips_through_toml() {
        let mut c = AppConfig::default();
        c.defaults.window_pos = Some(WindowPos::At { x: 10, y: -20 });
        c.game_settings.insert(
            "D:\\Games\\X".into(),
            LaunchSettings {
                network: Some(false),
                ..Default::default()
            },
        );
        let text = toml::to_string_pretty(&c).unwrap();
        let back: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn game_args_are_split_like_a_shell() {
        let s = LaunchSettings {
            game_args: Some(r#"-a "b c""#.into()),
            ..Default::default()
        };
        assert_eq!(
            s.split_game_args(),
            Some(vec!["-a".to_string(), "b c".to_string()])
        );
    }
}
