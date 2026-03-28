use egui::{Color32, RichText, Ui};

use crate::{MessageToRT, overlay::LyricsAppUI};

impl LyricsAppUI {
    pub fn authentication_ui(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 2.0 + 22.0);
            ui.label(
                RichText::new("♫ Lyrics Overlay")
                    .size(22.0)
                    .color(Color32::WHITE),
            );
            ui.add_space(12.0);

            // Read settings once to avoid repeated lock contention
            if let Ok(mut settings) = self.settings.try_write() {
                ui.label(
                    RichText::new("Client ID")
                        .size(12.0)
                        .color(Color32::from_gray(160)),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut settings.client_id)
                        .desired_width(200.0)
                        .hint_text("Spotify developer client ID"),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Client Secret")
                        .size(12.0)
                        .color(Color32::from_gray(160)),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut settings.client_secret)
                        .desired_width(200.0)
                        .hint_text("Spotify developer client secret")
                        .password(true),
                );
                // Persist if changed
                if settings.client_id != self.settings_cache.client_id
                    || settings.client_secret != self.settings_cache.client_secret
                {
                    if let Err(e) = settings.save() {
                        self.error_string = Some(e);
                    }
                    self.settings_cache = settings.clone();
                }
            }

            ui.add_space(12.0);
            let has_credentials = !self.settings_cache.client_id.is_empty()
                && !self.settings_cache.client_secret.is_empty();
            ui.add_enabled_ui(has_credentials, |ui| {
                if ui.button("Connect Spotify").clicked() {
                    self.tx.try_send(MessageToRT::Authenticate).unwrap();
                }
            });
            if !has_credentials {
                ui.label(
                    RichText::new("Enter credentials above to connect")
                        .size(11.0)
                        .color(Color32::from_gray(100)),
                );
            }
        });
    }
}
