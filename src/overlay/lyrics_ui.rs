use egui::{Align, Color32, Layout, Rect, RichText, ScrollArea, Sense, Ui, Vec2};

use crate::{lyrics_parser::LyricPosition, overlay::LyricsAppUI, settings::ProgressBarPosition};

fn cubic_ease_in_out(t: f32) -> f32 {
    //    t * t * (3.0 - 2.0 * t)
    t // TODO: Add easing back in once we have added transition time  
}

impl LyricsAppUI {
    pub(super) fn display_lyrics(&mut self, ui: &mut Ui) {
        // Do we have lyrics
        let Some(song) = &self.current_song_with_lyrics else {
            self.waiting_for_lyrics(ui);
            return;
        };
        // Make sure it's not the previous song's lyrics
        if Some(song.track_name.clone())
            != self
                .currently_playing
                .as_ref()
                .map_or(None, |p| p.get_track_title())
        {
            self.waiting_for_lyrics(ui);
            return;
        }

        ui.label(
            RichText::new(format!("♫ {1} - {0}", song.track_name, song.artist_name))
                .size(11.0)
                .color(Color32::from_gray(180)),
        );

        let progress_ms = self.currently_playing.as_ref().map_or(0, |p| p.progress_ms);
        let current_ms = progress_ms as u128
            + self.currently_playing.as_ref().map_or(0, |c| {
                if c.is_playing {
                    self.time_of_last_req.elapsed().as_millis()
                } else {
                    0
                }
            });

        let current_index = match song
            .lyrics
            .find_current_index(current_ms.try_into().unwrap())
        {
            LyricPosition::BeforeStart => 0,
            LyricPosition::Line(n) => n,
            LyricPosition::AfterEnd(n) => n,
        };
        let synced_lyrics = &song.lyrics.synced_lyrics;

        // Raw progress (0..1 within current line's duration)
        let raw_progress = if current_index + 1 < synced_lyrics.len() {
            let t0 = synced_lyrics[current_index].time_ms as i64;
            let t1 = synced_lyrics[current_index + 1].time_ms as i64;
            let elapsed = current_ms as i64 - t0;
            let duration = t1 - t0;
            if duration > 0 {
                (elapsed as f32 / duration as f32).clamp(0.0, 1.0)
            } else {
                0.0
            }
        } else {
            0.0
        };

        let target_line = if self.settings_cache.scroll_smoothly {
            current_index as f32 + cubic_ease_in_out(raw_progress)
        } else {
            current_index as f32
        };

        let available_height = ui.available_height();
        let center_bias = available_height * 0.25 * 0.5;
        // 0 is bottom, 0.25 is almost off screen, 0.25*0.5 is just above center.

        let scroll_y = {
            let line_floor = target_line.floor() as usize;
            let line_frac = target_line.fract();
            let y_floor = self
                .line_top_offsets
                .get(line_floor)
                .copied()
                .unwrap_or(0.0);
            let y_ceil = self
                .line_top_offsets
                .get(line_floor + 1)
                .copied()
                .unwrap_or(y_floor);

            // Interpolate between the two neighbouring line positions.
            let y_exact = y_floor + (y_ceil - y_floor) * line_frac;
            (y_exact - center_bias).max(0.0)
        };

        if self.settings_cache.draw_debug_stuff {
            ui.label(format!("target_line: {:.3}", target_line));
            ui.label(format!("scroll_y: {:.1}", scroll_y));
            ui.label(format!("current_ms: {}", current_ms));
        }

        let mut new_offsets: Vec<f32> = Vec::with_capacity(synced_lyrics.len());
        ScrollArea::vertical()
            .id_salt("lyrics_scroll")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .vertical_scroll_offset(scroll_y)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(center_bias);

                ui.with_layout(Layout::top_down(Align::Center), |ui| {
                    for (i, line) in synced_lyrics.iter().enumerate() {
                        let top_y = ui.cursor().top() - ui.min_rect().top() - center_bias;
                        new_offsets.push(top_y);

                        let dist = (i as f32 - target_line).abs();
                        let alpha_f = 0.20 + 0.80 * (1.0 - (dist / 3.5).clamp(0.0, 1.0)).powi(2);
                        let alpha = (alpha_f * 255.0) as u8;

                        let signed = i as f32 - target_line;
                        let past_color = [200u8, 180, 255];
                        let current_color = [255u8, 255, 255];
                        let future_color = [180u8, 210, 255];

                        let (r, g, b) = if signed < 0.0 {
                            let t = cubic_ease_in_out((-signed).min(1.0));
                            lerp_color(current_color, past_color, t)
                        } else {
                            let t = cubic_ease_in_out(signed.min(1.0));
                            lerp_color(current_color, future_color, t)
                        };

                        let color = Color32::from_rgba_unmultiplied(r, g, b, alpha);
                        let label_resp = ui.label(
                            RichText::new(&line.text)
                                .size(self.settings_cache.font_size)
                                .color(color)
                                .strong(),
                        );

                        if i == current_index
                            && self.settings_cache.progress_bar_position
                                == ProgressBarPosition::BelowCurrentLine
                        {
                            ui.add_space(2.0);
                            let bar_width = label_resp.rect.width();
                            draw_progress_bar(ui, raw_progress, bar_width);
                            ui.add_space(2.0);
                        }

                        ui.add_space(self.settings_cache.line_spacing);
                    }
                });
            });

        self.line_top_offsets = new_offsets;

        if self.settings_cache.progress_bar_position == ProgressBarPosition::Bottom {
            draw_progress_bar(ui, raw_progress, ui.available_width());
        }
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

/// Helper for nearly lerping between two colors
fn lerp_color(a: [u8; 3], b: [u8; 3], t: f32) -> (u8, u8, u8) {
    let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    (l(a[0], b[0]), l(a[1], b[1]), l(a[2], b[2]))
}

/// Draw progress
fn draw_progress_bar(ui: &mut Ui, progress: f32, width: f32) {
    let height = 2.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let filled_width = rect.width() * progress.clamp(0.0, 1.0);
    let filled_rect = Rect::from_min_size(rect.left_top(), Vec2::new(filled_width, height));
    // Dim background track
    ui.painter()
        .rect_filled(rect, 0.0, Color32::from_white_alpha(30));
    // Bright filled portion
    ui.painter()
        .rect_filled(filled_rect, 0.0, Color32::from_white_alpha(200));
}
