use egui::{Color32, Context, RichText, Ui};

use crate::{MessageToRT, overlay::LyricsAppUI, settings::MediaControlsPosition};

impl LyricsAppUI {
    pub(super) fn media_controls_ui(&mut self, ctx: &Context) {
        let Some((pos, _is_top)) = self.media_controls_layout(ctx) else {
            return;
        };

        egui::Area::new("media_controls".into())
            .fixed_pos(pos)
            .show(ctx, |ui| self.media_controls_row(ui));
    }

    /// Fixed position of the media controls row and whether it's anchored to the top,
    /// if the controls are currently visible (accounts for `TopHover`/`BottomHover`
    /// needing an active pointer hover).
    fn media_controls_layout(&self, ctx: &Context) -> Option<(egui::Pos2, bool)> {
        let position = self.settings_cache.media_controls_position;
        if position == MediaControlsPosition::Hidden {
            return None;
        }

        let hover_only = matches!(
            position,
            MediaControlsPosition::TopHover | MediaControlsPosition::BottomHover
        );
        if hover_only && ctx.input(|i| i.pointer.hover_pos()).is_none() {
            return None;
        }

        let is_top = matches!(
            position,
            MediaControlsPosition::Top | MediaControlsPosition::TopHover
        );

        let full_width = ctx.available_rect().width();
        let full_height = ctx.available_rect().height();
        let row_width = 84.0;

        let pos = if is_top {
            egui::pos2(full_width / 2.0 - row_width / 2.0, 16.0)
        } else {
            egui::pos2(full_width / 2.0 - row_width / 2.0, full_height - 34.0)
        };

        Some((pos, is_top))
    }

    /// X coordinate (in window space) where the media controls start, if they're
    /// currently visible in the top row - i.e. where the song title needs to stop to
    /// avoid overlapping them.
    pub(super) fn top_controls_start_x(&self, ctx: &Context) -> Option<f32> {
        let (pos, is_top) = self.media_controls_layout(ctx)?;
        is_top.then_some(pos.x)
    }

    fn media_controls_row(&mut self, ui: &mut Ui) {
        let accent = super::accent_color(self.settings_cache.current_line_color);

        ui.horizontal(|ui| {
            if control_button(ui, "⏮", accent).clicked() {
                let _ = self.tx.try_send(MessageToRT::PreviousTrack);
            }

            let is_playing = self
                .currently_playing
                .as_ref()
                .is_some_and(|playing| playing.is_playing);
            if control_button(ui, if is_playing { "⏸" } else { "▶" }, accent).clicked() {
                let msg = if is_playing {
                    MessageToRT::Pause
                } else {
                    MessageToRT::Play
                };
                let _ = self.tx.try_send(msg);
            }

            if control_button(ui, "⏭", accent).clicked() {
                let _ = self.tx.try_send(MessageToRT::NextTrack);
            }
        });
    }
}

fn control_button(ui: &mut Ui, label: &str, color: Color32) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(15.0).color(color))
            .frame(true)
            .frame_when_inactive(false),
    )
}
