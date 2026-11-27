//! Editors for the launch settings, used for the application defaults and for a game's
//! overrides alike.

use eframe::egui::{self, ComboBox, DragValue, Grid, RichText, Slider, TextEdit, Ui};

use crate::config::{
    DeviceType, EnvVar, FrameLimit, LaunchSettings, Raytracing, SkipBink, WindowPos,
};

const RESOLUTIONS: [(u32, u32); 6] = [
    (1280, 720),
    (1600, 900),
    (1920, 1080),
    (2560, 1440),
    (3440, 1440),
    (3840, 2160),
];
const FPS_PRESETS: [u32; 5] = [30, 60, 120, 144, 240];

/// What an unset setting means at this level.
#[derive(Clone, Copy)]
pub enum Level<'a> {
    /// The application defaults: unset leaves XCL's own default.
    Defaults,
    /// A game: unset uses the application default.
    Game(&'a LaunchSettings),
}

/// Edits `s`. Returns true when something changed.
pub fn launch_settings(ui: &mut Ui, id: &str, s: &mut LaunchSettings, level: Level) -> bool {
    let before = s.clone();
    let base = match level {
        Level::Defaults => None,
        Level::Game(b) => Some(b),
    };

    section(
        ui,
        "Window and input",
        "Written into the game's xcl.toml before it starts.",
    );
    Grid::new((id, "display"))
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            // width and height go together
            let mut size = s.window_width.zip(s.window_height);
            let base_size = base.and_then(|b| b.window_width.zip(b.window_height));
            row(
                ui,
                "Window size",
                &mut size,
                base_size.unwrap_or((1280, 720)),
                inherited(base_size.map(|(w, h)| format!("{w} × {h}")), "1280 × 720"),
                |ui, (w, h)| {
                    ComboBox::from_id_salt((id, "res"))
                        .selected_text(format!("{w} × {h}"))
                        .show_ui(ui, |ui| {
                            for (rw, rh) in RESOLUTIONS {
                                if ui
                                    .selectable_label((*w, *h) == (rw, rh), format!("{rw} × {rh}"))
                                    .clicked()
                                {
                                    (*w, *h) = (rw, rh);
                                }
                            }
                        });
                    ui.add(DragValue::new(w).range(320..=7680).suffix(" w"));
                    ui.add(DragValue::new(h).range(200..=4320).suffix(" h"));
                },
            );
            (s.window_width, s.window_height) = match size {
                Some((w, h)) => (Some(w), Some(h)),
                None => (None, None),
            };

            row(
                ui,
                "Window position",
                &mut s.window_pos,
                base.and_then(|b| b.window_pos).unwrap_or(WindowPos::Center),
                inherited(
                    base.and_then(|b| b.window_pos).map(describe_pos),
                    "chosen by Windows",
                ),
                |ui, pos| {
                    let label = match pos {
                        WindowPos::Auto => "Chosen by Windows",
                        WindowPos::Center => "Centred",
                        WindowPos::At { .. } => "At position",
                    };
                    ComboBox::from_id_salt((id, "pos"))
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(pos, WindowPos::Auto, "Chosen by Windows");
                            ui.selectable_value(pos, WindowPos::Center, "Centred");
                            if ui
                                .selectable_label(
                                    matches!(pos, WindowPos::At { .. }),
                                    "At position",
                                )
                                .clicked()
                                && !matches!(pos, WindowPos::At { .. })
                            {
                                *pos = WindowPos::At { x: 0, y: 0 };
                            }
                        });
                    if let WindowPos::At { x, y } = pos {
                        ui.add(DragValue::new(x).prefix("x "));
                        ui.add(DragValue::new(y).prefix("y "));
                    }
                },
            );

            row(
                ui,
                "Gamertag",
                &mut s.gamertag,
                base.and_then(|b| b.gamertag.clone())
                    .unwrap_or_else(|| "Player1".into()),
                inherited(base.and_then(|b| b.gamertag.clone()), "Player1"),
                |ui, tag| {
                    clipboard_menu(ui.add(TextEdit::singleline(tag).desired_width(160.0)));
                },
            );

            row(
                ui,
                "Keyboard and mouse",
                &mut s.keyboard_mouse,
                base.and_then(|b| b.keyboard_mouse).unwrap_or(true),
                inherited(base.and_then(|b| b.keyboard_mouse).map(on_off), "on"),
                |ui, on| {
                    ui.checkbox(on, "Play with keyboard and mouse as a gamepad");
                },
            );

            row(
                ui,
                "Mouse sensitivity",
                &mut s.mouse_sensitivity,
                base.and_then(|b| b.mouse_sensitivity).unwrap_or(1.0),
                inherited(
                    base.and_then(|b| b.mouse_sensitivity)
                        .map(|m| format!("{m:.2}")),
                    "1.00",
                ),
                |ui, m| {
                    ui.add(Slider::new(m, 0.1..=5.0).fixed_decimals(2));
                },
            );
        });
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "XCL has no fullscreen mode yet; set the window size to your screen size instead.",
        )
        .small()
        .weak(),
    );

    section(ui, "Game", "Passed to XCL as environment variables.");
    Grid::new((id, "env")).num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
        row(
            ui,
            "Network",
            &mut s.network,
            base.and_then(|b| b.network).unwrap_or(true),
            inherited(base.and_then(|b| b.network).map(on_off), "on"),
            |ui, on| {
                ui.checkbox(on, "Allow network access").on_hover_text("Off sets XCL_NETWORK=0: the game runs as on a console without a network.");
            },
        );

        row(
            ui,
            "Movies",
            &mut s.skip_bink,
            base.and_then(|b| b.skip_bink).unwrap_or(SkipBink::Intros),
            inherited(base.and_then(|b| b.skip_bink).map(|b| b.label().to_owned()), "play all"),
            |ui, b| {
                ComboBox::from_id_salt((id, "bink")).selected_text(b.label()).show_ui(ui, |ui| {
                    for v in SkipBink::ALL {
                        ui.selectable_value(b, v, v.label());
                    }
                });
            },
        );

        row(
            ui,
            "Frame rate limit",
            &mut s.frame_limit,
            base.and_then(|b| b.frame_limit).unwrap_or(FrameLimit::Fps(60)),
            inherited(base.and_then(|b| b.frame_limit).map(describe_fps), "the game's own"),
            |ui, f| {
                let text = describe_fps(*f);
                ComboBox::from_id_salt((id, "fps")).selected_text(text).show_ui(ui, |ui| {
                    ui.selectable_value(f, FrameLimit::Uncapped, "Uncapped");
                    for fps in FPS_PRESETS {
                        ui.selectable_value(f, FrameLimit::Fps(fps), format!("{fps} fps"));
                    }
                });
                if let FrameLimit::Fps(fps) = f {
                    ui.add(DragValue::new(fps).range(1..=1000).suffix(" fps"));
                }
            },
        );

        row(
            ui,
            "Console",
            &mut s.device_type,
            base.and_then(|b| b.device_type).unwrap_or(DeviceType::SeriesX),
            inherited(base.and_then(|b| b.device_type).map(|d| d.label().to_owned()), "from xcl.toml (Series X)"),
            |ui, d| {
                ComboBox::from_id_salt((id, "device")).selected_text(d.label()).show_ui(ui, |ui| {
                    for v in DeviceType::ALL {
                        ui.selectable_value(d, v, v.label());
                    }
                });
            },
        );

        row(
            ui,
            "Ray tracing",
            &mut s.raytracing,
            base.and_then(|b| b.raytracing).unwrap_or(Raytracing::Auto),
            inherited(base.and_then(|b| b.raytracing).map(|r| r.label().to_owned()), "from xcl.toml (GPU's own tier)"),
            |ui, r| {
                ComboBox::from_id_salt((id, "rt")).selected_text(r.label()).show_ui(ui, |ui| {
                    for v in Raytracing::ALL {
                        ui.selectable_value(r, v, v.label());
                    }
                });
            },
        );

        row(
            ui,
            "GPU",
            &mut s.adapter,
            base.and_then(|b| b.adapter.clone()).unwrap_or_else(|| "auto".into()),
            inherited(base.and_then(|b| b.adapter.clone()), "from xcl.toml (auto)"),
            |ui, a| {
                clipboard_menu(ui.add(TextEdit::singleline(a).desired_width(160.0)))
                    .on_hover_text("\"auto\", the adapter's number (1, 2, …) or part of its name (\"radeon\", \"nvidia\").");
            },
        );

        row(
            ui,
            "Mute speech",
            &mut s.mute_tts,
            base.and_then(|b| b.mute_tts).unwrap_or(true),
            inherited(base.and_then(|b| b.mute_tts).map(on_off), "off"),
            |ui, on| {
                ui.checkbox(on, "No text-to-speech").on_hover_text("XCL_MUTE_TTS: the game gets no Windows speech synthesis.");
            },
        );

        row(
            ui,
            "PlayStation controllers",
            &mut s.playstation_pad,
            base.and_then(|b| b.playstation_pad).unwrap_or(true),
            inherited(base.and_then(|b| b.playstation_pad).map(on_off), "on"),
            |ui, on| {
                ui.checkbox(on, "Use a DualShock 4 / DualSense as the gamepad");
            },
        );

        row(
            ui,
            "Game arguments",
            &mut s.game_args,
            base.and_then(|b| b.game_args.clone()).unwrap_or_default(),
            inherited(base.and_then(|b| b.game_args.clone()), "none"),
            |ui, a| {
                clipboard_menu(ui.add(TextEdit::singleline(a).desired_width(240.0).hint_text("-arg \"with space\"")));
            },
        );
    });

    section(
        ui,
        "Environment variables",
        "Any other XCL_* switch (see XCL's README). A game's variables are added to the defaults.",
    );
    env_editor(ui, id, s, base);

    *s != before
}

