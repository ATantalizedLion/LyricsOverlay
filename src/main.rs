#![warn(clippy::pedantic)]

use std::fs::{File, exists};
use std::io::Write;
use std::sync::Arc;
use tokio::sync::Mutex;

use lyrics_parser::LyricLine;
use tracing::subscriber::DefaultGuard;
use tracing::{info, trace};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::{non_blocking, rolling};
use tracing_subscriber::EnvFilter;

use crate::{settings::Settings, spotify::SpotifyClient};
mod lyrics_parser;
mod settings;
mod spotify;
//TODO: lyrics_fetch
//TODO: Cache fetched lyrics

/// Main application state
pub struct LyricsApp {
    settings: Arc<Mutex<Settings>>,
    spotify_client: Arc<Mutex<spotify::SpotifyClient>>,
    _log_guards: (WorkerGuard, DefaultGuard),
}

#[tokio::main]
async fn main() {
    if !exists("config.toml").unwrap() {
        let str = toml::ser::to_string_pretty(&Settings::default()).unwrap();
        let mut output = File::create("config.toml").unwrap();
        write!(output, "{str}").unwrap();
    }

    let settings = match Settings::new() {
        Ok(set) => set,
        Err(settings_error) => {
            println!("Errored on creating settings struct: {settings_error}. \n Returning default");
            Settings::default()
        }
    };

    let file_appender = rolling::daily("logs", "app.log");
    let (non_blocking, writer_guard) = non_blocking(file_appender);
    let filter = EnvFilter::try_new(&settings.log_level).unwrap();
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(false)
        .finish();

    // Set per-thread subscriber without global init
    let subscriber_guard = tracing::subscriber::set_default(subscriber);
    info!("Logging initialized with {}", &settings.log_level);
    trace!("Settings contents: {settings:?}");

    let state = LyricsApp {
        spotify_client: Arc::new(Mutex::new(SpotifyClient::new())),
        settings: Arc::new(Mutex::new(settings)),
        _log_guards: (writer_guard, subscriber_guard),
    };

    // Now authenticate with spotify..
    if let Err(e) = state.authenticate().await {
        eprintln!("Authentication failed: {e}");
    }

    let _ = state.get_current_track().await;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Lyrics Overlay")
            .with_inner_size([680.0, 340.0])
            .with_min_inner_size([320.0, 160.0])
            .with_decorations(false) // no window chrome
            .with_transparent(true) // transparent background
            .with_always_on_top()
            .with_resizable(true),
        ..Default::default()
    };
}

impl LyricsApp {
    async fn authenticate(&self) -> Result<(), Box<dyn std::error::Error>> {
        let (client_id, client_secret, redirect) = {
            let settings = self.settings.lock().await;
            (
                settings.client_id.clone(),
                settings.client_secret.clone(),
                settings.redirect_url().clone(),
            )
        };

        let mut client = self.spotify_client.lock().await;
        client
            .authenticate(&client_id, &client_secret, &redirect)
            .await?;
        Ok(())
    }

    async fn get_current_track(
        &self,
    ) -> Result<Option<(String, String)>, Box<dyn std::error::Error>> {
        let client = self.spotify_client.lock().await;
        Ok(client.get_current_track().await?)
    }
}
