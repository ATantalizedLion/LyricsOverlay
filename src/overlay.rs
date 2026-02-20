use std::time::Instant;

use tokio::sync::mpsc;

use crate::{MessageToRT, MessageToUI, spotify::CurrentlyPlayingResponse};

pub struct LyricsAppUI {
    is_auth: bool,
    tx: mpsc::Sender<MessageToRT>,
    rx: mpsc::Receiver<MessageToUI>,
    currently_playing: Option<CurrentlyPlayingResponse>,
    time_of_last_currently_playing_request: Option<Instant>,
}
impl LyricsAppUI {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        tx: mpsc::Sender<MessageToRT>,
        rx: mpsc::Receiver<MessageToUI>,
    ) -> Self {
        Self {
            is_auth: false,
            tx,
            rx,
            currently_playing: None,
            time_of_last_currently_playing_request: None,
        }
    }
}

impl eframe::App for LyricsAppUI {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        ctx.request_repaint();

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
        });
    }
}
