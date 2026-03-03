use std::fs;

use config::{Config, ConfigError, Environment, File};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Host for the OAuth server
    #[serde(default = "default_host")]
    pub host: String,
    /// Port for the OAuth server
    #[serde(default = "default_port")]
    pub port: u16,
    /// sp dc
    #[serde(default = "default_string")]
    pub sp_dc: String,
    /// Spotify client id
    #[serde(default = "default_string")]
    pub client_id: String,
    /// Spotify client secret
    #[serde(default = "default_string")]
    pub client_secret: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Background opacity 0.0–1.0
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    /// Font size for the active lyric line (px)
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default = "default_bool_true")]
    pub caching_enabled: bool,
    #[serde(default = "default_cache_folder")]
    pub cache_folder: String,
    /// Dim lines that are far from the current line
    #[serde(default = "default_bool_true")]
    pub dim_distant_lines: bool,
    /// How often (seconds) to poll Spotify for the current track
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            client_id: default_string(),
            client_secret: default_string(),
            log_level: default_log_level(),
            opacity: default_opacity(),
            font_size: default_font_size(),
            caching_enabled: default_bool_true(),
            cache_folder: default_cache_folder(),
            dim_distant_lines: default_bool_true(),
            poll_interval_secs: default_poll_interval(),
            sp_dc: default_string(),
        }
    }
}

fn default_bool_true() -> bool {
    true
}
fn default_string() -> String {
    String::new()
}
fn default_log_level() -> String {
    "trace".into()
}
fn default_cache_folder() -> String {
    "cache".into()
}
fn default_opacity() -> f32 {
    0.7
}
fn default_font_size() -> f32 {
    26.0
}
fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    8123
}
fn default_poll_interval() -> u32 {
    5
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
        let toml = toml::ser::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialise settings: {e}"))?;
        fs::write("config.toml", toml).map_err(|e| format!("Failed to write config.toml: {e}"))
    }
}

//TODO: Add settings value for which lyric source to use
//TODO: Maybe even add a sp_dc what am I playing call...
//TODO:
