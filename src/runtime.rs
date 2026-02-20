#![warn(clippy::pedantic)]

use std::sync::Arc;

use tokio::runtime::Builder;
use tokio::sync::mpsc;

use tracing::{debug, error, trace};

use crate::MessageToRT;
use crate::MessageToUI;
use crate::settings::Settings;
use crate::spotify::SpotifyClient;

use thiserror::Error;

#[derive(Error, Debug)]
enum RuntimeError {}

pub async fn start_runtime(
    tx: mpsc::Sender<MessageToUI>,
    mut rx: mpsc::Receiver<MessageToRT>,
    settings: Arc<Settings>,
) {
    // Channels
    // let (rt_to_spot, rx_in_spot) = mpsc::channel(32);
    // let (spot_to_rt, rx_in_rt) = mpsc::channel(32);

    // Spawn our runtime
    let runtime = Builder::new_multi_thread()
        .thread_name("spotify")
        .build()
        .unwrap();

    let mut spotify_client = SpotifyClient::new();

    // let lyrics_fetcher = LyricsFetcher::new();
    // let time_of_last_currently_playing_request: Option<Instant> = None;

    while let Some(msg) = rx.recv().await {
        let res = match msg {
            MessageToRT::Authenticate => authenticate(settings.clone(), &mut spotify_client).await,
            MessageToRT::GetCurrentTrack => get_current_track(&spotify_client).await,
        };

        match res {
            _ => {}
        }
    }
    trace!("Reached end of runtime");
}

async fn get_current_track(spotify_client: &SpotifyClient) -> Result<MessageToUI, RuntimeError> {
    debug!("Getting current track");
    let res = spotify_client.get_current_track().await.unwrap();

    Ok(MessageToUI::CurrentlyPlaying(res.unwrap()))
}

async fn authenticate(
    settings: Arc<Settings>,
    spotify_client: &mut SpotifyClient,
) -> Result<MessageToUI, RuntimeError> {
    debug!("Starting authentication");

    // Spawn a thread to wait for authentication
    let res = spotify_client
        .authenticate(
            settings.client_id.clone(),
            settings.client_secret.clone(),
            settings.redirect_url(),
        )
        .await;

    Ok(MessageToUI::Authenticated)
}

async fn log_and_display_error(err_string: String) {
    error!("{err_string}");
    //TODO: Send to display
}
