use std::time::Instant;

use egui::{Color32, RichText};
use tokio::sync::mpsc;
use tracing::trace;

use crate::{
    MessageToRT, MessageToUI,
    lyrics_fetch::{LyricsRequestInfo, SongWithLyrics},
    spotify::CurrentlyPlayingResponse,
};

pub struct LyricsAppUI {
    is_auth: bool,
    tx: mpsc::Sender<MessageToRT>,
    rx: mpsc::Receiver<MessageToUI>,
    error_string: Option<String>,
    currently_playing: Option<CurrentlyPlayingResponse>,
    current_song_with_lyrics: Option<SongWithLyrics>,
    time_of_last_currently_playing_request: Option<Instant>,
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
            time_of_last_currently_playing_request: None,
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

                    self.tx
                        .try_send(MessageToRT::GetLyrics(
                            LyricsRequestInfo::from_spotify_response(
                                self.currently_playing.clone().unwrap(),
                            )
                            .unwrap(),
                        ))
                        .unwrap();
                }
                MessageToUI::DisplayError(err) => self.error_string = Some(err),
                MessageToUI::GotLyrics(lyric_lines) => trace!("Received lyrics! {:?}", lyric_lines),
            }
        }

        let window = egui::Window::new("Spotify")
            .fixed_pos([20.0, 20.0])
            .default_width(400.0)
            .resizable(true)
            .collapsible(true);

        window.show(ctx, |ui| {
            if !self.is_auth {
                ui.heading("Spotify Lyrics Overlay");
                ui.separator();

                if ui.button("Authenticate with Spotify").clicked() {
                    self.tx.try_send(MessageToRT::Authenticate).unwrap();
                }

                ui.label("Click to connect your Spotify account");
            } else {
                ui.label("Spotify connected");

                // Display current song info
                if let Some(playing) = &self.currently_playing {
                    if let Some(title) = playing.get_track_title() {
                        ui.heading(title);
                    } else {
                        ui.label("No song currently playing");
                    }
                } else {
                    ui.label("Retreiving song");
                    let time_of_req = self
                        .time_of_last_currently_playing_request
                        .unwrap_or(Instant::now()); // If we cannot retreive, we return now so we don't end up spamming requests due to some error 
                    if time_of_req.elapsed().as_secs() > 5 {
                        self.tx.try_send(MessageToRT::GetCurrentTrack).unwrap();
                    }
                }
                ui.separator();
            }
            if self.error_string.is_some() {
                ui.label(RichText::new(self.error_string.clone().unwrap()).color(Color32::RED));
            }
            if self.current_song_with_lyrics.is_some() {
                let t = self.current_song_with_lyrics.as_ref().unwrap();
                ui.label(&t.lyrics.text_dump);
            }
        });
    }
}
