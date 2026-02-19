#![warn(clippy::pedantic)]

use std::fs::{File, exists};
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;
use tokio::sync::Mutex;

use tracing::subscriber::DefaultGuard;
use tracing::{debug, error, info, trace};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::{non_blocking, rolling};
use tracing_subscriber::EnvFilter;

use crate::lyrics_fetch::LyricsFetcher;
use crate::spotify::CurrentlyPlayingResponse;
use crate::spotify::SpotifyClientAuthError;
use crate::{settings::Settings, spotify::SpotifyClient};

mod lyrics_fetch;
mod lyrics_parser;
mod overlay;
mod settings;
mod spotify;

static APP_USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    " (github.com/ATantalizedLion/LyricsOverlay)"
);

//TODO: lyrics_fetch
//TODO: Cache fetched lyrics

/// Main application state
pub struct LyricsApp {
    is_authenticated: Arc<Mutex<bool>>,
    error_display_string: Arc<Mutex<Option<String>>>,
    settings: Arc<Mutex<Settings>>, // does not currently need to be mutable but we might want a nice lil settings screen later
    spotify_client: Arc<Mutex<spotify::SpotifyClient>>,
    currently_playing: Arc<Mutex<Option<CurrentlyPlayingResponse>>>,
    time_of_last_currently_playing_request: Arc<Mutex<Option<Instant>>>,
    lyrics_fetcher: Arc<Mutex<LyricsFetcher>>,
    log_guards: (WorkerGuard, DefaultGuard),
}

#[tokio::main]
async fn main() {
    // Generate config file if no config is in
    if !exists("config.toml").unwrap() {
        let str = toml::ser::to_string_pretty(&Settings::default()).unwrap();
        let mut output = File::create("config.toml").unwrap();
        write!(output, "{str}").unwrap();
        println!("Created config, please add client_id and client_secret")
    }

    // Load settings file
    let settings = match Settings::new() {
        Ok(set) => set,
        Err(settings_error) => {
            println!("Errored on creating settings struct: {settings_error}. \n Returning default");
            Settings::default()
        }
    };

    // Logging
    let file_appender = rolling::daily("logs", "app.log");
    let (non_blocking, writer_guard) = non_blocking(file_appender);
    let filter = EnvFilter::try_new(&settings.log_level).unwrap();
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(false)
        .finish();
    let subscriber_guard = tracing::subscriber::set_default(subscriber);
    let log_guards = (writer_guard, subscriber_guard);
    info!("Logging initialized with {}", &settings.log_level);
    trace!("Settings contents: {settings:?}");

    // TODO: Draggable and resizable
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

    _ = eframe::run_native(
        "Lyrics overlay",
        options,
        Box::new(|cc| Ok(Box::new(LyricsApp::new(cc, log_guards, settings)))),
    );

    debug!("Post-Eframe run native log");
}

#[derive(Error, Debug)]
pub enum LyricsAppError {
    #[error("Spotify Authentication Error: ")]
    Spotify(#[from] SpotifyClientAuthError),
}

impl LyricsApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        log_guards: (WorkerGuard, DefaultGuard),
        settings: Settings,
    ) -> Self {
        Self {
            log_guards,
            error_display_string: Arc::new(Mutex::new(None)),
            time_of_last_currently_playing_request: Arc::new(Mutex::new(None)),
            currently_playing: Arc::new(Mutex::new(None)),
            is_authenticated: Arc::new(Mutex::new(false)),
            spotify_client: Arc::new(Mutex::new(SpotifyClient::new())),
            settings: Arc::new(Mutex::new(settings)),
            lyrics_fetcher: Arc::new(Mutex::new(LyricsFetcher::new())),
        }
    }

    pub fn get_current_track(&self) -> Result<(), LyricsAppError> {
        debug!("Getting current track");
        let spot = self.spotify_client.clone();
        let req_time = self.time_of_last_currently_playing_request.clone();
        let err_disp = self.error_display_string.clone();

        // Spawn a thread to wait for authentication
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(async move {
                    let mut req_time_g = req_time.lock().await;
                    *req_time_g = Some(Instant::now());

                    let spotify_client = spot.lock().await;
                    let res = spotify_client.get_current_track().await;
                    if let Err(e) = res {
                        log_and_display_error(err_disp, format!("Client error: {e}")).await;
                    }
                });
        });

        Ok(())
    }

    pub fn authenticate(&self) -> Result<(), LyricsAppError> {
        debug!("Starting authentication");
        let spot = self.spotify_client.clone();
        let auth = self.is_authenticated.clone();
        let err_disp = self.error_display_string.clone();

        // Get owned copies of the required settings components
        let (client_id, client_secret, redirect) = {
            let settings = self.settings.try_lock().unwrap();
            (
                settings.client_id.clone(),
                settings.client_secret.clone(),
                settings.redirect_url().clone(),
            )
        };

        // Spawn a thread to wait for authentication
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(async move {
                    let mut spotify_client = spot.lock().await;
                    let res = spotify_client
                        .authenticate(client_id, client_secret, redirect)
                        .await;
                    if let Err(e) = res {
                        log_and_display_error(err_disp, format!("Auth error: {e}")).await;
                    }
                    let mut auth_lock = auth.lock().await;
                    *auth_lock = true;
                });
        });

        Ok(())
    }
}

async fn log_and_display_error(err_display: Arc<Mutex<Option<String>>>, err_string: String) {
    let mut err_display: tokio::sync::MutexGuard<'_, Option<String>> = err_display.lock().await;
    *err_display = Some(err_string.clone());
    error!("{err_string}");
}
