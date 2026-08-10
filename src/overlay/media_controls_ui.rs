use egui::{Color32, Context, RichText, Ui};

use crate::{MessageToRT, overlay::LyricsAppUI, settings::MediaControlsPosition};

impl LyricsAppUI {
    pub(super) fn media_controls_ui(&mut self, ctx: &Context) {
        let position = self.settings_cache.media_controls_position;
        if position == MediaControlsPosition::Hidden {
            return;
        }

        let hover_only = matches!(
            position,
            MediaControlsPosition::TopHover | MediaControlsPosition::BottomHover
        );
        if hover_only && ctx.input(|i| i.pointer.hover_pos()).is_none() {
            return;
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

        egui::Area::new("media_controls".into())
            .fixed_pos(pos)
            .show(ctx, |ui| self.media_controls_row(ui));
    }

    fn media_controls_row(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if control_button(ui, "⏮").clicked() {
                let _ = self.tx.try_send(MessageToRT::PreviousTrack);
            }

            let is_playing = self
                .currently_playing
                .as_ref()
                .is_some_and(|playing| playing.is_playing);
            if control_button(ui, if is_playing { "⏸" } else { "▶" }).clicked() {
                let msg = if is_playing {
                    MessageToRT::Pause
                } else {
                    MessageToRT::Play
                };
                let _ = self.tx.try_send(msg);
            }

            if control_button(ui, "⏭").clicked() {
                let _ = self.tx.try_send(MessageToRT::NextTrack);
            }
        });
    }
}

fn control_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(15.0).color(Color32::from_gray(190)))
            .frame(true)
            .frame_when_inactive(false),
    )
}
