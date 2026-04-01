use std::{sync::Arc, time::Instant};

use egui::{Color32, RichText, Ui};
use tokio::sync::mpsc;
use tracing::trace;

use tokio::sync::RwLock as TokioRwLock;

use crate::{
    MessageToRT, MessageToUI,
    lyrics_fetch::{LyricsRequestInfo, SongWithLyrics},
    overlay::resize::handle_resize,
    settings::Settings,
    spotify::CurrentlyPlayingResponse,
};

mod authentication_ui;
mod lyrics_ui;
mod resize;
mod settings_panel;

pub struct LyricsAppUI {
    /// Are we currently authenticated with spotify
    is_auth: bool,
    /// Transimitter of communication between the UI and the runtime
    tx: mpsc::Sender<MessageToRT>,
    /// Receiver of communication between the runtimme and the UI
    rx: mpsc::Receiver<MessageToUI>,
    /// If this contains something, we display it so the user knows what's going on
    error_string: Option<String>,
    /// The response to spotify's current lyrics
    currently_playing: Option<CurrentlyPlayingResponse>,

    /// Container for the current song's lyrics
    current_song_with_lyrics: Option<SongWithLyrics>,
    /// Time at which the last spotify request was received
    time_of_last_req: Instant,

    /// The `RWLock` for our setting struct
    settings: Arc<TokioRwLock<Settings>>,
    /// Cached settings to prevent hanging on blocking locks
    settings_cache: Settings,
    /// Is the settings window currenly open
    settings_open: bool,

    /// measured y of each line, updated every frame
    line_top_offsets: Vec<f32>,
}

impl LyricsAppUI {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        tx: mpsc::Sender<MessageToRT>,
        rx: mpsc::Receiver<MessageToUI>,
        settings: &Arc<TokioRwLock<Settings>>,
    ) -> Self {
        Self {
            is_auth: false,
            tx,
            rx,
            currently_playing: None,
            error_string: None,
            time_of_last_req: Instant::now(),
            current_song_with_lyrics: None,
            settings: settings.clone(),
            settings_cache: settings.blocking_read().clone(),
            settings_open: false,
            line_top_offsets: vec![],
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

                    self.currently_playing = Some(data);
                    // TODO: Also consider the time between request sent from spotify and the receiving of the request,
                    // there's something about this in the spotify API docs
                    self.time_of_last_req = Instant::now();

                    if !same_track {
                        self.tx
                            .try_send(MessageToRT::GetLyrics(
                                LyricsRequestInfo::from_spotify_response(
                                    &self.currently_playing.clone().unwrap(),
                                )
                                .unwrap(),
                            ))
                            .unwrap();
                        self.line_top_offsets.clear();
                    }
                }
                MessageToUI::DisplayError(err) => self.error_string = Some(err),
                MessageToUI::GotLyrics(song) => {
                    trace!("Received SongWithLyrics!: {:?}", song);
                    self.current_song_with_lyrics = Some(song);
                }
                MessageToUI::NotCurrentlyPlaying(reason) => {
                    self.error_string = Some(format!("No track found! ({reason})"));
                }
                MessageToUI::RateLimitsExceeded => {
                    self.error_string = Some("Rate limits exceeded!".to_string());
                }
            }
        }
    }
}

impl eframe::App for LyricsAppUI {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint();

        ctx.set_visuals(egui::Visuals {
            panel_fill: Color32::TRANSPARENT,
            window_fill: Color32::TRANSPARENT,
            ..egui::Visuals::dark()
        });

        handle_resize(ctx, 6.0f32);

        let full_width = ctx.available_rect().width();
        let full_height = ctx.available_rect().height();

        // Stop font from being shifted for font alignment
        ctx.tessellation_options_mut(|opts| {
            opts.round_text_to_pixels = false;
        });

        // Cache settings if not locked.
        if let Ok(s) = self.settings.try_read() {
            self.settings_cache = s.clone();
        }

        self.message_loop();

        // Exit button
        egui::Area::new("exit".into())
            .fixed_pos(egui::pos2(full_width - 25., 10.))
            .show(ctx, |ui| {
                let label = "X";
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(label)
                                .size(14.0)
                                .color(Color32::from_gray(160)),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });

        // Settings button
        egui::Area::new("settings_overlay".into())
            .fixed_pos(egui::pos2(full_width - 45., 10.))
            .show(ctx, |ui| {
                self.settings_ui(ui, ctx);
            });

        // Transparent outer frame, we use this for allowing dragging and resizing
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
                frame.show(ui, |ui: &mut Ui| {
                    // Show either the authenticate button or lyrics
                    if self.is_auth {
                        // Lyrics or "waiting for lyrics"
                        self.display_lyrics(ui);
                    } else {
                        self.authentication_ui(ui);
                    }
                });
            });

        egui::Area::new("error bar".into())
            .fixed_pos(egui::pos2(0., full_height - 20.))
            .show(ctx, |ui| {
                ui.set_min_width(full_width);
                ui.set_max_width(full_width);
                // Last received error information
                if let Some(err) = self.error_string.clone() {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(err)
                                .color(Color32::from_rgb(255, 80, 80))
                                .size(12.0),
                        );

                        if ui.button("Clear Error").clicked() {
                            self.error_string = None;
                        }
                    });
                }
            });
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, self.settings_cache.opacity]
    }
}
