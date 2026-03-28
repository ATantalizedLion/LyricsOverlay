use egui::{Color32, RichText, Ui};

use crate::settings::{ProgressBarPosition, Settings};

// TODO: Separate settings and theming (basically, color presets), might as well separate settings and state and settings into sub-structs while we are at it.
fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(8.0);
    ui.label(
        RichText::new(text)
            .size(11.0)
            .color(Color32::from_gray(130))
            .strong(),
    );
    ui.add_space(2.0);
}

fn settings_row(ui: &mut Ui, label: &str, tooltip: &str, widget: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .size(12.0)
                .color(Color32::from_gray(160)),
        )
        .on_hover_text(tooltip);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), widget);
    });
}

impl super::LyricsAppUI {
    pub(super) fn settings_ui(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let label = if self.settings_open { "" } else { "⚙" };
        if ui
            .add(
                egui::Button::new(
                    RichText::new(label)
                        .size(14.0)
                        .color(Color32::from_gray(160)),
                )
                .frame(false),
            )
            .clicked()
        {
            self.settings_open = !self.settings_open;
        }

        if !self.settings_open {
            return;
        }

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("settings_window"),
            egui::ViewportBuilder::default()
                .with_title("Lyrics Overlay — Settings")
                .with_inner_size([320.0, 480.0])
                .with_resizable(true),
            |ctx, _class| {
                // Make tooltips opaque
                ctx.style_mut(|style| {
                    style.visuals.window_fill = Color32::from_rgb(28, 28, 28);
                    style.visuals.popup_shadow = egui::Shadow::NONE;
                });
                // Close when the window's own X is clicked
                if ctx.input(|i| i.viewport().close_requested()) {
                    self.settings_open = false;
                }

                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut settings = self.settings.blocking_read().clone();
                    let snapshot = format!("{settings:?}");

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        display_settings(ui, &mut settings);
                        behaviour_settings(ui, &mut settings);
                        authentication_settings(ui, &mut settings);
                    });

                    reset_defaults(ui, &mut settings);

                    if format!("{settings:?}") != snapshot {
                        if let Err(e) = settings.save() {
                            self.error_string = Some(e);
                        }
                        *self.settings.blocking_write() = settings;
                    }
                });
            },
        );
    }
}

fn display_settings(ui: &mut Ui, settings: &mut Settings) {
    section_label(ui, "Display");

    settings_row(ui, "Font size", "Size of the font used for lyrics", |ui| {
        ui.add(
            egui::Slider::new(&mut settings.font_size, 10.0..=72.0)
                .step_by(1.0)
                .suffix(" px")
                .text_color(Color32::from_gray(200)),
        );
    });
    settings_row(
        ui,
        "Background opacity",
        "Opacity of the background of the main window",
        |ui| {
            ui.add(
                egui::Slider::new(&mut settings.opacity, 0.0..=1.0)
                    .step_by(0.01)
                    .custom_formatter(|v, _| format!("{:.00}%", v * 100.))
                    .text_color(Color32::from_gray(200)),
            );
        },
    );
    settings_row(
        ui,
        "Dim distant lines",
        "Do we reduce opacity of lines not currently being sung",
        |ui| {
            ui.checkbox(&mut settings.dim_distant_lines, "");
        },
    );
    settings_row(
        ui,
        "Scroll smoothly",
        "Scroll smoothly, or jump from line to line",
        |ui| {
            ui.checkbox(&mut settings.scroll_smoothly, "");
        },
    );
    settings_row(
        ui,
        "Transition time",
        "Time spent transitioning from one line to the next (if scrolling smoothly)",
        |ui| {
            ui.add(
                egui::Slider::new(&mut settings.line_transition_ms, 0..=1000)
                    .step_by(10.0)
                    .custom_formatter(|v, _| format!("{}ms", v))
                    .text_color(Color32::from_gray(200)),
            );
        },
    );
    settings_row(ui, "Show debug stuff", "Do we show debug stuff?", |ui| {
        ui.checkbox(&mut settings.draw_debug_stuff, "");
    });
}

