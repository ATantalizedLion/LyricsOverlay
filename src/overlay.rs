use std::time::Instant;

use egui::{Color32, RichText, Ui};
use tokio::sync::mpsc;
use tracing::trace;

use crate::{
    MessageToRT, MessageToUI,
    lyrics_fetch::{LyricsRequestInfo, SongWithLyrics},
    lyrics_parser::LyricPosition,
    spotify::CurrentlyPlayingResponse,
};

pub struct LyricsAppUI {
    is_auth: bool,
    tx: mpsc::Sender<MessageToRT>,
    rx: mpsc::Receiver<MessageToUI>,
    error_string: Option<String>,
    currently_playing: Option<CurrentlyPlayingResponse>,
    current_song_with_lyrics: Option<SongWithLyrics>,
    time_of_last_req: Instant,
}

//TODO: Better scrolling, need to always show 2 upcoming lines, current line and past line. this means our UI has a fixed size we can grab from the settings (from font size maybe? ).
impl LyricsAppUI {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        tx: mpsc::Sender<MessageToRT>,
        rx: mpsc::Receiver<MessageToUI>,
    ) -> Self {
        Self {
            is_auth: false,
            tx,
            rx,
            currently_playing: None,
            error_string: None,
            time_of_last_req: Instant::now(),
            current_song_with_lyrics: None,
        }
    }

    fn message_loop(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                MessageToUI::Authenticated => {
                    self.is_auth = true;
                    self.tx.try_send(MessageToRT::GetCurrentTrack).unwrap();
                }
                MessageToUI::CurrentlyPlaying(data) => {
                    self.currently_playing = Some(data);
                    self.time_of_last_req = Instant::now();

                    self.tx
                        .try_send(MessageToRT::GetLyrics(
                            LyricsRequestInfo::from_spotify_response(
                                &self.currently_playing.clone().unwrap(),
                            )
                            .unwrap(),
                        ))
                        .unwrap();
                }
                MessageToUI::DisplayError(err) => self.error_string = Some(err),
                MessageToUI::GotLyrics(song) => {
                    trace!("Received SongWithLyrics!: {:?}", song);
                    self.current_song_with_lyrics = Some(song);
                }
            }
        }
    }

    fn authentication_ui(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.label(
                RichText::new("♫ Lyrics Overlay")
                    .size(22.0)
                    .color(Color32::WHITE),
            );
            ui.add_space(12.0);
            if ui.button("Connect Spotify").clicked() {
                self.tx.try_send(MessageToRT::Authenticate).unwrap();
            }
        });
    }

    fn waiting_for_lyrics(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 2.0 - 20.0);
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

    fn display_lyrics(&self, ui: &mut Ui, song: &SongWithLyrics) {
        let progress_ms = self.currently_playing.as_ref().map_or(0, |p| p.progress_ms);
        #[allow(clippy::cast_possible_truncation)]
        let elapsed = self.time_of_last_req.elapsed().as_millis() as u64;
        let current_ms = progress_ms as u64 + elapsed;
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
        #[allow(clippy::cast_precision_loss)]
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
}

impl eframe::App for LyricsAppUI {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint();

        self.message_loop();

        // Fully transparent outer frame
        let frame = egui::Frame::new()
            .fill(Color32::from_rgba_unmultiplied(0, 0, 0, 180))
            .inner_margin(egui::Margin::symmetric(24, 16));

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                // Allow dragging
                let drag_response =
                    ui.interact(ui.clip_rect(), ui.id().with("drag"), egui::Sense::drag());
                if drag_response.dragged() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                frame.show(ui, |ui| {
                    if !self.is_auth {
                        self.authentication_ui(ui);
                        return;
                    }

                    if let Some(song) = &self.current_song_with_lyrics {
                        self.display_lyrics(ui, song);
                    } else {
                        self.waiting_for_lyrics(ui);
                    }

                    if let Some(err) = &self.error_string {
                        ui.label(
                            RichText::new(err)
                                .color(Color32::from_rgb(255, 80, 80))
                                .size(12.0),
                        );
                    }
                });
            });
    }
}
