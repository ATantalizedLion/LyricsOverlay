use std::{sync::Arc, time::Instant};

use egui::{
    Color32, RichText, Ui,
    epaint::text::{FontInsert, FontPriority, InsertFontFamily},
};
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

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
mod media_controls_ui;
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

    /// Current size of the main window, cached each frame so we can persist it on exit
    window_size: [f32; 2],
}

impl LyricsAppUI {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        tx: mpsc::Sender<MessageToRT>,
        rx: mpsc::Receiver<MessageToUI>,
        settings: &Arc<TokioRwLock<Settings>>,
    ) -> Self {
        install_cjk_fallback_font(&cc.egui_ctx);

        let settings_cache = settings.blocking_read().clone();
        let window_size = [settings_cache.window_width, settings_cache.window_height];

        Self {
            is_auth: false,
            tx,
            rx,
            currently_playing: None,
            error_string: None,
            time_of_last_req: Instant::now(),
            current_song_with_lyrics: None,
            settings: settings.clone(),
            settings_cache,
            settings_open: false,
            line_top_offsets: vec![],
            window_size,
        }
    }

    fn message_loop(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                MessageToUI::AuthenticationStateUpdate(new_state) => {
                    let was_authenticated = self.is_auth;
                    self.is_auth = new_state;
                    if new_state {
                        self.tx.try_send(MessageToRT::GetCurrentTrack).unwrap();
                    } else if was_authenticated {
                        // Only clear on the true->false transition, not every repeat of
                        // an already-false state: invalidate() itself re-sends this same
                        // message, and re-triggering here would ping-pong forever.
                        self.error_string =
                            Some("Spotify session expired, please reconnect".into());
                        let _ = self.tx.try_send(MessageToRT::InvalidateToken);
                    }
                }
                MessageToUI::CurrentlyPlaying(data) => {
                    let same_track = &self
                        .currently_playing
                        .take()
                        .is_some_and(|s| s.get_spotify_id() == data.get_spotify_id());

                    self.time_of_last_req = data.measured_at;
                    self.currently_playing = Some(data);

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
                    trace!("Not currently playing: {reason}");
                    self.currently_playing = None;
                    self.current_song_with_lyrics = None;
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

        let viewport_rect = ctx.viewport_rect();
        self.window_size = [viewport_rect.width(), viewport_rect.height()];

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
                        .frame(true)
                        .frame_when_inactive(false),
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

        if self.is_auth {
            self.media_controls_ui(ctx);
        }

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

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let mut settings = self.settings.blocking_write();
        [settings.window_width, settings.window_height] = self.window_size;
        if let Err(e) = settings.save() {
            warn!("Failed to save window size on exit: {e}");
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let [r, g, b] = self.settings_cache.background_color;
        [
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            self.settings_cache.opacity,
        ]
    }
}

/// egui's built-in font only covers Latin/Cyrillic, so Japanese/Korean (and most other
/// non-Latin) song titles and lyrics render blank without this. Rather than bundle
/// multi-megabyte font files with the app, fall back to fonts Windows already ships.
/// Japanese and Korean are installed as separate fallback fonts (not just one candidate
/// list) since Yu Gothic doesn't cover Hangul and Malgun Gothic doesn't cover kana.
fn install_cjk_fallback_font(ctx: &egui::Context) {
    install_fallback_font(
        ctx,
        "cjk_fallback_ja",
        &[
            r"C:\Windows\Fonts\YuGothR.ttc",
            r"C:\Windows\Fonts\msgothic.ttc",
        ],
    );
    install_fallback_font(ctx, "cjk_fallback_ko", &[r"C:\Windows\Fonts\malgun.ttf"]);
}

/// Tries each candidate path in order and installs the first one found as a fallback
/// font under `name`. Candidates are alternatives for the same script (e.g. a modern
/// font and an older one), not additional scripts - call this once per script.
fn install_fallback_font(ctx: &egui::Context, name: &str, candidates: &[&str]) {
    for path in candidates {
        match std::fs::read(path) {
            Ok(bytes) => {
                ctx.add_font(FontInsert::new(
                    name,
                    egui::FontData::from_owned(bytes),
                    vec![InsertFontFamily {
                        family: egui::FontFamily::Proportional,
                        priority: FontPriority::Lowest,
                    }],
                ));
                debug!("Loaded fallback font '{name}' from {path}");
                return;
            }
            Err(e) => trace!("Fallback font '{name}' not found at {path}: {e}"),
        }
    }
    warn!("No fallback font found for '{name}'; some text may not render");
}
