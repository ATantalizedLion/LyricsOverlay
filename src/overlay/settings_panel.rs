use egui::{Color32, RichText, Ui};

use crate::settings::{ProgressBarPosition, Settings};

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

fn settings_row(ui: &mut Ui, label: &str, widget: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .size(12.0)
                .color(Color32::from_gray(160)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), widget);
    });
}

impl super::LyricsAppUI {
    pub(super) fn settings_ui(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            let label = if self.settings_open { "X" } else { "⚙" };
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
        });

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

    settings_row(ui, "Font size", |ui| {
        ui.add(
            egui::Slider::new(&mut settings.font_size, 10.0..=72.0)
                .step_by(1.0)
                .suffix(" px")
                .text_color(Color32::from_gray(200)),
        );
    });
    settings_row(ui, "Background opacity", |ui| {
        ui.add(
            egui::Slider::new(&mut settings.opacity, 0.0..=1.0)
                .step_by(0.01)
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                .text_color(Color32::from_gray(200)),
        );
    });
    settings_row(ui, "Dim distant lines", |ui| {
        ui.checkbox(&mut settings.dim_distant_lines, "");
    });
    settings_row(ui, "Scroll smoothly", |ui| {
        ui.checkbox(&mut settings.scroll_smoothly, "");
    });
    settings_row(ui, "Transition time in ms", |ui| {
        ui.add(
            egui::Slider::new(&mut settings.line_transition_ms, 0..=1000)
                .step_by(10.0)
                .custom_formatter(|v, _| format!("{}ms", v))
                .text_color(Color32::from_gray(200)),
        );
    });
    settings_row(ui, "Show debug stuff", |ui| {
        ui.checkbox(&mut settings.draw_debug_stuff, "");
    });
}

fn behaviour_settings(ui: &mut Ui, settings: &mut Settings) {
    section_label(ui, "Behaviour");

    settings_row(ui, "Refresh interval", |ui| {
        ui.add(
            egui::Slider::new(&mut settings.poll_interval_ms, 1000..=10000)
                .suffix(" ms")
                .text_color(Color32::from_gray(200)),
        );
    });
    settings_row(ui, "Cache lyrics", |ui| {
        ui.checkbox(&mut settings.caching_enabled, "");
    });
    if settings.caching_enabled {
        settings_row(ui, "Cache folder", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut settings.cache_folder)
                    .desired_width(120.0)
                    .text_color(Color32::from_gray(200)),
            );
        });
    }
    settings_row(ui, "Log level", |ui| {
        egui::ComboBox::from_id_salt("log_level")
            .selected_text(settings.log_level.as_str())
            .show_ui(ui, |ui| {
                for level in ["error", "warn", "info", "debug", "trace"] {
                    ui.selectable_value(&mut settings.log_level, level.to_string(), level);
                }
            });
    });
    settings_row(ui, "Progress bar", |ui| {
        egui::ComboBox::from_id_salt("progress_bar_position")
            .selected_text(settings.progress_bar_position.as_str())
            .show_ui(ui, |ui| {
                for pos in [
                    ProgressBarPosition::BelowCurrentLine,
                    ProgressBarPosition::Bottom,
                    ProgressBarPosition::Hidden,
                ] {
                    ui.selectable_value(&mut settings.progress_bar_position, pos, pos.as_str());
                }
            });
    });
}

fn authentication_settings(ui: &mut Ui, settings: &mut Settings) {
    section_label(ui, "Authentication");

    settings_row(ui, "Spotify developer Client ID", |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut settings.client_id)
                .desired_width(120.0)
                .text_color(Color32::from_gray(200)),
        );
    });

    settings_row(ui, "Spotify developer Client Secret", |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut settings.client_secret)
                .desired_width(120.0)
                .text_color(Color32::from_gray(200)),
        );
    });
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
