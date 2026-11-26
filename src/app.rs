//! The launcher window: the game grid, the selected game's panel, settings and cache cleaning.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, RichText, ScrollArea, Sense,
    Stroke, StrokeKind, TextEdit, Ui, Vec2,
};

use crate::clean::{self, Group, Target};
use crate::config::{AppConfig, TILE_SIZE_RANGE};
use crate::game::{self, Game, Source};
use crate::paths::{self, LauncherDirs};
use crate::process::{Precompile, PrecompileState, Running};
use crate::settings_ui::{self, Level};
use crate::thumbs::Thumbnails;

const GAP: f32 = 12.0;
/// Height of the name under a tile.
const LABEL_H: f32 = 24.0;
const SAVE_DELAY: Duration = Duration::from_millis(600);
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_NOTICES: usize = 4;

#[derive(PartialEq)]
enum View {
    Library,
    Settings,
}

/// Something the user asked for, carried out after the frame's UI is built.
enum Action {
    Select(Option<String>),
    Launch(String),
    Stop(String),
    Precompile { id: String, used_only: bool },
    StopPrecompile(String),
    Open(PathBuf),
    AddGame,
    AddLibrary,
    RemoveGame(String),
    Rescan,
    OpenClean,
}

struct Notice {
    text: String,
    error: bool,
    /// A file or folder the notice is about ("Show output").
    path: Option<PathBuf>,
}

enum CleanState {
    Scanning(Receiver<Vec<Target>>),
    Choose(Vec<Target>),
    Confirm(Vec<Target>),
    Cleaning(Receiver<(u64, Vec<String>)>),
    Done(String),
}

