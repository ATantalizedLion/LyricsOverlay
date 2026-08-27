#![warn(clippy::pedantic)]

use std::sync::Arc;

use tokio::sync::Mutex as TokioMutex;
use tokio::sync::RwLock as TokioRwLock;
use tokio::sync::mpsc;

use tracing::error;
use tracing::info;
use tracing::{debug, trace};

use crate::MessageToRT;
use crate::MessageToUI;
use crate::lyrics_fetch::LyricsFetcher;
use crate::lyrics_fetch::LyricsFetcherErr;
use crate::settings::NowPlayingSource;
use crate::settings::Settings;
use crate::smtc::SmtcClient;
use crate::smtc::SmtcError;
use crate::smtc::poller::SmtcPoller;
use crate::spotify::SpotifyClient;
use crate::spotify::SpotifyClientTrackError;
use crate::spotify::auth::SpotifyAuthClient;
use crate::spotify::auth::SpotifyClientAuthError;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("Authentication failed: {0}")]
    Authentication(#[from] SpotifyClientAuthError),
    #[error("Getting lyrics failed: {0}")]
    Get(#[from] LyricsFetcherErr),
    #[error("Media control command failed: {0}")]
    Smtc(#[from] SmtcError),
    #[error("Spotify playback command failed: {0}")]
    Spotify(#[from] SpotifyClientTrackError),
}

/// The four playback controls, dispatched to whichever source is currently active - see
/// `dispatch_playback`.
#[derive(Clone, Copy)]
enum PlaybackAction {
    Play,
    Pause,
    Next,
    Previous,
}

impl PlaybackAction {
    async fn run_on_smtc(self, client: &SmtcClient) -> Result<(), SmtcError> {
        match self {
            Self::Play => client.play().await,
            Self::Pause => client.pause().await,
            Self::Next => client.next().await,
            Self::Previous => client.previous().await,
        }
    }

    async fn run_on_spotify(self, client: &SpotifyClient) -> Result<(), SpotifyClientTrackError> {
        match self {
            Self::Play => client.play().await,
            Self::Pause => client.pause().await,
            Self::Next => client.next_track().await,
            Self::Previous => client.previous_track().await,
        }
    }
}

/// Routes a playback command the same way the poller routes now-playing reads, per the
/// user's `NowPlayingSource` preference: the preferred source is used if it can (SMTC:
/// has a local session; Spotify: attempted directly, since there's no cheap "is there
/// anything" check), otherwise the other source is tried as a fallback.
async fn dispatch_playback(
    action: PlaybackAction,
    smtc: Option<Arc<SmtcClient>>,
    spotify: Arc<SpotifyClient>,
    settings: Arc<TokioRwLock<Settings>>,
) -> Result<Messages, RuntimeError> {
    let preference = settings.read().await.now_playing_source;

    match preference {
        NowPlayingSource::SmtcOnly => run_on_smtc(action, smtc.as_deref()).await,
        NowPlayingSource::PreferSmtc => {
            if smtc.as_ref().is_some_and(|c| c.has_active_session()) {
                run_on_smtc(action, smtc.as_deref()).await
            } else {
                spotify_result(action.run_on_spotify(&spotify).await)
            }
        }
        NowPlayingSource::PreferSpotify => match action.run_on_spotify(&spotify).await {
            // Nothing there for Spotify to act on - fall back to SMTC rather than
            // surfacing this as an error. Any other failure (auth/rate-limit/network) is
            // a real problem worth reporting, not silently masked by a fallback attempt.
            Err(
                SpotifyClientTrackError::NotAuthenticated
                | SpotifyClientTrackError::NoContentResponse
                | SpotifyClientTrackError::NotATrack,
            ) => run_on_smtc(action, smtc.as_deref()).await,
            result => spotify_result(result),
        },
    }
}

async fn run_on_smtc(
    action: PlaybackAction,
    smtc: Option<&SmtcClient>,
) -> Result<Messages, RuntimeError> {
    match smtc {
        Some(client) => smtc_result(action.run_on_smtc(client).await),
        None => Ok(no_smtc_error()),
    }
}

fn spotify_result(result: Result<(), SpotifyClientTrackError>) -> Result<Messages, RuntimeError> {
    match result {
        Ok(()) => Ok(Messages::none()),
        Err(e) => Err(RuntimeError::Spotify(e)),
    }
}

/// Struct to possibly allow handling different types of messages in a send or receive loop
#[derive(Debug)]
pub struct Messages {
    to_ui: Option<MessageToUI>,
}
impl Messages {
    pub fn to_ui(to_ui: MessageToUI) -> Self {
        Self { to_ui: Some(to_ui) }
    }
    /// No UI update needed - e.g. a control command succeeded and the next SMTC poll
    /// tick will reflect the new state shortly anyway.
    pub fn none() -> Self {
        Self { to_ui: None }
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

    // Windows System Media Transport Controls: works with whatever's playing anywhere,
    // no auth needed. On the rare init failure, the app degrades to showing an error
    // rather than crashing - nothing else here depends on it having succeeded.
    let smtc_client = match SmtcClient::new().await {
        Ok(client) => Some(Arc::new(client)),
        Err(e) => {
            error!("Failed to initialize Windows media controls: {e}");
            tx_to_ui
                .send(MessageToUI::DisplayError(format!(
                    "Failed to access Windows media controls: {e}"
                )))
                .await
                .unwrap();
            None
        }
    };

    if let Some(smtc) = smtc_client.clone() {
        let poller = SmtcPoller::new(smtc, spotify_client.clone(), settings.clone());
        tokio::spawn(poller.run(tx_to_ui.clone()));
    }

    if settings.read().await.auto_auth
        && !settings.read().await.client_id.is_empty()
        && !settings.read().await.client_secret.is_empty()
    {
        tx_to_rt.send(MessageToRT::Authenticate).await.unwrap();
    }

    while let Some(msg) = rx.recv().await {
        let tx_ui = tx_to_ui.clone();
        let auth = spotify_auth_client.clone();
        let lyrics = lyrics_fetcher.clone();
        let smtc = smtc_client.clone();
        let spotify = spotify_client.clone();
        let settings = settings.clone();

        // Start a new thread which handles our message, and the required response.
        // A message returns a (MessageToUI, and a MessageToRT), so an action can
        // trigger an update of the UI, or trigger a new action.
        tokio::spawn(async move {
            let res = match msg {
                MessageToRT::Authenticate => authenticate(auth).await,
                MessageToRT::InvalidateToken => invalidate(auth).await,
                MessageToRT::GetLyrics(request) => lyrics.get_lyrics(request).await,
                MessageToRT::Play => {
                    dispatch_playback(PlaybackAction::Play, smtc, spotify, settings).await
                }
                MessageToRT::Pause => {
                    dispatch_playback(PlaybackAction::Pause, smtc, spotify, settings).await
                }
                MessageToRT::NextTrack => {
                    dispatch_playback(PlaybackAction::Next, smtc, spotify, settings).await
                }
                MessageToRT::PreviousTrack => {
                    dispatch_playback(PlaybackAction::Previous, smtc, spotify, settings).await
                }
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
            }
        });
    }
    trace!("Reached end of runtime");
}

fn smtc_result(result: Result<(), SmtcError>) -> Result<Messages, RuntimeError> {
    match result {
        Ok(()) => Ok(Messages::none()),
        Err(e) => Err(RuntimeError::Smtc(e)),
    }
}

fn no_smtc_error() -> Messages {
    Messages::to_ui(MessageToUI::DisplayError(
        "Windows media controls unavailable".to_owned(),
    ))
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
        Err(err) => Err(RuntimeError::Authentication(err)),
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
