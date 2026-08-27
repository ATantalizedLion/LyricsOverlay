use std::fs;

use config::{Config, ConfigError, Environment, File};
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Host for the OAuth server
    pub host: String,
    /// Port for the OAuth server
    pub port: u16,
    /// sp dc, do not use!
    pub sp_dc: String,
    /// Spotify client id
    pub client_id: String,
    /// Spotify client secret
    pub client_secret: String,
    /// Spotify refresh token
    pub refresh_token: Option<String>,
    /// Spotify access token
    pub access_token: Option<String>,
    /// Spotify token expiry date/time
    pub expiry_time_as_unix: Option<u64>,
    /// Authenticate on startup
    pub auto_auth: bool,
    /// Log level for all logs
    pub log_level: String,
    /// Background opacity 0.0–1.0
    pub opacity: f32,
    /// Font size for the active lyric line (px)
    pub font_size: f32,
    /// Line spacing
    pub line_spacing: f32,
    /// Do we cache found lyrics
    pub caching_enabled: bool,
    /// Folder in which we store cached lyrics
    pub cache_folder: String,
    /// Dim lines that are far from the current line
    pub dim_distant_lines: bool,
    /// How often (ms) to poll Windows SMTC for the current track
    pub smtc_poll_interval_ms: u64,
    /// Scroll smoothly or jump per line
    pub scroll_smoothly: bool,
    /// Time between line transitions
    pub line_transition_ms: u64,
    /// progress bar position
    pub line_progress_bar_position: ProgressBarPosition,
    /// song bar position
    pub song_progress_bar_position: ProgressBarPosition,
    /// easing of the position while playing
    pub ease_position: EasingModes,
    /// easing of the color while playing
    pub ease_color: EasingModes,
    /// where (if at all) to show play/pause/skip controls
    pub media_controls_position: MediaControlsPosition,
    /// Background color of the overlay (opacity is controlled separately)
    pub background_color: [u8; 3],
    /// Color of lines already sung
    pub past_line_color: [u8; 3],
    /// Color of the line currently being sung
    pub current_line_color: [u8; 3],
    /// Color of lines not yet sung
    pub future_line_color: [u8; 3],
    /// Only show the minimize/settings/close buttons while hovering the window
    pub window_controls_on_hover: bool,
    /// Last known window position (top-left, monitor space, egui points). `None` until
    /// the window has been moved at least once.
    pub window_pos: Option<[f32; 2]>,
    /// Last known window size (egui points)
    pub window_size: [f32; 2],
    /// Which now-playing/control source to check first - the other is used as a fallback
    /// when the preferred one has nothing (e.g. Spotify Connect playing on another
    /// device/speaker that SMTC can't see, or vice versa). Requires being connected (see
    /// Client ID/Secret below) for either Spotify option to do anything.
    pub now_playing_source: NowPlayingSource,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8123,
            sp_dc: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            refresh_token: None,
            access_token: None,
            expiry_time_as_unix: None,
            auto_auth: true,
            log_level: "debug".into(),
            opacity: 0.7,
            font_size: 26.0,
            line_spacing: 42.0,
            caching_enabled: true,
            cache_folder: "cache".into(),
            dim_distant_lines: true,
            smtc_poll_interval_ms: 1000,
            scroll_smoothly: false,
            line_transition_ms: 400,
            line_progress_bar_position: ProgressBarPosition::Hidden,
            song_progress_bar_position: ProgressBarPosition::Hidden,
            ease_position: EasingModes::Linear,
            ease_color: EasingModes::Cubic,
            media_controls_position: MediaControlsPosition::Hidden,
            background_color: [0, 0, 0],
            past_line_color: [200, 180, 255],
            current_line_color: [255, 255, 255],
            future_line_color: [180, 210, 255],
            window_controls_on_hover: false,
            window_pos: None,
            window_size: [680.0, 340.0],
            now_playing_source: NowPlayingSource::SmtcOnly,
        }
    }
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        Config::builder()
            .add_source(File::with_name("config"))
            .add_source(Environment::with_prefix("APP"))
            .build()?
            .try_deserialize()
    }

    pub fn reset(&mut self) {
        *self = Self { ..Self::default() };
    }

    pub fn redirect_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    /// Serialize the current state back to `config.toml`.
    pub fn save(&self) -> Result<(), String> {
        debug!("Starting save!");
        let toml = toml::ser::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialise settings: {e}"))?;
        let res =
            fs::write("config.toml", toml).map_err(|e| format!("Failed to write config.toml: {e}"));
        if res.is_err() {
            error!("{}", res.clone().err().unwrap());
        }
        res
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum ProgressBarPosition {
    Hidden,
    BelowCurrentLine,
    #[default]
    Bottom,
}
impl ProgressBarPosition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BelowCurrentLine => "Below line",
            Self::Bottom => "Bottom",
            Self::Hidden => "Hidden",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum EasingModes {
    Cubic,
    #[default]
    Linear,
}
impl EasingModes {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cubic => "Cubic",
            Self::Linear => "Linear",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum MediaControlsPosition {
    #[default]
    Hidden,
    Top,
    TopHover,
    Bottom,
    BottomHover,
}
impl MediaControlsPosition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hidden => "Hidden",
            Self::Top => "Top",
            Self::TopHover => "Top (on hover)",
            Self::Bottom => "Bottom",
            Self::BottomHover => "Bottom (on hover)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum NowPlayingSource {
    /// Windows SMTC only - works with any app, no Spotify calls ever made.
    #[default]
    SmtcOnly,
    /// SMTC checked first; Spotify's own API used as a fallback when SMTC has nothing.
    PreferSmtc,
    /// Spotify's own API checked first; SMTC used as a fallback when Spotify has nothing.
    PreferSpotify,
}
impl NowPlayingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SmtcOnly => "Windows only",
            Self::PreferSmtc => "Windows, then Spotify",
            Self::PreferSpotify => "Spotify, then Windows",
        }
    }
}
