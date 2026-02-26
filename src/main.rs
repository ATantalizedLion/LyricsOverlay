#![warn(clippy::pedantic)]

use std::fs::{File, exists};
use std::io::Write;
use std::sync::Arc;

use thiserror::Error;

use tokio::sync::mpsc;

use tracing::{debug, info, trace};
use tracing_appender::{non_blocking, rolling};
use tracing_subscriber::EnvFilter;

use crate::lyrics_fetch::LyricsRequestInfo;
use crate::lyrics_fetch::SongWithLyrics;
use crate::overlay::LyricsAppUI;
use crate::runtime::start_runtime;
use crate::settings::Settings;
use crate::spotify::{CurrentlyPlayingResponse, SpotifyClientAuthError};

mod lyrics_fetch;
mod lyrics_parser;
mod overlay;
mod runtime;
mod settings;
mod spotify;

static APP_USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    " (github.com/ATantalizedLion/LyricsOverlay)"
);

#[derive(Debug)]
pub enum MessageToUI {
    Authenticated,
    CurrentlyPlaying(CurrentlyPlayingResponse),
    DisplayError(String),
    GotLyrics(SongWithLyrics),
}

#[derive(Debug)]
pub enum MessageToRT {
    Authenticate,
    GetCurrentTrack,
    GetLyrics(LyricsRequestInfo),
}

#[derive(Error, Debug)]
pub enum LyricsAppError {
    #[error("Spotify Authentication Error: ")]
    Spotify(#[from] SpotifyClientAuthError),
}

fn main() {
    // Generate config file if no config is found
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
    let arc_settings = Arc::new(settings);

    // Logging
    let file_appender = rolling::daily("logs", "app.log");
    let (non_blocking, _writer_guard) = non_blocking(file_appender);
    let filter = EnvFilter::try_new(&arc_settings.clone().log_level).unwrap();
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(false)
        .finish();
    let _subscriber_guard = tracing::subscriber::set_global_default(subscriber);
    info!(
        "Logging initialized with {}",
        &arc_settings.clone().log_level
    );
    trace!("Settings contents: {:?}", arc_settings.clone());

    // Channels
    let (rt_to_ui, rx_in_ui) = mpsc::channel(32);
    let (ui_to_rt, rx_in_rt) = mpsc::channel(32);

    // Spawn a thread for our runtime
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                start_runtime(rt_to_ui, rx_in_rt, arc_settings).await;
            });
    });

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
        Box::new(|cc| Ok(Box::new(LyricsAppUI::new(cc, ui_to_rt, rx_in_ui)))),
    );

    debug!("Post-Eframe run native log");
}
