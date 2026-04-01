//! Module for talking with spotify, implements only the parts of the API needed for this app
use serde::Deserialize;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock as TokioRwLock;
use tracing::trace;

pub mod auth;
pub mod poller;

#[derive(Error, Debug)]
/// Error enum for spotify requests
pub enum SpotifyClientTrackError {
    #[error("Not authenticated")]
    NotAuthenticated,
    #[error("Not playing a track")]
    NotATrack,
    #[error("Not playing anything")]
    NoContentResponse,
    #[error("OAuthError, try reauthenticating")]
    TokenError,
    #[error("BadRequest, reauthentication won't help you, I don't know what will")]
    BadRequest,
    #[error("Exceeded spotify rate limits")]
    RateLimitsExceeded,
    #[error("Reqwest error: {0}")]
    ReqwestError(#[from] reqwest::Error),
}

#[derive(Debug, Deserialize, Clone)]
/// (Partial) Response of the spotify currently playing song endpoint
pub struct CurrentlyPlayingResponse {
    /// Type of the included item, we only care if this matches "track"
    currently_playing_type: String,
    /// Item, can also be a podcast ep, but we only care about track
    item: Option<Track>,
    /// Are we currently playing this song?
    pub is_playing: bool,
    /// Playback progress
    pub progress_ms: usize,
}

impl CurrentlyPlayingResponse {
    pub fn is_track(&self) -> bool {
        self.currently_playing_type == "track" && self.item.is_some()
    }
    pub fn get_track_title(&self) -> Option<String> {
        self.item.as_ref().map(|track| track.name.clone())
    }
    pub fn get_artist(&self) -> Option<String> {
        self.item.as_ref().map(|track| track.get_artist().clone())
    }
    pub fn get_album(&self) -> Option<String> {
        self.item.as_ref().map(|track| track.get_album().clone())
    }
    pub fn get_duration_sec(&self) -> Option<f64> {
        self.item.as_ref().map(Track::get_duration_sec)
    }
    pub fn get_spotify_id(&self) -> Option<String> {
        self.item.as_ref().map(|track| track.id.clone())
    }
}

#[derive(Debug, Deserialize, Clone)]
/// (Partial) Contents of the track item of the spotify API
struct Track {
    /// Song title
    name: String,
    /// Spotify song id
    id: String,
    /// Duration in ms of the song
    duration_ms: usize,
    /// Artists listed for this song
    artists: Vec<Artist>,
    /// Song's album
    album: Album,
}
impl Track {
    fn get_artist(&self) -> String {
        self.artists.first().unwrap().name.clone()
    }
    fn get_album(&self) -> String {
        self.album.name.clone()
    }
    #[allow(clippy::cast_precision_loss)]
    fn get_duration_sec(&self) -> f64 {
        self.duration_ms as f64 / 1000.0
    }
}

#[derive(Debug, Deserialize, Clone)]
/// (Partial) Contents of the artist item of the spotify API
struct Artist {
    /// Artist name
    name: String,
}

#[derive(Debug, Deserialize, Clone)]
/// (Partial) Contents of the album item of the spotify API
struct Album {
    /// Album name
    name: String,
}

/// Spotify client state
pub struct SpotifyClient {
    /// Our very important amazing access token
    access_token: Arc<TokioRwLock<Option<String>>>,
    /// Client used for requests (not used in oauth request)
    client: reqwest::Client,
}

impl SpotifyClient {
    pub fn new(access_token: Arc<TokioRwLock<Option<String>>>) -> Self {
        Self {
            access_token,
            client: reqwest::Client::new(),
        }
    }

    pub async fn get_current_track(
        &self,
    ) -> Result<CurrentlyPlayingResponse, SpotifyClientTrackError> {
        let token_opt = self.access_token.read().await.clone();

        let Some(token) = token_opt else {
            return Err(SpotifyClientTrackError::NotAuthenticated);
        };

        let response: reqwest::Response = self
            .client
            .get("https://api.spotify.com/v1/me/player/currently-playing")
            .bearer_auth(token)
            .send()
            .await?;

        if response.status().as_u16() == 204 {
            // No content - nothing playing
            return Err(SpotifyClientTrackError::NoContentResponse);
        }
        if response.status().as_u16() == 401 {
            // Bad or expired token. This can happen if the user revoked a token or the access token has expired. You should re-authenticate the user.
            return Err(SpotifyClientTrackError::TokenError);
        }
        if response.status().as_u16() == 403 {
            // Bad OAuth request (wrong consumer key, bad nonce, expired timestamp...). Unfortunately, re-authenticating the user won't help here.
            return Err(SpotifyClientTrackError::BadRequest);
        }
        if response.status().as_u16() == 429 {
            // The app has exceeded its rate limits.
            // According to the internet, "100 requests per hour for each user token and 25 requests per second for each application token."
            // But spotify is vague about this
            return Err(SpotifyClientTrackError::RateLimitsExceeded);
        }

        let playing: CurrentlyPlayingResponse = response.json().await?;

        trace!("CurrentlyPlayingResponse {playing:?}");

        if playing.currently_playing_type != "track" {
            return Err(SpotifyClientTrackError::NotATrack);
        }

        Ok(playing)
    }
}