/// Adds a right-click menu with Cut, Copy and Paste to a text field (the keyboard shortcuts
/// work in every text field anyway).
pub fn clipboard_menu(field: egui::Response) -> egui::Response {
    use egui::text_edit::TextEditState;
    use egui::text_selection::CCursorRange;
    // the right-click that opens the menu drops the field's selection: remember it while the
    // field is being edited, and put it back before cutting or copying
    let saved = field.id.with("clipboard_menu_selection");
    let ctx = field.ctx.clone();
    let right_click = ctx.input(|i| {
        i.pointer.button_down(egui::PointerButton::Secondary)
            || i.pointer.button_released(egui::PointerButton::Secondary)
    });
    if field.has_focus() && !right_click && !field.context_menu_opened() {
        let range = TextEditState::load(&ctx, field.id).and_then(|s| s.cursor.char_range());
        ctx.data_mut(|d| d.insert_temp::<Option<CCursorRange>>(saved, range));
    }
    field.context_menu(|ui| {
        for (label, cmd) in [
            ("Cut", egui::ViewportCommand::RequestCut),
            ("Copy", egui::ViewportCommand::RequestCopy),
            ("Paste", egui::ViewportCommand::RequestPaste),
        ] {
            if ui.button(label).clicked() {
                if let Some(range) = ctx.data(|d| d.get_temp::<Option<CCursorRange>>(saved)) {
                    let mut state = TextEditState::load(&ctx, field.id).unwrap_or_default();
                    state.cursor.set_char_range(range);
                    state.store(&ctx, field.id);
                }
                // the command arrives as a cut/copy/paste event for the focused field in the
                // next frame
                field.request_focus();
                ctx.send_viewport_cmd(cmd);
                ui.close();
            }
        }
    });
    field
}

