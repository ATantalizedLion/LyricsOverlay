use egui::{Color32, RichText, Ui};

use crate::{lyrics_parser::LyricPosition, overlay::LyricsAppUI};

impl LyricsAppUI {
    pub(super) fn display_lyrics(&mut self, ui: &mut Ui) {
        let Some(song) = &self.current_song_with_lyrics else {
            self.waiting_for_lyrics(ui);
            return;
        };

        let progress_ms = self.currently_playing.as_ref().map_or(0, |p| p.progress_ms);

        if self.playback_state {
            // This is reset every time we receive a new currently playing
            self.ms_played_since_last_update += self.time_of_last_frame.elapsed().as_millis();
        }

        let current_ms = progress_ms as u128 + self.ms_played_since_last_update;
        let current_pos = song
            .lyrics
            .find_current_index(current_ms.try_into().unwrap());
        let current_idx = match current_pos {
            LyricPosition::Line(n) => Some(n),
            _ => None,
        };

        let line_height = 36.0;
        let panel_height = ui.available_height();

        // Scroll so the current line sits in the vertical center
        // TODO: This has some drift.
        let scroll_offset = current_idx
            .map_or(0.0, |i| {
                i as f32 * line_height - panel_height / 2.0 + line_height / 2.0
            })
            .max(0.0);

        egui::ScrollArea::vertical()
            .vertical_scroll_offset(scroll_offset)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    for (i, line) in song.lyrics.synced_lyrics.iter().enumerate() {
                        let dist = current_idx.map_or(99, |ci| i.abs_diff(ci));

                        let (size, alpha) = match dist {
                            0 => (26.0, 255u8), // current — full white, large
                            1 => (20.0, 180u8), // adjacent — slightly dimmed
                            2 => (17.0, 110u8),
                            _ => (15.0, 55u8), // far — ghosted
                        };

                        let color = match current_idx {
                            None => Color32::from_rgba_unmultiplied(200, 200, 200, alpha),
                            Some(ci) if i < ci => {
                                Color32::from_rgba_unmultiplied(200, 180, 255, alpha)
                            } // past, slightly purple
                            Some(ci) if i == ci => {
                                Color32::from_rgba_unmultiplied(255, 255, 255, alpha)
                            } // current, white
                            Some(_) => Color32::from_rgba_unmultiplied(180, 210, 255, alpha), // future, slightly blue
                        };

                        let text = RichText::new(&line.text).size(size).color(color);

                        // Reserve fixed height per line so scroll math is stable
                        ui.allocate_ui(egui::vec2(ui.available_width(), line_height), |ui| {
                            ui.centered_and_justified(|ui| {
                                ui.label(text);
                            });
                        });
                    }
                    // Padding so last lines can scroll to center
                    ui.add_space(panel_height / 2.0);
                });
            });
    }

    fn waiting_for_lyrics(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            if let Some(playing) = &self.currently_playing
                && let Some(title) = playing.get_track_title()
            {
                ui.label(
                    RichText::new(format!("♫  {title}"))
                        .size(18.0)
                        .color(Color32::from_gray(180)),
                );
            }
            ui.label(
                RichText::new("Loading lyrics…")
                    .size(14.0)
                    .color(Color32::from_gray(100)),
            );
        });
    }
}
