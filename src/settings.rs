use serde::{Deserialize, Serialize};

use config::{Config, ConfigError, Environment, File};

#[derive(Debug, Serialize, Deserialize)]
pub struct Settings {
    /// Host for the OAuth server
    #[serde(default = "default_host")]
    pub host: String,
    /// Port for the OAuth server
    #[serde(default = "default_port")]
    pub port: u16,
    /// Spotify client id
    #[serde(default = "default_string")]
    pub client_id: String,
    /// Spotify client secret
    #[serde(default = "default_string")]
    pub client_secret: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default = "default_bool_true")]
    pub caching_enabled: bool,
    #[serde(default = "default_cache_folder")]
    pub cache_folder: String,
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
    0.2
}
fn default_font_size() -> f32 {
    10.0
}
fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    8123
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        Config::builder()
            .add_source(File::with_name("config"))
            .add_source(Environment::with_prefix("APP"))
            .build()?
            .try_deserialize()
    }

    pub fn redirect_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}