pub struct App {
    dirs: LauncherDirs,
    config: AppConfig,
    /// What is on disk; the config is saved shortly after it differs.
    saved: AppConfig,
    save_due: Option<Instant>,
    games: Vec<Game>,
    scan: Option<Receiver<Vec<Game>>>,
    thumbs: Thumbnails,
    selected: Option<String>,
    filter: String,
    view: View,
    running: Vec<Running>,
    precompiles: Vec<Precompile>,
    notices: Vec<Notice>,
    clean: Option<CleanState>,
    /// Asked before precompiling: only the pipelines earlier runs used.
    precompile_used_only: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let dirs = LauncherDirs::new();
        let mut notices = Vec::new();
        let mut config = AppConfig::load(&dirs.config_file).unwrap_or_else(|e| {
            notices.push(Notice {
                text: format!("Settings could not be read, using defaults: {e:#}"),
                error: true,
                path: None,
            });
            AppConfig::default()
        });
        let saved = config.clone();
        if config.xcl_path.is_none() {
            config.xcl_path = paths::guess_xcl();
        }
        let mut app = App {
            dirs,
            config,
            saved,
            save_due: None,
            games: Vec::new(),
            scan: None,
            thumbs: Thumbnails::new(),
            selected: None,
            filter: String::new(),
            view: View::Library,
            running: Vec::new(),
            precompiles: Vec::new(),
            notices,
            clean: None,
            precompile_used_only: false,
        };
        if app.config.xcl_path.is_none() {
            app.notice(
                "Set the path to xcl.exe in Settings to start games.",
                false,
                None,
            );
        }
        app.rescan(&cc.egui_ctx);
        app
    }

    fn notice(&mut self, text: impl Into<String>, error: bool, path: Option<PathBuf>) {
        self.notices.push(Notice {
            text: text.into(),
            error,
            path,
        });
        if self.notices.len() > MAX_NOTICES {
            self.notices.remove(0);
        }
    }

    fn rescan(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let (libs, manual, ctx) = (
            self.config.library_dirs.clone(),
            self.config.games.clone(),
            ctx.clone(),
        );
        std::thread::spawn(move || {
            let _ = tx.send(game::scan(&libs, &manual));
            ctx.request_repaint();
        });
        self.scan = Some(rx);
    }

    fn game(&self, id: &str) -> Option<&Game> {
        self.games.iter().find(|g| g.id == id)
    }

    fn is_running(&self, id: &str) -> bool {
        self.running.iter().any(|r| r.game_id == id)
    }

    fn data_dir(&self, game: &Game) -> Option<PathBuf> {
        paths::xcl_data_root(&self.config).map(|root| game.data_dir(&root))
    }

    // ------------------------------------------------------------------ background work

    /// Takes in finished scans, ended games and precompile progress; saves the configuration.
    fn update_background(&mut self, ctx: &egui::Context) {
        self.thumbs.receive(ctx);
        if let Some(games) = self.scan.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.scan = None;
            self.thumbs.clear();
            self.games = games;
        }

        let mut ended = Vec::new();
        self.running.retain_mut(|r| match r.poll() {
            Some(f) => {
                ended.push(f);
                false
            }
            None => true,
        });
        for f in ended {
            if f.status.success() {
                self.notice(format!("{} ended.", f.name), false, None);
            } else {
                let last = error_line(&f.output)
                    .map(|l| format!(": {l}"))
                    .unwrap_or_default();
                let code = f.status.code().map_or_else(
                    || f.status.to_string(),
                    // Windows status codes (0xC0000005 …) read better in hex
                    |c| {
                        if (0..=0xFFFF).contains(&c) {
                            format!("code {c}")
                        } else {
                            format!("code 0x{c:08X}")
                        }
                    },
                );
                self.notice(
                    format!("{} ended with {code}{last}", f.name),
                    true,
                    Some(f.output),
                );
            }
        }

        let mut messages = Vec::new();
        let cfg = &self.config;
        self.precompiles.retain_mut(|p| match p.poll(cfg) {
            PrecompileState::Working => true,
            PrecompileState::Done => {
                let took = format_duration(p.progress.started.elapsed());
                let summary = p.progress.summary.join("; ");
                messages.push((
                    format!("Shaders of {} precompiled in {took}. {summary}", p.name),
                    false,
                    Some(p.output.clone()),
                ));
                false
            }
            PrecompileState::Failed(e) => {
                messages.push((
                    format!("Precompiling {} failed: {e}", p.name),
                    true,
                    Some(p.output.clone()),
                ));
                false
            }
        });
        for (text, error, path) in messages {
            self.notice(text, error, path);
        }

        if !self.running.is_empty() || !self.precompiles.is_empty() {
            ctx.request_repaint_after(POLL_INTERVAL);
        }

        if self.config != self.saved {
            let due = *self
                .save_due
                .get_or_insert_with(|| Instant::now() + SAVE_DELAY);
            if Instant::now() >= due {
                self.save();
            } else {
                ctx.request_repaint_after(due - Instant::now());
            }
        }
    }

    fn save(&mut self) {
        self.save_due = None;
        match self.config.save(&self.dirs.config_file) {
            Ok(()) => self.saved = self.config.clone(),
            Err(e) => {
                // keep the in-memory state; trying again on every change would spam
                self.saved = self.config.clone();
                self.notice(format!("Saving the settings failed: {e:#}"), true, None);
            }
        }
    }

    // ------------------------------------------------------------------ actions

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Select(id) => self.selected = id,
            Action::Launch(id) => self.launch(&id),
            Action::Stop(id) => {
                let cfg = &self.config;
                if let Some(r) = self.running.iter_mut().find(|r| r.game_id == id) {
                    r.stop(cfg);
                }
            }
            Action::Precompile { id, used_only } => self.precompile(&id, used_only),
            Action::StopPrecompile(id) => {
                let cfg = &self.config;
                self.precompiles.retain_mut(|p| {
                    if p.game_id == id {
                        p.stop(cfg);
                    }
                    p.game_id != id
                });
            }
            Action::Open(path) => self.open(&path),
            Action::AddGame => {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Choose a game folder (the one with MicrosoftGame.config)")
                    .pick_folder()
                {
                    let root = game::game_root_for(&dir);
                    if !game::is_game_dir(&root) {
                        self.notice(
                            format!(
                                "{} has no MicrosoftGame.config; added anyway.",
                                root.display()
                            ),
                            false,
                            None,
                        );
                    }
                    if !self.config.games.contains(&root) {
                        self.config.games.push(root.clone());
                    }
                    self.selected = Some(root.to_string_lossy().into_owned());
                    self.rescan(ctx);
                }
            }
            Action::AddLibrary => {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Choose a folder that holds game folders")
                    .pick_folder()
                {
                    if !self.config.library_dirs.contains(&dir) {
                        self.config.library_dirs.push(dir);
                    }
                    self.rescan(ctx);
                }
            }
            Action::RemoveGame(id) => {
                self.config.games.retain(|g| g.to_string_lossy() != id);
                self.config.game_settings.remove(&id);
                if self.selected.as_deref() == Some(id.as_str()) {
                    self.selected = None;
                }
                self.rescan(ctx);
            }
            Action::Rescan => self.rescan(ctx),
            Action::OpenClean => self.open_clean(ctx),
        }
    }

    fn launch(&mut self, id: &str) {
        if self.is_running(id) {
            return;
        }
        let Some(game) = self.game(id).cloned() else {
            return;
        };
        let settings = self.config.effective_settings(id);
        match Running::start(&self.config, &game, &settings, &self.dirs.logs) {
            Ok(r) => {
                self.notice(format!("Starting {}…", game.name), false, None);
                self.running.push(r);
            }
            Err(e) => self.notice(format!("Could not start {}: {e:#}", game.name), true, None),
        }
    }

    fn precompile(&mut self, id: &str, used_only: bool) {
        if self.precompiles.iter().any(|p| p.game_id == id) {
            return;
        }
        let Some(game) = self.game(id).cloned() else {
            return;
        };
        let packs = Precompile::find_packs(&game.root);
        if packs.is_empty() {
            self.notice(
                format!("{} has no shaders.rpf to precompile.", game.name),
                true,
                None,
            );
            return;
        }
        let job = Precompile::new(&game, packs, used_only, &self.dirs.logs);
        self.notice(
            format!(
                "Precompiling the shaders of {} – the first time takes a while.",
                game.name
            ),
            false,
            Some(job.output.clone()),
        );
        self.precompiles.push(job);
    }

    fn open(&mut self, path: &Path) {
        if !path.exists() {
            self.notice(
                format!(
                    "{} does not exist yet – it is made when the game runs.",
                    path.display()
                ),
                false,
                None,
            );
            return;
        }
        if let Err(e) = open::that_detached(path) {
            self.notice(
                format!("Could not open {}: {e}", path.display()),
                true,
                None,
            );
        }
    }

    fn open_clean(&mut self, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let (cfg, games, logs, ctx) = (
            self.config.clone(),
            self.games.clone(),
            self.dirs.logs.clone(),
            ctx.clone(),
        );
        std::thread::spawn(move || {
            let _ = tx.send(clean::find_targets(&cfg, &games, &logs));
            ctx.request_repaint();
        });
        self.clean = Some(CleanState::Scanning(rx));
    }

    // ------------------------------------------------------------------ UI

    fn top_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.heading(RichText::new("XCL Launcher").strong());
            ui.add_space(12.0);
            if self.view == View::Library {
                ui.add(
                    TextEdit::singleline(&mut self.filter)
                        .hint_text("Search games")
                        .desired_width(200.0),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let settings_label = if self.view == View::Settings {
                    "Back to games"
                } else {
                    "Settings"
                };
                if ui.button(settings_label).clicked() {
                    self.view = if self.view == View::Settings {
                        View::Library
                    } else {
                        View::Settings
                    };
                }
                if ui.button("Clean caches…").clicked() {
                    actions.push(Action::OpenClean);
                }
                ui.menu_button("Folders", |ui| self.folders_menu(ui, actions));
                if ui
                    .button("Refresh")
                    .on_hover_text("Look for games again")
                    .clicked()
                {
                    actions.push(Action::Rescan);
                }
                if ui.button("Add game…").clicked() {
                    actions.push(Action::AddGame);
                }
                if self.scan.is_some() {
                    ui.spinner();
                }
            });
        });
    }

    fn folders_menu(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let mut entry = |ui: &mut Ui, label: &str, path: Option<PathBuf>| {
            let enabled = path.is_some();
            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                if let Some(p) = path {
                    actions.push(Action::Open(p));
                }
                ui.close();
            }
        };
        entry(
            ui,
            "XCL data (all games)",
            paths::xcl_data_root(&self.config),
        );
        entry(ui, "Shader cache", paths::shader_cache(&self.config));
        entry(
            ui,
            "XCL program folder",
            self.config
                .xcl_path
                .as_ref()
                .and_then(|p| p.parent().map(Path::to_path_buf)),
        );
        entry(ui, "Launcher logs", Some(self.dirs.logs.clone()));
        entry(
            ui,
            "Launcher settings",
            self.dirs.config_file.parent().map(Path::to_path_buf),
        );
        if !cfg!(windows) {
            entry(ui, "Wine prefix", Some(paths::wine_prefix(&self.config)));
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let mut dismiss = None;
        let mut open = None;
        for (i, n) in self.notices.iter().enumerate().rev() {
            ui.horizontal(|ui| {
                let color = if n.error {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().text_color()
                };
                if ui.small_button("×").on_hover_text("Dismiss").clicked() {
                    dismiss = Some(i);
                }
                if let Some(p) = &n.path
                    && ui.small_button("Show output").clicked()
                {
                    open = Some(p.clone());
                }
                ui.add(egui::Label::new(RichText::new(&n.text).color(color)).truncate());
            });
        }
        for r in &self.running {
            ui.horizontal(|ui| {
                ui.label(format!("{} is running", r.name));
                run_shaders_bar(ui, &r.shaders, 260.0, true);
                ui.label(RichText::new(run_shaders_detail(&r.shaders)).weak());
            });
        }
        for p in &self.precompiles {
            ui.horizontal(|ui| {
                ui.label(format!("Precompiling {}", p.name));
                precompile_bar(ui, &p.progress, 260.0);
                ui.label(RichText::new(precompile_detail(&p.progress)).weak());
            });
        }
        if let Some(i) = dismiss {
            self.notices.remove(i);
        }
        if let Some(p) = open {
            self.open(&p);
        }
    }

    fn grid(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        if self.games.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.3);
                if self.scan.is_some() {
                    ui.spinner();
                    return;
                }
                ui.heading("No games yet");
                ui.label("Add a folder that holds your game folders, or add a single game.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let w = 260.0;
                    ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
                    if ui.button("Add library folder…").clicked() {
                        actions.push(Action::AddLibrary);
                    }
                    if ui.button("Add game…").clicked() {
                        actions.push(Action::AddGame);
                    }
                });
            });
            return;
        }
        let filter = self.filter.trim().to_lowercase();
        let visible: Vec<usize> = (0..self.games.len())
            .filter(|&i| filter.is_empty() || self.games[i].name.to_lowercase().contains(&filter))
            .collect();
        let tile = self
            .config
            .tile_size
            .clamp(*TILE_SIZE_RANGE.start(), *TILE_SIZE_RANGE.end());
        let columns = (((ui.available_width() - GAP) / (tile + GAP)).floor() as usize).max(1);
        let rows = visible.len().div_ceil(columns);
        let ctx = ui.ctx().clone();
        ScrollArea::vertical().auto_shrink(false).show_rows(
            ui,
            tile + LABEL_H + GAP,
            rows,
            |ui, range| {
                for row in range {
                    ui.add_space(GAP / 2.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = GAP;
                        ui.add_space(GAP / 2.0);
                        for &i in visible.iter().skip(row * columns).take(columns) {
                            self.tile(ui, &ctx, i, tile, actions);
                        }
                    });
                    ui.add_space(GAP / 2.0);
                }
            },
        );
    }

    fn tile(
        &mut self,
        ui: &mut Ui,
        ctx: &egui::Context,
        index: usize,
        size: f32,
        actions: &mut Vec<Action>,
    ) {
        let (outer, response) =
            ui.allocate_exact_size(Vec2::new(size, size + LABEL_H), Sense::click());
        let rect = Rect::from_min_size(outer.min, Vec2::splat(size));
        let game = &self.games[index];
        let id = game.id.clone();
        let selected = self.selected.as_deref() == Some(id.as_str());
        let run = self.running.iter().find(|r| r.game_id == id);
        let running = run.is_some();
        let preparing = run.is_some_and(|r| r.shaders.preparing().is_some());
        let compiling = run.is_some_and(|r| r.shaders.compiling() > 0);
        let radius = CornerRadius::same(8);

        if ui.is_rect_visible(rect) {
            let painter = ui.painter_at(outer.expand(3.0));
            let bg = game.background.map_or(Color32::from_gray(40), |[r, g, b]| {
                Color32::from_rgb(r, g, b)
            });
            painter.rect_filled(rect, radius, bg);
            let logo = game.logo.clone();
            let has_texture = match self.thumbs.get(ctx, &id, logo.as_ref()) {
                Some(tex) => {
                    egui::Image::new((tex.id(), rect.size()))
                        .corner_radius(radius)
                        .paint_at(ui, rect);
                    true
                }
                None => false,
            };
            let game = &self.games[index];
            if !has_texture {
                // no logo (yet): the name in the middle of the tile
                let galley = ui.painter().layout(
                    game.name.clone(),
                    FontId::proportional(16.0),
                    Color32::from_gray(235),
                    size - 24.0,
                );
                painter.galley(
                    rect.center() - galley.size() / 2.0,
                    galley,
                    Color32::from_gray(235),
                );
            }
            // the name from the game's metadata, on one line under the tile
            let name_color = if selected || response.hovered() {
                ui.visuals().strong_text_color()
            } else {
                ui.visuals().text_color()
            };
            let mut job = egui::text::LayoutJob::simple_singleline(
                game.name.clone(),
                FontId::proportional(14.0),
                name_color,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(size);
            let galley = ui.painter().layout_job(job);
            painter.galley(
                egui::pos2(rect.center().x - galley.size().x / 2.0, rect.bottom() + 5.0),
                galley,
                name_color,
            );
            if game.missing {
                painter.rect_filled(rect, radius, Color32::from_black_alpha(150));
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "Folder missing",
                    FontId::proportional(14.0),
                    ui.visuals().error_fg_color,
                );
            }
            if running {
                let badge = Rect::from_min_size(
                    rect.left_top() + Vec2::new(8.0, 8.0),
                    Vec2::new(if preparing || compiling { 140.0 } else { 70.0 }, 22.0),
                );
                let color = if preparing || compiling {
                    Color32::from_rgb(170, 110, 20)
                } else {
                    Color32::from_rgb(30, 140, 70)
                };
                painter.rect_filled(badge, CornerRadius::same(11), color);
                painter.text(
                    badge.center(),
                    Align2::CENTER_CENTER,
                    if preparing {
                        "Preparing shaders"
                    } else if compiling {
                        "Compiling shaders"
                    } else {
                        "Running"
                    },
                    FontId::proportional(13.0),
                    Color32::WHITE,
                );
            }
            let stroke = if selected {
                Stroke::new(3.0, ui.visuals().selection.bg_fill)
            } else if response.hovered() {
                Stroke::new(2.0, Color32::from_gray(180))
            } else {
                Stroke::new(1.0, Color32::from_gray(60))
            };
            painter.rect_stroke(rect, radius, stroke, StrokeKind::Outside);
        }

        let game = &self.games[index];
        let response = response.on_hover_text(&game.name);
        if response.clicked() {
            actions.push(Action::Select(Some(id.clone())));
        }
        if response.double_clicked() {
            actions.push(Action::Launch(id.clone()));
        }
        response.context_menu(|ui| {
            if running {
                if ui.button("Stop").clicked() {
                    actions.push(Action::Stop(id.clone()));
                }
            } else if ui.button("Play").clicked() {
                actions.push(Action::Launch(id.clone()));
            }
            if ui.button("Settings").clicked() {
                actions.push(Action::Select(Some(id.clone())));
            }
            if ui.button("Open game folder").clicked() {
                actions.push(Action::Open(game.root.clone()));
            }
            if let Some(data) = self.data_dir(game)
                && ui.button("Open saves").clicked()
            {
                actions.push(Action::Open(saves_dir(&data)));
            }
            if game.source == Source::Manual {
                ui.separator();
                if ui.button("Remove from list").clicked() {
                    actions.push(Action::RemoveGame(id.clone()));
                }
            }
        });
    }

    fn game_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(game) = self.game(&id).cloned() else {
            return;
        };
        let running = self.is_running(&id);
        let precompiling = self.precompiles.iter().any(|p| p.game_id == id);

        ui.horizontal(|ui| {
            ui.heading(RichText::new(&game.name).strong());
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if ui.small_button("×").on_hover_text("Close").clicked() {
                    actions.push(Action::Select(None));
                }
            });
        });
        let mut info = Vec::new();
        if let Some(p) = &game.publisher {
            info.push(p.clone());
        }
        if let Some(v) = &game.version {
            info.push(format!("version {v}"));
        }
        if !info.is_empty() {
            ui.label(RichText::new(info.join(" · ")).weak());
        }
        ui.label(
            RichText::new(game.root.display().to_string())
                .small()
                .weak(),
        );
        ui.add_space(8.0);

        let play = if running { "Stop" } else { "Play" };
        let button = egui::Button::new(RichText::new(play).size(18.0))
            .min_size(Vec2::new(ui.available_width(), 40.0));
        if ui.add_enabled(!game.missing, button).clicked() {
            actions.push(if running {
                Action::Stop(id.clone())
            } else {
                Action::Launch(id.clone())
            });
        }
        if let Some(r) = self.running.iter().find(|r| r.game_id == id) {
            ui.label(
                RichText::new(format!(
                    "Running for {}",
                    format_duration(r.started.elapsed())
                ))
                .small()
                .weak(),
            );
            ui.add_space(4.0);
            run_shaders_bar(ui, &r.shaders, ui.available_width(), false);
            let hover = if r.shaders.preparing().is_some() {
                "From xcl.log. XCL prepares the shaders in the game window before the game \
                 starts and logs how far it is every 5 seconds. Closing the game window \
                 cancels the launch."
            } else {
                "From xcl.log. XCL reports the pipelines it builds ahead at 1, 2, 4, 8 … \
                 only, and logs large pipelines' compiles only, so these are lower bounds."
            };
            ui.label(RichText::new(run_shaders_detail(&r.shaders)).small().weak())
                .on_hover_text(hover);
        }
        ui.add_space(8.0);

        ui.label(RichText::new("Folders").strong());
        let data = self.data_dir(&game);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Game").clicked() {
                actions.push(Action::Open(game.root.clone()));
            }
            if let Some(data) = &data {
                for (label, path, hint) in [
                    ("Saves", saves_dir(data), "The game's saves (XGameSave)"),
                    (
                        "Local storage",
                        paths::child_ci(data, "pls"),
                        "The game's persistent local storage (S:)",
                    ),
                    (
                        "XCL data",
                        data.clone(),
                        "Everything XCL keeps for this game",
                    ),
                    (
                        "Logs",
                        paths::child_ci(data, "logs"),
                        "xcl.log and report.json",
                    ),
                    (
                        "Cache",
                        paths::child_ci(data, "cache"),
                        "XCL's runtime cache for this game",
                    ),
                ] {
                    if ui.button(label).on_hover_text(hint).clicked() {
                        actions.push(Action::Open(path));
                    }
                }
                if ui
                    .button("xcl.toml")
                    .on_hover_text("The game's XCL configuration file")
                    .clicked()
                {
                    actions.push(Action::Open(paths::child_ci(data, "xcl.toml")));
                }
            }
            if ui
                .button("Console output")
                .on_hover_text("What XCL printed during the last launch from here")
                .clicked()
            {
                actions.push(Action::Open(
                    self.dirs.logs.join(format!("{}.log", game.title)),
                ));
            }
        });
        ui.add_space(8.0);

        ui.label(RichText::new("Shaders").strong());
        if let Some(p) = self.precompiles.iter().find(|p| p.game_id == id) {
            precompile_bar(ui, &p.progress, ui.available_width());
            ui.label(RichText::new(precompile_detail(&p.progress)).small().weak());
            ui.add_space(4.0);
        }
        ui.horizontal_wrapped(|ui| {
            if precompiling {
                if ui.button("Stop precompiling").clicked() {
                    actions.push(Action::StopPrecompile(id.clone()));
                }
            } else if ui
                .add_enabled(!running && !game.missing, egui::Button::new("Precompile shaders"))
                .on_hover_text("Compiles the game's compute shaders and pipelines ahead of time, without starting the game. XCL also prepares them in the game window on every launch; this does that work now. Resumable.")
                .clicked()
            {
                actions.push(Action::Precompile { id: id.clone(), used_only: self.precompile_used_only });
            }
            ui.checkbox(&mut self.precompile_used_only, "Only pipelines used before")
                .on_hover_text("--used-only: skips pipelines no earlier run of the game used. Faster.");
        });
        ui.add_space(8.0);
        ui.separator();

        ui.horizontal(|ui| {
            ui.label(RichText::new("Game settings").strong().size(16.0));
            let has_overrides = self
                .config
                .game_settings
                .get(&id)
                .is_some_and(|s| !s.is_empty());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(has_overrides, egui::Button::new("Reset to defaults"))
                    .clicked()
                {
                    self.config.game_settings.remove(&id);
                }
            });
        });
        ui.label(
            RichText::new("Ticked settings override the defaults (Settings) for this game only.")
                .small()
                .weak(),
        );
        let defaults = self.config.defaults.clone();
        let mut settings = self
            .config
            .game_settings
            .get(&id)
            .cloned()
            .unwrap_or_default();
        if settings_ui::launch_settings(ui, &id, &mut settings, Level::Game(&defaults)) {
            if settings.is_empty() {
                self.config.game_settings.remove(&id);
            } else {
                self.config.game_settings.insert(id.clone(), settings);
            }
        }

        if game.source == Source::Manual {
            ui.add_space(12.0);
            ui.separator();
            if ui
                .button("Remove from list")
                .on_hover_text("Only removes it from the launcher; no files are deleted")
                .clicked()
            {
                actions.push(Action::RemoveGame(id.clone()));
            }
        }
    }

    fn settings_view(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.set_max_width(820.0);
            ui.heading("Settings");
            ui.add_space(8.0);

            ui.label(RichText::new("XCL").strong().size(15.0));
            egui::Grid::new("xcl-settings").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label("xcl.exe");
                ui.horizontal(|ui| {
                    let mut text = self.config.xcl_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                    if ui.add(TextEdit::singleline(&mut text).desired_width(460.0).hint_text("path to xcl.exe")).changed() {
                        self.config.xcl_path = (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
                    }
                    if ui.button("Browse…").clicked()
                        && let Some(p) = rfd::FileDialog::new().set_title("Choose xcl.exe").add_filter("xcl.exe", &["exe"]).pick_file() {
                            self.config.xcl_path = Some(p);
                        }
                });
                ui.end_row();
                ui.label("");
                match &self.config.xcl_path {
                    Some(p) if p.is_file() => ui.label(RichText::new("Found.").small().weak()),
                    Some(_) => ui.label(RichText::new("No file at this path.").small().color(ui.visuals().error_fg_color)),
                    None => ui.label(RichText::new("Usually xcl\\target\\release\\xcl.exe in the XCL repository.").small().weak()),
                };
                ui.end_row();

                ui.label("XCL data folder");
                ui.horizontal(|ui| {
                    let mut text = self.config.xcl_data_root.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                    let default = paths::xcl_data_root(&AppConfig { xcl_data_root: None, ..self.config.clone() });
                    let hint = default.map(|d| d.display().to_string()).unwrap_or_default();
                    if ui.add(TextEdit::singleline(&mut text).desired_width(460.0).hint_text(hint)).on_hover_text("XCL_DATA_ROOT; leave empty for XCL's default").changed() {
                        self.config.xcl_data_root = (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
                    }
                    if ui.button("Browse…").clicked()
                        && let Some(p) = rfd::FileDialog::new().set_title("Choose XCL's data folder").pick_folder() {
                            self.config.xcl_data_root = Some(p);
                        }
                });
                ui.end_row();

                if !cfg!(windows) {
                    ui.label("Runner");
                    ui.add(TextEdit::singleline(&mut self.config.runner).desired_width(460.0).hint_text("wine"))
                        .on_hover_text("The program that runs xcl.exe: wine, a Proton script, umu-run …");
                    ui.end_row();
                    ui.label("Runner arguments");
                    ui.add(TextEdit::singleline(&mut self.config.runner_args).desired_width(460.0).hint_text("e.g. run (for Proton)"));
                    ui.end_row();
                    ui.label("Wine prefix");
                    ui.horizontal(|ui| {
                        let mut text = self.config.wine_prefix.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                        let hint = paths::wine_prefix(&AppConfig { wine_prefix: None, ..self.config.clone() }).display().to_string();
                        if ui.add(TextEdit::singleline(&mut text).desired_width(460.0).hint_text(hint)).changed() {
                            self.config.wine_prefix = (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
                        }
                        if ui.button("Browse…").clicked()
                            && let Some(p) = rfd::FileDialog::new().set_title("Choose the Wine prefix").pick_folder() {
                                self.config.wine_prefix = Some(p);
                            }
                    });
                    ui.end_row();
                }
            });

            ui.add_space(16.0);
            ui.label(RichText::new("Library folders").strong().size(15.0));
            ui.label(RichText::new("Every game folder (with a MicrosoftGame.config) inside these folders is listed.").small().weak());
            let mut remove = None;
            for (i, dir) in self.config.library_dirs.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                    if ui.link(dir.display().to_string()).clicked() {
                        actions.push(Action::Open(dir.clone()));
                    }
                });
            }
            if let Some(i) = remove {
                self.config.library_dirs.remove(i);
                actions.push(Action::Rescan);
            }
            if ui.button("Add library folder…").clicked() {
                actions.push(Action::AddLibrary);
            }

            ui.add_space(16.0);
            ui.label(RichText::new("Appearance").strong().size(15.0));
            ui.horizontal(|ui| {
                ui.label("Tile size");
                ui.add(egui::Slider::new(&mut self.config.tile_size, TILE_SIZE_RANGE).suffix(" pt"));
            });

            ui.add_space(16.0);
            ui.separator();
            ui.label(RichText::new("Default game settings").strong().size(18.0));
            ui.label(RichText::new("Used for every game unless the game overrides them. Unticked settings keep XCL's own default.").small().weak());
            let mut defaults = self.config.defaults.clone();
            if settings_ui::launch_settings(ui, "defaults", &mut defaults, Level::Defaults) {
                self.config.defaults = defaults;
            }
            ui.add_space(24.0);
        });
    }

    fn clean_dialog(&mut self, ctx: &egui::Context) {
        let Some(state) = self.clean.take() else {
            return;
        };
        let busy = !self.running.is_empty() || !self.precompiles.is_empty();
        let mut next = Some(state);
        let modal = egui::Modal::new(egui::Id::unique("clean-caches")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("Clean caches");
            ui.add_space(4.0);
            match next.take().expect("clean state") {
                CleanState::Scanning(rx) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Measuring the caches…");
                    });
                    next = Some(match rx.try_recv() {
                        Ok(targets) => CleanState::Choose(targets),
                        Err(_) => CleanState::Scanning(rx),
                    });
                    if ui.button("Cancel").clicked() {
                        next = None;
                    }
                }
                CleanState::Choose(mut targets) => {
                    ui.label(
                        RichText::new(
                            "Saves, game settings and the last run's logs are never deleted.",
                        )
                        .small()
                        .weak(),
                    );
                    ui.add_space(6.0);
                    if targets.is_empty() {
                        ui.label("Nothing to clean.");
                    }
                    ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                        for group in [Group::Shaders, Group::Games, Group::Other] {
                            if !targets.iter().any(|t| t.group == group) {
                                continue;
                            }
                            ui.add_space(6.0);
                            ui.label(RichText::new(group.label()).strong());
                            for t in targets.iter_mut().filter(|t| t.group == group) {
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut t.selected, &t.label).on_hover_text(t.hint);
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.label(
                                            RichText::new(format!(
                                                "{} · {} file{}",
                                                clean::format_bytes(t.bytes),
                                                t.files,
                                                if t.files == 1 { "" } else { "s" }
                                            ))
                                            .weak(),
                                        );
                                    });
                                });
                            }
                        }
                    });
                    ui.add_space(8.0);
                    let (count, bytes) = selection(&targets);
                    ui.horizontal(|ui| {
                        if ui.button("Select all").clicked() {
                            targets.iter_mut().for_each(|t| t.selected = true);
                        }
                        if ui.button("Select none").clicked() {
                            targets.iter_mut().for_each(|t| t.selected = false);
                        }
                        ui.label(format!("Selected: {}", clean::format_bytes(bytes)));
                    });
                    ui.separator();
                    let mut close = false;
                    let mut confirm = false;
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui
                            .add_enabled(count > 0, egui::Button::new("Clean selected…"))
                            .clicked()
                        {
                            confirm = true;
                        }
                    });
                    next = if close {
                        None
                    } else if confirm {
                        Some(CleanState::Confirm(targets))
                    } else {
                        Some(CleanState::Choose(targets))
                    };
                }
                CleanState::Confirm(targets) => {
                    let (count, bytes) = selection(&targets);
                    ui.label(format!(
                        "Delete {count} selected item{} ({})? This cannot be undone.",
                        if count == 1 { "" } else { "s" },
                        clean::format_bytes(bytes)
                    ));
                    for t in targets.iter().filter(|t| t.selected) {
                        ui.label(RichText::new(format!("• {}", t.label)).small());
                    }
                    let xcl_running = busy || crate::process::xcl_is_running();
                    if xcl_running {
                        ui.add_space(4.0);
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            "XCL is running. Close the game (and any precompiling) first.",
                        );
                    }
                    ui.add_space(8.0);
                    let mut back = false;
                    let mut go = false;
                    ui.horizontal(|ui| {
                        if ui.button("Back").clicked() {
                            back = true;
                        }
                        let delete =
                            egui::Button::new(RichText::new("Delete").color(Color32::WHITE))
                                .fill(Color32::from_rgb(170, 40, 40));
                        if ui.add_enabled(!xcl_running, delete).clicked() {
                            go = true;
                        }
                    });
                    next = Some(if go {
                        let (tx, rx) = channel();
                        let ctx = ui.ctx().clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(clean::clean(&targets));
                            ctx.request_repaint();
                        });
                        CleanState::Cleaning(rx)
                    } else if back {
                        CleanState::Choose(targets)
                    } else {
                        CleanState::Confirm(targets)
                    });
                    if xcl_running {
                        // re-check while the dialog is open
                        ui.ctx().request_repaint_after(Duration::from_secs(1));
                    }
                }
                CleanState::Cleaning(rx) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Deleting…");
                    });
                    next = Some(match rx.try_recv() {
                        Ok((freed, errors)) if errors.is_empty() => {
                            CleanState::Done(format!("Done. {} freed.", clean::format_bytes(freed)))
                        }
                        Ok((freed, errors)) => CleanState::Done(format!(
                            "{} freed. {} item(s) could not be deleted:\n{}",
                            clean::format_bytes(freed),
                            errors.len(),
                            errors
                                .iter()
                                .take(8)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("\n")
                        )),
                        Err(_) => CleanState::Cleaning(rx),
                    });
                }
                CleanState::Done(text) => {
                    ui.label(&text);
                    ui.add_space(8.0);
                    next = if ui.button("Close").clicked() {
                        None
                    } else {
                        Some(CleanState::Done(text))
                    };
                }
            }
        });
        // Escape or a click outside closes the dialog, except while deleting
        if modal.should_close()
            && !matches!(next, Some(CleanState::Cleaning(_) | CleanState::Confirm(_)))
        {
            next = None;
        }
        self.clean = next;
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_background(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let mut actions = Vec::new();
        let ctx = ui.ctx().clone();

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            self.top_bar(ui, &mut actions);
            ui.add_space(6.0);
        });
        if !self.notices.is_empty() || !self.precompiles.is_empty() || !self.running.is_empty() {
            egui::Panel::bottom("status").show(ui, |ui| {
                ui.add_space(4.0);
                self.status_bar(ui);
                ui.add_space(4.0);
            });
        }
        if self.view == View::Library && self.selected.is_some() {
            egui::Panel::right("game")
                .resizable(true)
                .default_size(400.0)
                .min_size(320.0)
                .show(ui, |ui| {
                    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                        ui.add_space(8.0);
                        self.game_panel(ui, &mut actions);
                        ui.add_space(16.0);
                    });
                });
        }
        egui::CentralPanel::default().show(ui, |ui| match self.view {
            View::Library => self.grid(ui, &mut actions),
            View::Settings => self.settings_view(ui, &mut actions),
        });
        self.clean_dialog(&ctx);

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.clean.is_none() {
            actions.push(Action::Select(None));
        }
        for action in actions {
            self.apply(&ctx, action);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.config != self.saved {
            let _ = self.config.save(&self.dirs.config_file);
        }
    }
}

