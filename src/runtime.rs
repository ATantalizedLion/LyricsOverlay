#![warn(clippy::pedantic)]

use std::sync::Arc;
use std::sync::Mutex;

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

//TODO: Better state tracking, pause/play, next song, song end
pub async fn start_runtime(
    tx: mpsc::Sender<MessageToUI>,
    mut rx: mpsc::Receiver<MessageToRT>,
    settings: Arc<Mutex<Settings>>,
) {
    let mut spotify_client = SpotifyClient::new(settings.clone());

    let lyrics_fetcher = LyricsFetcher::new(settings.clone());
    // let time_of_last_currently_playing_request: Option<Instant> = None;

    while let Some(msg) = rx.recv().await {
        let res = match msg {
            MessageToRT::Authenticate => authenticate(&mut spotify_client).await,
            MessageToRT::GetCurrentTrack => get_current_track(&spotify_client).await,
            MessageToRT::GetLyrics(request) => lyrics_fetcher.get_lyrics(request).await,
        };

        match res {
            Ok(message) => tx.send(message).await,
            Err(x) => tx.send(MessageToUI::DisplayError(format!("{x:?}"))).await,
        }
        .unwrap();
    }

    trace!("Reached end of runtime");
}

async fn get_current_track(spotify_client: &SpotifyClient) -> Result<MessageToUI, RuntimeError> {
    debug!("Getting current track");
    let res = spotify_client.get_current_track().await.unwrap();
    // TODO: HANDLE ERRORS PROPERLY HERE, EXPECTED POINT OF FAILURE

    Ok(MessageToUI::CurrentlyPlaying(res))
}

async fn authenticate(spotify_client: &mut SpotifyClient) -> Result<MessageToUI, RuntimeError> {
    debug!("Starting authentication");

    let res = spotify_client.authenticate().await;

    match res {
        Ok(()) => Ok(MessageToUI::Authenticated),
        Err(err) => Err(RuntimeError::AuthenticationFailed(err)),
    }
}