fn env_editor(ui: &mut Ui, id: &str, s: &mut LaunchSettings, base: Option<&LaunchSettings>) {
    if let Some(base) = base {
        for var in base
            .env
            .iter()
            .filter(|b| !s.env.iter().any(|o| o.name.trim() == b.name.trim()))
        {
            ui.label(
                RichText::new(format!("{}={}  (default)", var.name, var.value))
                    .weak()
                    .monospace(),
            );
        }
    }
    let mut remove = None;
    // the fields share the panel's width: name 40 %, value the rest, never narrower than
    // what a typical `XCL_*` name needs
    let font = egui::FontId::monospace(14.0);
    for (i, var) in s.env.iter_mut().enumerate() {
        ui.push_id((id, "env", i), |ui| {
            ui.horizontal(|ui| {
                let gap = ui.spacing().item_spacing.x;
                let height = ui.spacing().interact_size.y.max(26.0);
                let fields = (ui.available_width() - 80.0 - 2.0 * gap).max(320.0);
                let name_w = (fields * 0.38).max(150.0);
                clipboard_menu(
                    ui.add_sized(
                        [name_w, height],
                        TextEdit::singleline(&mut var.name)
                            .font(font.clone())
                            .align(egui::Align2::LEFT_CENTER)
                            .hint_text("NAME"),
                    ),
                );
                clipboard_menu(
                    ui.add_sized(
                        [(fields - name_w).max(160.0), height],
                        TextEdit::singleline(&mut var.value)
                            .font(font.clone())
                            .align(egui::Align2::LEFT_CENTER)
                            .hint_text("value"),
                    ),
                );
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        s.env.remove(i);
    }
    if ui.button("Add variable").clicked() {
        s.env.push(EnvVar::new("XCL_", ""));
    }
}

/// A row with a checkbox that turns the setting on (with `init`) or off (showing what applies
/// instead).
fn row<T>(
    ui: &mut Ui,
    label: &str,
    value: &mut Option<T>,
    init: T,
    unset_text: String,
    edit: impl FnOnce(&mut Ui, &mut T),
) {
    let mut set = value.is_some();
    if ui.checkbox(&mut set, label).changed() {
        *value = if set { Some(init) } else { None };
    }
    ui.horizontal(|ui| match value {
        Some(v) => edit(ui, v),
        None => {
            ui.label(RichText::new(unset_text).weak());
        }
    });
    ui.end_row();
}

fn inherited(from_defaults: Option<String>, xcl_default: &str) -> String {
    match from_defaults {
        Some(v) => format!("Default: {v}"),
        None => format!("XCL default: {xcl_default}"),
    }
}

fn section(ui: &mut Ui, title: &str, hint: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).strong().size(15.0));
    ui.label(RichText::new(hint).small().weak());
    ui.add_space(4.0);
}

fn on_off(b: bool) -> String {
    if b { "on".into() } else { "off".into() }
}

fn describe_pos(p: WindowPos) -> String {
    match p {
        WindowPos::Auto => "chosen by Windows".into(),
        WindowPos::Center => "centred".into(),
        WindowPos::At { x, y } => format!("at {x}, {y}"),
    }
}

fn describe_fps(f: FrameLimit) -> String {
    match f {
        FrameLimit::Uncapped => "Uncapped".into(),
        FrameLimit::Fps(fps) => format!("{fps} fps"),
    }
}
