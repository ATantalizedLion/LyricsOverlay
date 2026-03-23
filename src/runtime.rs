#![warn(clippy::pedantic)]

use std::sync::Arc;

use tokio::sync::Mutex as TokioMutex;
use tokio::sync::RwLock as TokioRwLock;
use tokio::sync::mpsc;

use tracing::info;
use tracing::{debug, trace};

use crate::MessageToRT;
use crate::MessageToUI;
use crate::lyrics_fetch::LyricsFetcher;
use crate::lyrics_fetch::LyricsFetcherErr;
use crate::settings::Settings;
use crate::spotify::SpotifyClient;
use crate::spotify::auth::SpotifyAuthClient;
use crate::spotify::auth::SpotifyClientAuthError;
use crate::spotify::poller::SpotifyPoller;
use crate::spotify::poller::process_current_track_response;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("Authentication failed: {0}")]
    AuthenticationFailed(#[from] SpotifyClientAuthError),
    #[error("Getting lyrics failed: {0}")]
    GetFailed(#[from] LyricsFetcherErr),
}

/// Struct to allow handling different types of messages in a send or receive loop
#[derive(Debug)]
pub struct Messages {
    to_ui: Option<MessageToUI>,
}
impl Messages {
    pub fn to_ui(to_ui: MessageToUI) -> Self {
        Self { to_ui: Some(to_ui) }
    }
    pub async fn send(self, tx_to_ui: mpsc::Sender<MessageToUI>) {
        if let Some(message_ui) = self.to_ui {
            tx_to_ui.send(message_ui).await.unwrap();
        }
    }
}

pub async fn start_runtime(
    tx_to_ui: mpsc::Sender<MessageToUI>,
    tx_to_rt: mpsc::Sender<MessageToRT>,
    mut rx: mpsc::Receiver<MessageToRT>,
    settings: Arc<TokioRwLock<Settings>>,
) {
    info!("Runtime started");
    let spotify_auth_client = Arc::new(TokioMutex::new(SpotifyAuthClient::new(settings.clone())));

    let token_handle = {
        let auth_lock = spotify_auth_client.lock().await;
        auth_lock.retreive_token_handle().clone()
    };
    let spotify_client = Arc::new(SpotifyClient::new(token_handle));
    let lyrics_fetcher = Arc::new(LyricsFetcher::new(settings.clone()));

    // Spawn a thread for our spotify poller
    let poller = SpotifyPoller::new(spotify_client.clone(), settings.clone());
    tokio::spawn(poller.run(tx_to_ui.clone()));

    if settings.read().await.auto_auth {
        tx_to_rt.send(MessageToRT::Authenticate).await.unwrap();
    }

    while let Some(msg) = rx.recv().await {
        let tx_ui = tx_to_ui.clone();
        let auth = spotify_auth_client.clone();
        let client = spotify_client.clone();
        let lyrics = lyrics_fetcher.clone();

        // Start a new thread which handles our message, and the required response.
        // A message returns a (MessageToUI, and a MessageToRT), so an action can
        // trigger an update of the UI, or trigger a new action.
        tokio::spawn(async move {
            let res = match msg {
                MessageToRT::Authenticate => authenticate(auth).await,
                MessageToRT::InvalidateToken => invalidate(auth).await,
                MessageToRT::GetCurrentTrack => get_current_track(client).await,
                MessageToRT::GetLyrics(request) => lyrics.get_lyrics(request).await,
            };

            match res {
                Ok(msg) => {
                    msg.send(tx_ui).await;
                }
                Err(x) => {
                    tx_ui
                        .send(MessageToUI::DisplayError(format!("{x:?}")))
                        .await
                        .unwrap();
                }
            };
        });
    }
    trace!("Reached end of runtime");
}

async fn get_current_track(spotify_client: Arc<SpotifyClient>) -> Result<Messages, RuntimeError> {
    process_current_track_response(spotify_client.get_current_track().await).await
}

async fn authenticate(
    spotify_auth_client: Arc<TokioMutex<SpotifyAuthClient>>,
) -> Result<Messages, RuntimeError> {
    debug!("Starting authentication");
    let res = spotify_auth_client.lock().await.authenticate().await;
    match res {
        Ok(()) => Ok(Messages::to_ui(MessageToUI::AuthenticationStateUpdate(
            true,
        ))),
        Err(err) => Err(RuntimeError::AuthenticationFailed(err)),
    }
}

async fn invalidate(
    spotify_auth_client: Arc<TokioMutex<SpotifyAuthClient>>,
) -> Result<Messages, RuntimeError> {
    debug!("Invalidating authentication");
    spotify_auth_client.lock().await.invalidate_token().await;
    Ok(Messages::to_ui(MessageToUI::AuthenticationStateUpdate(
        false,
    )))
}
