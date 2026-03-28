use std::fs;

use config::{Config, ConfigError, Environment, File};
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

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
    /// How often (seconds) to poll Spotify for the current track
    pub poll_interval_ms: u64,
    /// Scroll smoothly or jump per line
    pub scroll_smoothly: bool,
    /// Time between line transitions
    pub line_transition_ms: u64,
    /// Do we show debug draws or not.
    pub draw_debug_stuff: bool,
    /// progress bar position
    pub line_progress_bar_position: ProgressBarPosition,
    /// song bar position
    pub song_progress_bar_position: ProgressBarPosition,
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
            poll_interval_ms: 4000,
            scroll_smoothly: false,
            line_transition_ms: 400,
            draw_debug_stuff: false,
            line_progress_bar_position: ProgressBarPosition::Hidden,
            song_progress_bar_position: ProgressBarPosition::Hidden,
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
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BelowCurrentLine => "Below line",
            Self::Bottom => "Bottom",
            Self::Hidden => "Hidden",
        }
    }
}
