use egui::{Align, Color32, Layout, Rect, RichText, Sense, Ui, Vec2};

use crate::{lyrics_parser::LyricPosition, overlay::LyricsAppUI, settings::ProgressBarPosition};

/// Smooth ease-in-out (cubic)
fn ease_in_out(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
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
            RichText::new(format!("♫ {0}", song.track_name))
                .size(11.0)
                .color(Color32::from_gray(180)),
        );

        //TODO: Currently biggest issue for smoothness: Jumping on loading or unloading lyrics
        //      Extra problematic when line is wrapped to next like
        //      Also problematic when new lines appear (e.g. first 3 lines)

        // TODO: Fade in and not just fade out lines

        //TODO: "Bottom" progress bar can clip off of screen

        let row_height = &self.settings_cache.font_size + &self.settings_cache.line_spacing;

        let progress_ms = self.currently_playing.as_ref().map_or(0, |p| p.progress_ms);

        let current_ms = progress_ms as u128 + self.time_of_last_req.elapsed().as_millis();
        let current_index = match song
            .lyrics
            .find_current_index(current_ms.try_into().unwrap())
        {
            LyricPosition::BeforeStart => 0,
            LyricPosition::Line(n) => n,
            LyricPosition::AfterEnd(n) => n,
        };
        let synced_lyrics = &song.lyrics.synced_lyrics;

        // Progress from current line to the next.
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
            // Last line — no next line to interpolate toward,
            0.0
        };

        let anim_progress = if self.settings_cache.scroll_smoothly {
            raw_progress
        } else {
            0.0
        };
        let eased = ease_in_out(anim_progress);
        let effective_ci = current_index as f32 + eased;

        let start = current_index.saturating_sub(2);
        let end = (current_index + 2).min(synced_lyrics.len() - 1);

        if self.settings_cache.draw_debug_stuff {
            ui.label(format!(
                "progress vs eased: {:.2}, {:.2}",
                anim_progress, eased
            ));
            ui.label(format!("effective_current index: {:.2}", effective_ci));
            ui.label(format!("current_ms: {:.2}", current_ms));
        }

        let slide_frac = effective_ci - effective_ci.floor();
        let slide_offset = slide_frac * row_height; // pixels to shift up

        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            let center_offset = ui.available_height() * 0.25 - row_height * 0.5;
            ui.add_space(center_offset - slide_offset); // subtract to slide up
            for i in start..=end {
                let line = &synced_lyrics[i];

                // Alpha falls off with distance
                let edge_dist = ((i as f32 - effective_ci).abs() - 2.0).max(0.0);
                let edge_fade = (1.0 - edge_dist).clamp(0.0, 1.0);
                let alpha = (ease_in_out(edge_fade) * 255.0) as u8;

                // Color: blend between past/present/future tints based on signed distance
                let past_color = [200u8, 180, 255]; // purple tint
                let current_color = [255u8, 255, 255]; // white
                let future_color = [180u8, 210, 255]; // blue tint

                let signed = i as f32 - effective_ci;
                let (r, g, b) = if signed < 0.0 {
                    // Between past and current
                    let t = (-signed).min(1.0);
                    lerp_color(current_color, past_color, t)
                } else {
                    // Between current and future
                    let t = signed.min(1.0);
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
