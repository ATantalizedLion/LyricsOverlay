use egui::{Color32, RichText, Ui};

use crate::settings::Settings;

fn section_label(ui: &mut Ui, text: &str) {
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
    pub(super) fn settings_ui(&mut self, ui: &mut Ui) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            let label = if self.settings_open { "✕" } else { "⚙" };
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

        let mut s = self.settings.lock().unwrap().clone();

        let snapshot = format!("{s:?}");
        egui::Frame::new()
            .fill(Color32::from_rgba_unmultiplied(20, 20, 30, 230))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .stroke(egui::Stroke::new(
                1.0,
                Color32::from_rgba_unmultiplied(255, 255, 255, 25),
            ))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());

                ui.label(
                    RichText::new("Settings")
                        .size(13.0)
                        .color(Color32::from_gray(200))
                        .strong(),
                );
                ui.add_space(8.0);

                display_settings(ui, &mut s);
                behaviour_settings(ui, &mut s);
            });

        // Write back and persist only on change
        if format!("{s:?}") != snapshot {
            if let Err(e) = s.save() {
                self.error_string = Some(e);
            }
            *self.settings.lock().unwrap() = s;
        }

        ui.add_space(4.0);
    }
}

fn display_settings(ui: &mut Ui, s: &mut Settings) {
    section_label(ui, "Display");

    settings_row(ui, "Font size", |ui| {
        ui.add(
            egui::Slider::new(&mut s.font_size, 10.0..=72.0)
                .step_by(1.0)
                .suffix(" px")
                .text_color(Color32::from_gray(200)),
        );
    });
    settings_row(ui, "Background opacity", |ui| {
        ui.add(
            egui::Slider::new(&mut s.opacity, 0.0..=1.0)
                .step_by(0.01)
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                .text_color(Color32::from_gray(200)),
        );
    });
    settings_row(ui, "Dim distant lines", |ui| {
        ui.checkbox(&mut s.dim_distant_lines, "");
    });

    ui.add_space(8.0);
}

fn behaviour_settings(ui: &mut Ui, s: &mut Settings) {
    section_label(ui, "Behaviour");

    settings_row(ui, "Refresh interval", |ui| {
        ui.add(
            egui::Slider::new(&mut s.poll_interval_secs, 1..=30)
                .suffix(" s")
                .text_color(Color32::from_gray(200)),
        );
    });
    settings_row(ui, "Cache lyrics", |ui| {
        ui.checkbox(&mut s.caching_enabled, "");
    });
    if s.caching_enabled {
        settings_row(ui, "Cache folder", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut s.cache_folder)
                    .desired_width(120.0)
                    .text_color(Color32::from_gray(200)),
            );
        });
    }
    settings_row(ui, "Log level", |ui| {
        egui::ComboBox::from_id_salt("log_level")
            .selected_text(s.log_level.as_str())
            .show_ui(ui, |ui| {
                for level in ["error", "warn", "info", "debug", "trace"] {
                    ui.selectable_value(&mut s.log_level, level.to_string(), level);
                }
            });
    });

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
            let client_id = s.client_id.clone();
            let client_secret = s.client_secret.clone();
            s.reset();
            s.client_id = client_id;
            s.client_secret = client_secret;
        }
    });
}
