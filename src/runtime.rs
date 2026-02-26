#![warn(clippy::pedantic)]

use std::sync::Arc;
use tokio::sync::mpsc;

use tracing::{debug, trace};

use crate::MessageToRT;
use crate::MessageToUI;
use crate::lyrics_fetch::LyricsFetcher;
use crate::lyrics_fetch::LyricsFetcherErr;
use crate::settings::Settings;
use crate::spotify::SpotifyClient;
use crate::spotify::SpotifyClientAuthError;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("Authentication failed: {0}")]
    AuthenticationFailed(#[from] SpotifyClientAuthError),
    #[error("Getting lyrics failed: {0}")]
    GetFailed(#[from] LyricsFetcherErr),
}

pub async fn start_runtime(
    tx: mpsc::Sender<MessageToUI>,
    mut rx: mpsc::Receiver<MessageToRT>,
    settings: Arc<Settings>,
) {
    let mut spotify_client = SpotifyClient::new();
    let lyrics_fetcher = LyricsFetcher::new(settings.clone());
    // let time_of_last_currently_playing_request: Option<Instant> = None;

    while let Some(msg) = rx.recv().await {
        let res = match msg {
            MessageToRT::Authenticate => authenticate(settings.clone(), &mut spotify_client).await,
            MessageToRT::GetCurrentTrack => get_current_track(&spotify_client).await,
            MessageToRT::GetLyrics(request) => lyrics_fetcher.get_lyrics(request).await,
        };

        match res {
            Ok(message) => tx.send(message).await,
            Err(x) => tx.send(MessageToUI::DisplayError(format!("{:?}", x))).await,
        }
        .unwrap();
    }

    trace!("Reached end of runtime");
}

async fn get_current_track(spotify_client: &SpotifyClient) -> Result<MessageToUI, RuntimeError> {
    debug!("Getting current track");
    let res = spotify_client.get_current_track().await.unwrap();

    Ok(MessageToUI::CurrentlyPlaying(res))
}

async fn authenticate(
    settings: Arc<Settings>,
    spotify_client: &mut SpotifyClient,
) -> Result<MessageToUI, RuntimeError> {
    debug!("Starting authentication");

    // Spawn a thread to wait for authentication
    let res = spotify_client.authenticate(settings).await;

    match res {
        Ok(_) => Ok(MessageToUI::Authenticated),
        Err(err) => Err(RuntimeError::AuthenticationFailed(err)),
    }
}
