use std::time::Instant;

use crate::LyricsApp;

impl eframe::App for LyricsApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        ctx.request_repaint();

        let window = egui::Window::new("Spotify")
            .fixed_pos([20.0, 20.0])
            .default_width(400.0)
            .resizable(true)
            .collapsible(true);

        window.show(ctx, |ui| {
            let is_auth = self
                .is_authenticated
                .try_lock()
                .map(|guard| *guard)
                .unwrap_or(false);

            if !is_auth {
                ui.heading("Spotify Lyrics Overlay");
                ui.separator();

                if ui.button("Authenticate with Spotify").clicked() {
                    self.authenticate();
                }

                ui.label("Click to connect your Spotify account");
            } else {
                ui.label("Spotify connected");

                // Display current song info
                if let Ok(playing_guard) = self.currently_playing.try_lock() {
                    if let Some(playing) = playing_guard.clone() {
                        if let Some(title) = playing.get_track_title() {
                            ui.heading(title);
                        } else {
                            ui.label("No song currently playing");
                        }
                    } else {
                        ui.label("Retreiving song");
                        let time_of_req = self
                            .time_of_last_currently_playing_request
                            .try_lock()
                            .map(|guard| *guard)
                            .unwrap_or(Some(Instant::now())); // If we cannot retreive, we return now so we don't end up spamming requests due to some error 
                        if time_of_req.is_some_and(|f| f.elapsed().as_secs() > 5) {
                            self.get_current_track();
                        }
                    }
                }
                ui.separator();
            }
        });
    }
}
