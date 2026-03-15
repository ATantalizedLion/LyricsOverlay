use std::{sync::Arc, time::Instant};

use egui::{Color32, RichText, Ui};
use tokio::sync::mpsc;
use tracing::trace;

use tokio::sync::RwLock as TokioRwLock;

mod lyrics_ui;
mod settings_panel;

use crate::{
    MessageToRT, MessageToUI,
    lyrics_fetch::{LyricsRequestInfo, SongWithLyrics},
    settings::Settings,
    spotify::CurrentlyPlayingResponse,
};

pub struct LyricsAppUI {
    is_auth: bool,
    tx: mpsc::Sender<MessageToRT>,
    rx: mpsc::Receiver<MessageToUI>,
    error_string: Option<String>,
    currently_playing: Option<CurrentlyPlayingResponse>,

    playback_state: bool,
    current_song_with_lyrics: Option<SongWithLyrics>,
    time_of_last_req: Instant,
    time_of_last_frame: Instant,
    ms_played_since_last_update: u128,

    settings: Arc<TokioRwLock<Settings>>,
    settings_open: bool,
}

impl LyricsAppUI {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        tx: mpsc::Sender<MessageToRT>,
        rx: mpsc::Receiver<MessageToUI>,
        settings: Arc<TokioRwLock<Settings>>,
    ) -> Self {
        Self {
            is_auth: false,
            tx,
            rx,
            currently_playing: None,
            error_string: None,
            time_of_last_req: Instant::now(),
            time_of_last_frame: Instant::now(),
            current_song_with_lyrics: None,
            ms_played_since_last_update: 0,
            settings,
            settings_open: false,
            playback_state: false,
        }
    }

    fn message_loop(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                MessageToUI::AuthenticationStateUpdate(new_state) => {
                    self.is_auth = new_state;
                    if new_state {
                        self.tx.try_send(MessageToRT::GetCurrentTrack).unwrap();
                    }
                    /*else {
                        self.error_string =
                            Some("Authentication expired, please reauthenticate".into())
                    }*/
                }

                MessageToUI::CurrentlyPlaying(data) => {
                    let same_track = &self
                        .currently_playing
                        .take()
                        .is_some_and(|s| s.get_spotify_id() == data.get_spotify_id());
                    self.playback_state = data.is_playing.clone();

                    self.currently_playing = Some(data);
                    // TODO: Also consider the time between request sent from spotify and the reeceing of the request
                    self.time_of_last_req = Instant::now();
                    self.ms_played_since_last_update = 0;

                    if !same_track {
                        self.tx
                            .try_send(MessageToRT::GetLyrics(
                                LyricsRequestInfo::from_spotify_response(
                                    &self.currently_playing.clone().unwrap(),
                                )
                                .unwrap(),
                            ))
                            .unwrap();
                    }
                }
                MessageToUI::DisplayError(err) => self.error_string = Some(err),
                MessageToUI::GotLyrics(song) => {
                    trace!("Received SongWithLyrics!: {:?}", song);
                    self.current_song_with_lyrics = Some(song);
                }
                MessageToUI::NotCurrentlyPlaying(reason) => {
                    self.error_string = Some(format!("No track found! ({reason})"))
                }
            }
        }
    }

    fn authentication_ui(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 2.0 + 22.0);
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
}

impl eframe::App for LyricsAppUI {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint();

        self.message_loop();

        // Transparent outer frame
        let frame = egui::Frame::new()
            .fill(Color32::from_rgba_unmultiplied(0, 0, 0, 0))
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

                // Render stuff :)
                frame.show(ui, |ui| {
                    // Settings foldout
                    self.settings_ui(ui);

                    // Show either the authenticate button or lyrics
                    let auth = self.is_auth.clone();
                    if !auth {
                        self.authentication_ui(ui);
                    } else {
                        // Lyrics or "waiting for lyrics"
                        self.display_lyrics(ui);
                    }

                    // Last received error information
                    if let Some(err) = &self.error_string {
                        ui.label(
                            RichText::new(err)
                                .color(Color32::from_rgb(255, 80, 80))
                                .size(12.0),
                        );

                        if ui.button("Clear Error").clicked() {
                            self.error_string = None;
                        }
                    }
                });
            });

        self.time_of_last_frame = Instant::now();
    }
}
