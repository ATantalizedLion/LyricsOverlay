use egui::{Align, Color32, Layout, RichText, Ui};

use crate::{lyrics_parser::LyricPosition, overlay::LyricsAppUI};
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
        // TODO: Add title header for which song is currently playing

        // TODO: Progress bar towards next lyric, either at bottom or under current line
        // Get all relevant settings vars here:
        let binding = self.settings.blocking_read();
        let line_spacing = binding.line_spacing;
        let font_size = binding.font_size;
        let transition_ms = binding.line_transition_ms;
        let scroll_smoothly = binding.scroll_smoothly;
        drop(binding);

        let row_height = font_size + line_spacing;

        let progress_ms = self.currently_playing.as_ref().map_or(0, |p| p.progress_ms);

        if self.playback_state {
            // This is reset every time we receive a new currently playing
            self.ms_played_since_last_update += self.time_of_last_frame.elapsed().as_millis();
        }

        let current_ms = progress_ms as u128 + self.ms_played_since_last_update;
        let current_index = match song
            .lyrics
            .find_current_index(current_ms.try_into().unwrap())
        {
            LyricPosition::BeforeStart => 0,
            LyricPosition::Line(n) => n,
            LyricPosition::AfterEnd(n) => n,
        };
        let synced_lyrics = &song.lyrics.synced_lyrics;

        // TODO: Only start moving when within transition_ms of next_line
        // Current index is already in focus
        let progress = if scroll_smoothly && current_index + 1 < synced_lyrics.len() {
            let t0 = synced_lyrics[current_index].time_ms as i64;
            let t1 = synced_lyrics[current_index + 1].time_ms as i64;
            ui.label(format!("Timing: {t0}-{t1}"));

            let elapsed = current_ms as i64 - t0;
            let duration = t1 - t0;

            if duration > 0 {
                (elapsed as f32 / duration as f32).clamp(0.0, 1.0)
            } else {
                1.0
            }
        } else {
            // Last line — no next line to interpolate toward,
            // OR we have disabled scrolling "smoothly"
            0.0
        };
        let eased = ease_in_out(progress);
        let effective_ci = current_index as f32 + eased;

        let ci_rounded = effective_ci.round() as usize;
        let start = ci_rounded.saturating_sub(2);
        let end = (ci_rounded + 2).min(synced_lyrics.len() - 1);

        let base_size = font_size * 0.6;
        let highlight_size = font_size;

        ui.label(format!("progress vs eased: {:.2}, {:.2}", progress, eased));
        ui.label(format!("effective_current index: {:.2}", effective_ci));
        ui.label(format!("current_ms: {:.2}", current_ms));

        let slide_frac = effective_ci - effective_ci.floor();
        let slide_offset = slide_frac * row_height; // pixels to shift up

        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            let center_offset = ui.available_height() * 0.25 - row_height * 0.5;
            ui.add_space(center_offset - slide_offset); // subtract to slide up
            for i in start..=end {
                let line = &synced_lyrics[i];
                let dist = (i as f32 - effective_ci).abs();

                // Size falls off with distance based on setttings: each step away shrinks by a fixed ratio
                let size_range = highlight_size - base_size;
                let size = (highlight_size - dist * size_range * 0.5).max(base_size * 0.7);

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
                ui.label(RichText::new(&line.text).size(size).color(color).strong());
                ui.add_space(line_spacing);
            }
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

/// Helper for nearly lerping between two colors
fn lerp_color(a: [u8; 3], b: [u8; 3], t: f32) -> (u8, u8, u8) {
    let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    (l(a[0], b[0]), l(a[1], b[1]), l(a[2], b[2]))
}