/// Where XCL keeps the game's saves: `gamesave` (`XGameSave`), else `saves`.
fn saves_dir(data: &Path) -> PathBuf {
    let gamesave = paths::child_ci(data, "gamesave");
    if gamesave.exists() {
        gamesave
    } else {
        paths::child_ci(data, "saves")
    }
}

fn selection(targets: &[Target]) -> (usize, u64) {
    targets
        .iter()
        .filter(|t| t.selected)
        .fold((0, 0), |(c, b), t| (c + 1, b + t.bytes))
}

/// The line of XCL's output that says why it stopped: the last `CRASH:` or `xcl:` line, else
/// the last line. Only the end of the file is read.
fn error_line(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(4096))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines().rev().map(str::trim).filter(|l| !l.is_empty());
    let reason = lines
        .clone()
        .find(|l| l.starts_with("CRASH:") || l.starts_with("xcl:"));
    reason
        .or_else(|| lines.next())
        .map(|l| l.chars().take(240).collect())
}

/// A progress bar for a shader precompile: the share done and, while it is not known yet, an
/// animated bar.
fn precompile_bar(ui: &mut Ui, p: &crate::process::PrecompileProgress, width: f32) {
    let bar = match p.fraction() {
        Some(f) => egui::ProgressBar::new(f).text(format!("{:.0} %", f * 100.0)),
        None => egui::ProgressBar::new(0.0).text("Starting…"),
    };
    // the spinner shows XCL is at work while the count stands still (it moves every 200 shaders)
    ui.horizontal(|ui| {
        let spinner = ui.spacing().interact_size.y;
        ui.add(bar.desired_width((width - spinner - ui.spacing().item_spacing.x).max(40.0)));
        ui.spinner();
    });
}

