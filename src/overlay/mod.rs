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
    now_playing::NowPlaying,
    overlay::resize::handle_resize,
    settings::Settings,
    theming::{self, Theme},
};

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
    /// When `error_string` was last set, to auto-dismiss it after a while - see
    /// `draw_error_toast`.
    error_shown_at: Instant,
    /// Whatever's currently playing, from whichever source detected it
    currently_playing: Option<NowPlaying>,

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

    /// User-defined themes loaded from the `themes/` folder at startup
    custom_themes: Vec<Theme>,

    /// When we last wrote window position/size to disk, to avoid hammering it while
    /// the window is actively being dragged or resized.
    last_window_save: Instant,
    /// Last window (position, size) observed from `egui`, updated every frame
    /// regardless of the save throttle - `on_exit` flushes whatever's here last.
    last_known_window_rect: Option<([f32; 2], [f32; 2])>,
}

impl LyricsAppUI {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        tx: mpsc::Sender<MessageToRT>,
        rx: mpsc::Receiver<MessageToUI>,
        settings: &Arc<TokioRwLock<Settings>>,
    ) -> Self {
        install_cjk_fallback_font(&cc.egui_ctx);

        Self {
            is_auth: false,
            tx,
            rx,
            currently_playing: None,
            error_string: None,
            error_shown_at: Instant::now(),
            time_of_last_req: Instant::now(),
            current_song_with_lyrics: None,
            settings: settings.clone(),
            settings_cache: settings.blocking_read().clone(),
            settings_open: false,
            line_top_offsets: vec![],
            custom_themes: theming::load_custom_themes(),
            last_window_save: Instant::now(),
            last_known_window_rect: None,
        }
    }

    fn message_loop(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                MessageToUI::AuthenticationStateUpdate(new_state) => {
                    let was_authenticated = self.is_auth;
                    self.is_auth = new_state;
                    if !new_state && was_authenticated {
                        // Only clear on the true->false transition, not every repeat of
                        // an already-false state: invalidate() itself re-sends this same
                        // message, and re-triggering here would ping-pong forever.
                        self.show_error("Spotify session expired, please reconnect".into());
                        let _ = self.tx.try_send(MessageToRT::InvalidateToken);
                    }
                }
                MessageToUI::CurrentlyPlaying(data) => {
                    let same_track = self
                        .currently_playing
                        .as_ref()
                        .is_some_and(|s| s.is_same_track(&data));

                    self.time_of_last_req = data.measured_at;
                    self.currently_playing = Some(data);

                    if !same_track {
                        self.tx
                            .try_send(MessageToRT::GetLyrics(LyricsRequestInfo::from_now_playing(
                                self.currently_playing.as_ref().unwrap(),
                            )))
                            .unwrap();
                        self.line_top_offsets.clear();
                    }
                }
                MessageToUI::DisplayError(err) => self.show_error(err),
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
                    self.show_error("Rate limits exceeded!".to_string());
                }
            }
        }
    }

    /// Sets the error toast's message and (re)starts its auto-dismiss timer.
    fn show_error(&mut self, message: String) {
        self.error_string = Some(message);
        self.error_shown_at = Instant::now();
    }

    /// Auto-dismissing toast for the last error, floating over the lyrics rather than
    /// reserving a fixed-height strip - a long Debug-formatted error (auth failures,
    /// reqwest errors) used to overflow that strip instead of wrapping.
    fn draw_error_toast(&mut self, ctx: &egui::Context, full_width: f32) {
        const AUTO_DISMISS: std::time::Duration = std::time::Duration::from_secs(6);
        const ALERT: Color32 = Color32::from_rgb(230, 90, 90);
        const ALERT_TEXT: Color32 = Color32::from_rgb(255, 150, 150);

        let Some(err) = self.error_string.clone() else {
            return;
        };
        if self.error_shown_at.elapsed() > AUTO_DISMISS {
            self.error_string = None;
            return;
        }

        let background = self.settings_cache.background_color;
        let max_width = (full_width - 40.0).min(360.0);
        egui::Area::new("error_toast".into())
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -14.0))
            .show(ctx, |ui| {
                ui.set_max_width(max_width);
                egui::Frame::new()
                    .fill(Color32::from_rgb(background[0], background[1], background[2]))
                    .corner_radius(8)
                    .stroke(egui::Stroke::new(1.0f32, ALERT))
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("⚠ Error")
                                    .size(11.0)
                                    .color(ALERT)
                                    .strong(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("×").clicked() {
                                        self.error_string = None;
                                    }
                                },
                            );
                        });
                        ui.add_space(2.0);
                        ui.add(
                            egui::Label::new(RichText::new(&err).size(12.0).color(ALERT_TEXT))
                                .wrap(),
                        );
                    });
            });
    }

    /// Minimize/close buttons, and the settings gear + its viewport.
    fn draw_window_controls(&mut self, ctx: &egui::Context, full_width: f32) {
        // When enabled, the minimize/settings/close buttons only render while the
        // pointer is somewhere over the window, keeping the overlay fully minimal at rest.
        let show_window_controls = !self.settings_cache.window_controls_on_hover
            || ctx.input(|i| i.pointer.hover_pos()).is_some();

        if show_window_controls {
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
                                    .color(accent_color(self.settings_cache.current_line_color)),
                            )
                            .frame(true)
                            .frame_when_inactive(false),
                        )
                        .clicked()
                    {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

            // Minimize button
            egui::Area::new("minimize".into())
                .fixed_pos(egui::pos2(full_width - 45., 10.))
                .show(ctx, |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("─")
                                    .size(14.0)
                                    .color(accent_color(self.settings_cache.current_line_color)),
                            )
                            .frame(true)
                            .frame_when_inactive(false),
                        )
                        .clicked()
                    {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                });
        }

        // Always shown (not gated on hover): once settings is open, its own viewport
        // needs to be serviced every frame regardless of whether the gear button that
        // toggles it is currently visible. The gear button itself still respects
        // `show_window_controls`.
        egui::Area::new("settings_overlay".into())
            .fixed_pos(egui::pos2(full_width - 65., 10.))
            .show(ctx, |ui| {
                self.settings_ui(ui, ctx, show_window_controls);
            });
    }

    /// Remembers the window's current position/size (in-memory only) so it reopens where
    /// it was left, then opportunistically flushes it to disk. Flushes are throttled so
    /// dragging/resizing doesn't hammer disk I/O; `on_exit` does one final unthrottled
    /// flush of whatever was last observed here, to catch the very last change.
    fn persist_window_geometry(&mut self, ctx: &egui::Context) {
        let minimized = ctx.input(|i| i.viewport().minimized).unwrap_or(false);
        if minimized {
            return;
        }
        let Some(rect) = ctx.input(|i| i.viewport().inner_rect) else {
            return;
        };
        self.last_known_window_rect = Some(([rect.min.x, rect.min.y], [rect.width(), rect.height()]));

        if self.last_window_save.elapsed() < std::time::Duration::from_millis(500) {
            return;
        }
        self.flush_window_geometry(false);
    }

    /// Below the OS/compositor's own jitter in reported window rects (observed: a
    /// perfectly still window's `inner_rect` still drifts by fractions of a point
    /// frame-to-frame) - without this tolerance in `flush_window_geometry`, an exact
    /// comparison never settles and re-saves on every throttle tick forever, even at rest.
    const GEOMETRY_EPSILON: f32 = 1.0;

    /// Writes `last_known_window_rect` to settings and disk, if it differs from what's
    /// already cached beyond `GEOMETRY_EPSILON`. Uses a non-blocking write so a normal
    /// per-frame flush never stalls the render thread; `on_exit` sets `blocking` since
    /// it's a one-shot call where losing the final position would be worse than a brief
    /// stall.
    fn flush_window_geometry(&mut self, blocking: bool) {
        let Some((pos, size)) = self.last_known_window_rect else {
            return;
        };
        let unchanged = self.settings_cache.window_pos.is_some_and(|cached| {
            (cached[0] - pos[0]).abs() < Self::GEOMETRY_EPSILON
                && (cached[1] - pos[1]).abs() < Self::GEOMETRY_EPSILON
        }) && (self.settings_cache.window_size[0] - size[0]).abs() < Self::GEOMETRY_EPSILON
            && (self.settings_cache.window_size[1] - size[1]).abs() < Self::GEOMETRY_EPSILON;
        if unchanged {
            return;
        }

        let mut settings = if blocking {
            self.settings.blocking_write()
        } else {
            let Ok(settings) = self.settings.try_write() else {
                return;
            };
            settings
        };
        settings.window_pos = Some(pos);
        settings.window_size = size;
        let _ = settings.save();
        self.settings_cache.window_pos = Some(pos);
        self.settings_cache.window_size = size;
        drop(settings);
        self.last_window_save = Instant::now();
    }
}

