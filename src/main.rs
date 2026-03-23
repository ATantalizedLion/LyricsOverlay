#![warn(clippy::pedantic)]
//TODO: allow offsetting lyrics, using arrow keys?
//TODO: Handle unsynced lyrics, show scrollbar?
//TODO: Clear lyrics on lyric fetch failure (no need for error response, maybe an empty lyric reponse with "ded")
//TODO: add a nice little readme so the project is nice and usable by others
//TODO: Change color change timing in scroll.
//TODO: Add indication of time between final lyric and song end to lyrics. Same for song start.

//TODO: Settings for how much we change scale and color

use std::fs::{File, exists};
use std::io::Write;
use std::sync::Arc;

use tokio::sync::RwLock as TokioRwLock;
use tokio::sync::mpsc;

use tracing::{debug, info};
use tracing_appender::{non_blocking, rolling};
use tracing_subscriber::EnvFilter;

use crate::lyrics_fetch::LyricsRequestInfo;
use crate::lyrics_fetch::SongWithLyrics;
use crate::overlay::LyricsAppUI;
use crate::runtime::start_runtime;
use crate::settings::Settings;
use crate::spotify::CurrentlyPlayingResponse;

mod lyrics_fetch;
mod lyrics_parser;
mod overlay;
mod runtime;
mod settings;
mod spotify;

#[derive(Debug)]
pub enum MessageToUI {
    AuthenticationStateUpdate(bool),
    RateLimitsExceeded,
    CurrentlyPlaying(CurrentlyPlayingResponse),
    NotCurrentlyPlaying(String),
    DisplayError(String),
    GotLyrics(SongWithLyrics),
}

#[derive(Debug)]
pub enum MessageToRT {
    Authenticate,
    GetCurrentTrack,
    GetLyrics(LyricsRequestInfo),
    InvalidateToken,
}

fn main() {
    // Generate config file if no config is found
    if !exists("config.toml").unwrap() {
        let str = toml::ser::to_string_pretty(&Settings::default()).unwrap();
        let mut output = File::create("config.toml").unwrap();
        write!(output, "{str}").unwrap();
        println!("Created config, please add client_id and client_secret");
    }

    // Load settings file
    let settings = match Settings::new() {
        Ok(set) => set,
        Err(settings_error) => {
            println!("Errored on creating settings struct: {settings_error}. \n Returning default");
            Settings::default()
        }
    };
    let rw_settings = Arc::new(TokioRwLock::new(settings));
    let settings_read = rw_settings.blocking_read();
    // Logging
    let file_appender = rolling::daily("logs", "app.log");
    let (non_blocking, _writer_guard) = non_blocking(file_appender);
    let filter = EnvFilter::try_new(&settings_read.log_level).unwrap();
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(false)
        .finish();
    let _subscriber_guard = tracing::subscriber::set_global_default(subscriber);
    info!("Logging initialized with {}", &settings_read.log_level);
    std::mem::drop(settings_read);

    // Channels
    let (to_ui, ui_rx) = mpsc::channel(32);
    let (to_rt, rt_rx) = mpsc::channel(32);

    // Spawn a thread for our runtime
    std::thread::spawn({
        let arc_settings = Arc::clone(&rw_settings);
        let to_rt_clone = to_rt.clone();
        move || {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(async move {
                    start_runtime(to_ui, to_rt_clone, rt_rx, arc_settings.clone()).await;
                });
        }
    });

    // TODO: resizable or auto calculated size
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
        Box::new(|cc| {
            Ok(Box::new(LyricsAppUI::new(
                cc,
                to_rt,
                ui_rx,
                Arc::clone(&rw_settings),
            )))
        }),
    );

    debug!("Post-Eframe run native log");
}