/// The shader work of a running game: the startup warm-up as a bar (a lower bound: XCL logs
/// it at powers of two), else an activity bar while large pipelines compile.
/// `compact`: the short texts for the status bar.
fn run_shaders_bar(ui: &mut Ui, s: &crate::process::RunShaders, width: f32, compact: bool) {
    if let Some(p) = s.preparing() {
        preparation_bar(ui, p, width, compact);
        return;
    }
    let (built, total) = (
        group_digits(s.warmup_built),
        group_digits(s.warmup_total.unwrap_or_default()),
    );
    let bar = match s.warmup_fraction() {
        Some(f) if f < 1.0 => egui::ProgressBar::new(f).text(if compact {
            format!("Warm-up: {built}+ of {total}")
        } else {
            format!("Building pipelines from earlier runs: at least {built} of {total}")
        }),
        Some(_) => egui::ProgressBar::new(1.0).text(if compact {
            "Warm-up done"
        } else {
            "Pipelines from earlier runs built"
        }),
        None if s.compiling() > 0 => egui::ProgressBar::new(0.0).text("Compiling shaders"),
        None => egui::ProgressBar::new(0.0).text(if compact {
            "No shader work yet"
        } else {
            "No shader work logged yet"
        }),
    };
    ui.horizontal(|ui| {
        let spinner = ui.spacing().interact_size.y;
        ui.add(bar.desired_width((width - spinner - ui.spacing().item_spacing.x).max(40.0)));
        if s.compiling() > 0 {
            ui.spinner();
        }
    });
}