impl eframe::App for LyricsAppUI {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint();

        let accent = accent_color(self.settings_cache.current_line_color);
        ctx.set_visuals(egui::Visuals {
            panel_fill: Color32::TRANSPARENT,
            window_fill: Color32::TRANSPARENT,
            ..theme_visuals(self.settings_cache.background_color, accent)
        });

        handle_resize(ctx, 6.0f32);

        let full_width = ctx.available_rect().width();

        // Stop font from being shifted for font alignment
        ctx.tessellation_options_mut(|opts| {
            opts.round_text_to_pixels = false;
        });

        // Cache settings if not locked.
        if let Ok(s) = self.settings.try_read() {
            self.settings_cache = s.clone();
        }

        self.persist_window_geometry(ctx);

        self.message_loop();

        self.draw_window_controls(ctx, full_width);

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
                    // Works with no setup via SMTC - Spotify connection (see Settings)
                    // is optional, only needed for Spotify's own bonus lyrics source.
                    self.display_lyrics(ui);
                });
            });

        self.media_controls_ui(ctx);

        self.draw_error_toast(ctx, full_width);
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

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush_window_geometry(true);
    }
}

/// Blends a theme color partway toward neutral gray, for UI chrome (icons, titles,
/// window backgrounds) that should hint at the active theme without becoming as loud as
/// the fully-saturated lyric colors.
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_sign_loss)]
pub(super) fn accent_color(base: [u8; 3]) -> Color32 {
    const NEUTRAL: f32 = 170.0;
    const BLEND: f32 = 0.4;
    // Always in 0..=255: a weighted average of two values already in that range.
    let mix = |c: u8| (f32::from(c).mul_add(BLEND, NEUTRAL * (1.0 - BLEND))) as u8;
    Color32::from_rgb(mix(base[0]), mix(base[1]), mix(base[2]))
}

