use std::time::Instant;

use egui::{Color32, RichText};
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
    time_of_last_currently_playing_request: Instant,
}

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
            time_of_last_currently_playing_request: Instant::now(),
            current_song_with_lyrics: None,
        }
    }
}

impl eframe::App for LyricsAppUI {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint();

        while let Ok(message) = self.rx.try_recv() {
            match message {
                MessageToUI::Authenticated => {
                    self.is_auth = true;
                    self.tx.try_send(MessageToRT::GetCurrentTrack).unwrap();
                }
                MessageToUI::CurrentlyPlaying(data) => {
                    self.currently_playing = Some(data);
                    self.time_of_last_currently_playing_request = Instant::now();

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

        let window = egui::Window::new("Spotify")
            .fixed_pos([20.0, 20.0])
            .default_width(400.0)
            .resizable(true)
            .collapsible(true);

        window.show(ctx, |ui| {
            if self.is_auth {
                ui.label("Spotify connected");

                // Display current song info
                if let Some(playing) = &self.currently_playing {
                    if let Some(title) = playing.get_track_title() {
                        ui.heading(title);
                    } else {
                        ui.label("No song currently playing");
                    }
                }
                ui.separator();
            } else {
                ui.heading("Spotify Lyrics Overlay");
                ui.separator();

                if ui.button("Authenticate with Spotify").clicked() {
                    self.tx.try_send(MessageToRT::Authenticate).unwrap();
                }

                ui.label("Click to connect your Spotify account");
            }
            if self.error_string.is_some() {
                ui.label(RichText::new(self.error_string.clone().unwrap()).color(Color32::RED));
            }
            if let Some(song) = &self.current_song_with_lyrics {
                let progress_ms = self.currently_playing.as_ref().map_or(0, |p| p.progress_ms);
                let current_pos = song.lyrics.find_current_index(
                    progress_ms
                        + self
                            .time_of_last_currently_playing_request
                            .elapsed()
                            .as_millis() as usize,
                );
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, line) in song.lyrics.synced_lyrics.iter().enumerate() {
                            let is_current =
                                matches!(current_pos, LyricPosition::Line(n) if n == i);
                            let text = if is_current {
                                RichText::new(&line.text)
                                    .color(Color32::WHITE)
                                    .strong()
                                    .size(18.0)
                            } else {
                                RichText::new(&line.text)
                                    .color(Color32::from_gray(120))
                                    .size(16.0)
                            };
                            ui.label(text);
                        }
                    });
            }
        });
    }
}
