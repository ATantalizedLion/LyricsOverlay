use egui::{Align, Color32, Layout, Rect, RichText, ScrollArea, Sense, Ui, Vec2};

use crate::{
    lyrics_fetch::SongWithLyrics,
    lyrics_parser::{LyricLine, LyricPosition},
    overlay::LyricsAppUI,
    settings::{EasingModes, ProgressBarPosition},
    spotify::CurrentlyPlayingResponse,
};
fn ease_in_out(t: f32, mode: EasingModes) -> f32 {
    match mode {
        EasingModes::Cubic => t * t * (3.0 - 2.0 * t),
        EasingModes::Linear => t,
    }
}

#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_possible_wrap)]
#[allow(clippy::cast_precision_loss)]
#[allow(clippy::cast_sign_loss)]
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
                .and_then(CurrentlyPlayingResponse::get_track_title)
        {
            self.waiting_for_lyrics(ui);
            return;
        }

        draw_song_header(ui, song);

        if song.lyrics.synced_lyrics.is_empty() {
            if song.lyrics.plain_lyrics.is_empty() {
                self.waiting_for_lyrics(ui);
            } else {
                self.draw_plain_lyrics(ui, &song.lyrics.plain_lyrics);
            }
            return;
        }

        let current_ms = self.current_playback_ms();
        let synced_lyrics = &song.lyrics.synced_lyrics;
        let song_end_ms = (song.duration_sec * 1000.) as i64;
        let song_progress = current_ms as f32 / song_end_ms as f32;

        let position = song
            .lyrics
            .find_current_index(current_ms.try_into().unwrap());
        let (t0, t1, current_index) = line_bounds(position, synced_lyrics, song_end_ms);
        let (raw_progress, target_line) =
            self.compute_target_line(position, current_ms, t0, t1, current_index);

        let available_height = ui.available_height();
        let (scroll_y, center_bias) = self.compute_scroll_offset(target_line, available_height);

        let new_offsets = self.draw_lyric_lines(
            ui,
            synced_lyrics,
            scroll_y,
            center_bias,
            target_line,
            current_index,
            raw_progress,
            song_progress,
        );
        self.line_top_offsets = new_offsets;

        if self.settings_cache.line_progress_bar_position == ProgressBarPosition::Bottom {
            draw_progress_bar(ui, raw_progress, ui.available_width());
        }
        if self.settings_cache.song_progress_bar_position == ProgressBarPosition::Bottom {
            draw_progress_bar(ui, song_progress, ui.available_width());
        }
    }

    /// Estimated current playback position, extrapolated from the last poll response.
    fn current_playback_ms(&self) -> u128 {
        let Some(playing) = self.currently_playing.as_ref() else {
            return 0;
        };
        let elapsed = if playing.is_playing {
            self.time_of_last_req.elapsed().as_millis()
        } else {
            0
        };
        playing.progress_ms as u128 + elapsed
    }

    /// Where the scroll/color transition should be, and how far through the
    /// current line's own progress bar we are.
    fn compute_target_line(
        &self,
        position: LyricPosition,
        current_ms: u128,
        t0: i64,
        t1: i64,
        current_index: usize,
    ) -> (f32, f32) {
        let raw_progress = if t1 - t0 > 0 {
            ((current_ms as i64 - t0) as f32 / (t1 - t0) as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };

        // Snap into place over `line_transition_ms`, then hold, rather than
        // smearing the transition across the whole (possibly long) gap to the next line.
        let transition_progress = {
            let span = (t1 - t0)
                .min(self.settings_cache.line_transition_ms as i64)
                .max(1);
            ((current_ms as i64 - t0) as f32 / span as f32).clamp(0.0, 1.0)
        };

        let target_line = if self.settings_cache.scroll_smoothly {
            match position {
                // Nothing to transition from yet, hold off-screen until the first line's
                // own transition (handled below) eases it into view.
                LyricPosition::BeforeStart => -1.0,
                _ => {
                    current_index as f32 - 1.0
                        + ease_in_out(transition_progress, self.settings_cache.ease_position)
                }
            }
        } else {
            current_index as f32
        };

        (raw_progress, target_line)
    }

    /// Vertical scroll offset that centers `target_line`, plus the bias used to
    /// keep the current line just above dead-center.
    fn compute_scroll_offset(&self, target_line: f32, available_height: f32) -> (f32, f32) {
        // 0 is bottom, 0.25 is almost off screen, 0.25*0.5 is just above center.
        let center_bias = available_height * 0.25 * 0.5;

        let line_floor = target_line.floor() as usize;
        let line_frac = target_line.fract();
        let y_floor = self
            .line_top_offsets
            .get(line_floor)
            .copied()
            .unwrap_or_else(|| self.line_top_offsets.last().copied().unwrap_or(0.0));
        let y_ceil = self
            .line_top_offsets
            .get(line_floor + 1)
            .copied()
            .unwrap_or(y_floor);

        // Interpolate between the two neighbouring line positions.
        let y_exact = y_floor + (y_ceil - y_floor) * line_frac;
        let scroll_y = (y_exact - center_bias).max(0.0);

        (scroll_y, center_bias)
    }

    /// Draws the scrolling lyric lines and returns each line's measured top offset,
    /// to be cached for next frame's scroll-position interpolation.
    #[allow(clippy::too_many_arguments)]
    fn draw_lyric_lines(
        &self,
        ui: &mut Ui,
        synced_lyrics: &[LyricLine],
        scroll_y: f32,
        center_bias: f32,
        target_line: f32,
        current_index: usize,
        raw_progress: f32,
        song_progress: f32,
    ) -> Vec<f32> {
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
                        let past_color = self.settings_cache.past_line_color;
                        let current_color = self.settings_cache.current_line_color;
                        let future_color = self.settings_cache.future_line_color;

                        let (r, g, b) = if signed < 0.0 {
                            let t = ease_in_out((-signed).min(1.0), self.settings_cache.ease_color);
                            lerp_color(current_color, past_color, t)
                        } else {
                            let t = ease_in_out(signed.min(1.0), self.settings_cache.ease_color);
                            lerp_color(current_color, future_color, t)
                        };

                        let color = Color32::from_rgba_unmultiplied(r, g, b, alpha);
                        let label_resp = ui.label(
                            RichText::new(&line.text)
                                .size(self.settings_cache.font_size)
                                .color(color)
                                .strong(),
                        );

                        if i == current_index {
                            let bar_width = label_resp.rect.width();
                            if self.settings_cache.line_progress_bar_position
                                == ProgressBarPosition::BelowCurrentLine
                            {
                                ui.add_space(2.0);
                                draw_progress_bar(ui, raw_progress, bar_width);
                                ui.add_space(2.0);
                            }
                            if self.settings_cache.song_progress_bar_position
                                == ProgressBarPosition::BelowCurrentLine
                            {
                                ui.add_space(2.0);
                                draw_progress_bar(ui, song_progress, bar_width);
                                ui.add_space(2.0);
                            }
                        }

                        ui.add_space(self.settings_cache.line_spacing);
                    }
                });
            });

        new_offsets
    }

    fn waiting_for_lyrics(&mut self, ui: &mut Ui) {
        let Some(playing) = &self.currently_playing else {
            Self::nothing_playing(ui);
            return;
        };

        ui.vertical_centered(|ui| {
            if let Some(title) = playing.get_track_title() {
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

    fn nothing_playing(ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() / 2.0 - 24.0).max(0.0));
            ui.label(RichText::new("♫").size(26.0).color(Color32::from_gray(70)));
            ui.add_space(4.0);
            ui.label(
                RichText::new("Nothing playing")
                    .size(14.0)
                    .color(Color32::from_gray(110)),
            );
        });
    }

    /// No line-level timing is available for this song, so just show the full text and
    /// let the user scroll manually instead of pretending we can track their position.
    fn draw_plain_lyrics(&self, ui: &mut Ui, lines: &[String]) {
        ui.label(
            RichText::new("Lyrics aren't synced to playback")
                .size(11.0)
                .color(Color32::from_gray(120)),
        );
        ui.add_space(4.0);

        ScrollArea::vertical()
            .id_salt("plain_lyrics_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.with_layout(Layout::top_down(Align::Center), |ui| {
                    for line in lines {
                        ui.label(
                            RichText::new(line)
                                .size(self.settings_cache.font_size)
                                .color(Color32::from_gray(210)),
                        );
                        ui.add_space(self.settings_cache.line_spacing);
                    }
                });
            });
    }
}