/// Perceived brightness of an `[r, g, b]` color, 0 (black) to 255 (white).
fn luminance(color: [u8; 3]) -> f32 {
    0.299 * f32::from(color[0]) + 0.587 * f32::from(color[1]) + 0.114 * f32::from(color[2])
}

/// A gray that stays legible against `background`, for text that isn't itself
/// theme-colored (section labels, hints, slider/text-edit contents). `value_on_dark` is
/// the gray value that was tuned by eye assuming a near-black background; on a bright
/// background it's mirrored (`255 - value_on_dark`) instead, with a smooth blend between
/// the two as `background` crosses the midpoint - so custom themes with light
/// backgrounds don't end up with light-gray-on-light-gray text.
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_sign_loss)]
pub(super) fn readable_gray(background: [u8; 3], value_on_dark: u8) -> Color32 {
    let t = (luminance(background) / 255.0).clamp(0.0, 1.0);
    let light_bg_value = 255.0 - f32::from(value_on_dark);
    let value = f32::from(value_on_dark) + (light_bg_value - f32::from(value_on_dark)) * t;
    Color32::from_gray(value as u8)
}

/// Base `egui::Visuals` for a panel painted directly on `background`: dark widget chrome
/// (text-edit fill, slider track, scrollbar, ...) for a dark background, light chrome for
/// a light one, so those don't default to dark-on-dark or light-on-light for a custom
/// theme. The selection highlight (checked/selected widget backgrounds) is tied to
/// `accent` instead of egui's default blue, so it actually reflects the active theme.
pub(super) fn theme_visuals(background: [u8; 3], accent: Color32) -> egui::Visuals {
    let base = if luminance(background) < 128.0 {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    egui::Visuals {
        selection: egui::style::Selection {
            bg_fill: accent,
            stroke: egui::Stroke::new(1.0f32, accent),
        },
        ..base
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