/// XCL's shader preparation before the game starts: the current step's count as a bar.
fn preparation_bar(ui: &mut Ui, p: &crate::process::Preparation, width: f32, compact: bool) {
    use crate::process::PreparationStep;
    let (n, what) = match p.step {
        PreparationStep::Translating => (1, "translating shaders"),
        PreparationStep::Compute => (2, "compute pipelines"),
        PreparationStep::Graphics => (3, "graphics pipelines"),
    };
    let text = match p.fraction() {
        Some(f) if compact => format!("Preparing {n}/3: {:.0} %", f * 100.0),
        Some(f) => format!(
            "Step {n} of 3, {what}: {} of {} ({:.0} %)",
            group_digits(p.done),
            group_digits(p.total),
            f * 100.0
        ),
        None if compact => format!("Preparing {n}/3"),
        None => format!("Step {n} of 3, {what}"),
    };
    let bar = egui::ProgressBar::new(p.fraction().unwrap_or(0.0)).text(text);
    ui.horizontal(|ui| {
        let spinner = ui.spacing().interact_size.y;
        ui.add(bar.desired_width((width - spinner - ui.spacing().item_spacing.x).max(40.0)));
        ui.spinner();
    });
}

/// "Preparing shaders · 20 workers · 3m 06s · no progress for 2m 21s: large pipelines"
fn preparation_detail(p: &crate::process::Preparation) -> String {
    let mut parts = vec!["Preparing shaders before the game starts".to_string()];
    if let Some(n) = p.only_used {
        parts.push(format!("only the {} shaders used before", group_digits(n)));
    }
    if p.workers > 0 {
        parts.push(format!("{} workers", p.workers));
    }
    parts.push(format_duration(Duration::from_secs_f64(p.seconds.max(0.0))));
    let still = p.still_for();
    if still >= 15.0 {
        parts.push(format!(
            "no progress for {}: building large pipelines",
            format_duration(Duration::from_secs_f64(still))
        ));
    }
    parts.join(" · ")
}