fn draw_song_header(ui: &mut Ui, song: &SongWithLyrics) {
    ui.label(
        RichText::new(format!("♫ {} - {}", song.artist_name, song.track_name))
            .size(11.0)
            .color(Color32::from_gray(180)),
    );
}

/// Start/end time (ms) of the line `position` refers to, and its line index.
#[allow(clippy::cast_possible_wrap)]
fn line_bounds(
    position: LyricPosition,
    synced_lyrics: &[LyricLine],
    song_end_ms: i64,
) -> (i64, i64, usize) {
    match position {
        LyricPosition::BeforeStart => (
            0,
            synced_lyrics
                .first()
                .map_or(song_end_ms, |l| l.time_ms as i64),
            0,
        ),
        LyricPosition::Line(n) => (
            synced_lyrics[n].time_ms as i64,
            synced_lyrics
                .get(n + 1)
                .map_or(song_end_ms, |l| l.time_ms as i64),
            n,
        ),
        LyricPosition::AfterEnd(n) => (synced_lyrics[n - 1].time_ms as i64, song_end_ms, n),
    }
}

/// Helper for nearly lerping between two colors
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_sign_loss)]
fn lerp_color(a: [u8; 3], b: [u8; 3], t: f32) -> (u8, u8, u8) {
    let l = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t) as u8;
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