fn behaviour_settings(ui: &mut Ui, settings: &mut Settings) {
    section_label(ui, "Behaviour");

    settings_row(ui, "Refresh interval", "", |ui| {
        ui.add(
            egui::Slider::new(&mut settings.poll_interval_ms, 1000..=10000)
                .suffix(" ms")
                .text_color(Color32::from_gray(200)),
        );
    });
    settings_row(
        ui,
        "Cache lyrics",
        "Do we cache any requested lyrics, improves future responsiveness and reduces load on the LRC lib",
        |ui| {
            ui.checkbox(&mut settings.caching_enabled, "");
        },
    );
    if settings.caching_enabled {
        settings_row(
            ui,
            "Cache folder",
            "Where do you want to store cache?",
            |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut settings.cache_folder)
                        .desired_width(120.0)
                        .text_color(Color32::from_gray(200)),
                );
            },
        );
    }
    settings_row(ui, "Log level", "Log level, what more can I say", |ui| {
        egui::ComboBox::from_id_salt("log_level")
            .selected_text(settings.log_level.as_str())
            .show_ui(ui, |ui| {
                for level in ["error", "warn", "info", "debug", "trace"] {
                    ui.selectable_value(&mut settings.log_level, level.to_string(), level);
                }
            });
    });
    settings_row(
        ui,
        "Line progress bar",
        "Shows a bar with progress/duration of current line",
        |ui| {
            egui::ComboBox::from_id_salt("progress_bar_position")
                .selected_text(settings.line_progress_bar_position.as_str())
                .show_ui(ui, |ui| {
                    for pos in [
                        ProgressBarPosition::BelowCurrentLine,
                        ProgressBarPosition::Bottom,
                        ProgressBarPosition::Hidden,
                    ] {
                        ui.selectable_value(
                            &mut settings.line_progress_bar_position,
                            pos,
                            pos.as_str(),
                        );
                    }
                });
        },
    );
    settings_row(
        ui,
        "Song progress bar",
        "Shows a bar with progress/duration of current song",
        |ui| {
            egui::ComboBox::from_id_salt("song_progress_bar_position")
                .selected_text(settings.song_progress_bar_position.as_str())
                .show_ui(ui, |ui| {
                    for pos in [
                        ProgressBarPosition::BelowCurrentLine,
                        ProgressBarPosition::Bottom,
                        ProgressBarPosition::Hidden,
                    ] {
                        ui.selectable_value(
                            &mut settings.song_progress_bar_position,
                            pos,
                            pos.as_str(),
                        );
                    }
                });
        },
    );
}

fn authentication_settings(ui: &mut Ui, settings: &mut Settings) {
    section_label(ui, "Authentication");

    settings_row(
        ui,
        "Spotify developer Client ID",
        "Client ID as found in spotify developer dashboard",
        |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut settings.client_id)
                    .desired_width(120.0)
                    .text_color(Color32::from_gray(200)),
            );
        },
    );

    settings_row(
        ui,
        "Spotify developer Client Secret",
        "Client Secret as found in spotify developer dashboard",
        |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut settings.client_secret)
                    .desired_width(120.0)
                    .text_color(Color32::from_gray(200)),
            );
        },
    );
}

fn reset_defaults(ui: &mut Ui, settings: &mut Settings) {
    ui.add_space(8.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
        if ui
            .add(
                egui::Button::new(
                    RichText::new("Reset defaults")
                        .size(11.0)
                        .color(Color32::from_gray(110)),
                )
                .frame(false),
            )
            .clicked()
        {
            // Preserve credentials when resetting display/behaviour settings
            let client_id = settings.client_id.clone();
            let client_secret = settings.client_secret.clone();
            let sp_dc = settings.sp_dc.clone();
            settings.reset();
            settings.client_id = client_id;
            settings.client_secret = client_secret;
            settings.sp_dc = sp_dc;
        }
    });
}