/// "1,234 pipelines built · 2 large ones compiling now"
fn run_shaders_detail(s: &crate::process::RunShaders) -> String {
    if let Some(p) = s.preparing() {
        return preparation_detail(p);
    }
    let mut parts = Vec::new();
    if let Some(p) = &s.preparation {
        use crate::process::PreparationEnd;
        match &p.end {
            Some(PreparationEnd::LeftOut) => parts.push("Preparation left out".to_string()),
            Some(PreparationEnd::Failed(e)) => parts.push(e.clone()),
            _ => parts.push(format!(
                "Prepared in {}",
                format_duration(Duration::from_secs_f64(p.seconds.max(0.0)))
            )),
        }
    }
    parts.push(format!("{} pipelines built", group_digits(s.built)));
    match s.compiling() {
        0 => {}
        1 => parts.push("1 large one compiling now".into()),
        n => parts.push(format!("{n} large ones compiling now")),
    }
    parts.join(" · ")
}

/// "Pack 1 of 2 · 13,400 / 26,698 shaders · 3m 10s · about 3m 05s left"
fn precompile_detail(p: &crate::process::PrecompileProgress) -> String {
    let mut parts = Vec::new();
    if p.packs > 1 {
        parts.push(format!("Pack {} of {}", p.pack.max(1), p.packs));
    }
    if let Some(total) = p.total {
        parts.push(format!(
            "{} / {} shaders",
            group_digits(p.done),
            group_digits(total)
        ));
    } else {
        parts.push("Reading the shader pack".into());
    }
    if p.pipelines_made + p.pipelines_refused > 0 {
        let refused = if p.pipelines_refused > 0 {
            format!(", {} refused", group_digits(p.pipelines_refused))
        } else {
            String::new()
        };
        parts.push(format!(
            "{} pipelines built{refused}",
            group_digits(p.pipelines_made)
        ));
    }
    parts.push(format_duration(p.started.elapsed()));
    if p.on_large_shaders() {
        parts.push("compiling large shaders, this can take minutes".into());
    } else if let Some(left) = p.remaining() {
        parts.push(format!("about {} left", format_duration(left)));
    }
    parts.join(" · ")
}

/// 26698 → "26,698".
fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, s / 60 % 60)
    } else {
        format!("{}m {:02}s", s / 60, s % 60)
    }
}
