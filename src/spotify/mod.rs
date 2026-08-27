//! Module for talking with spotify, implements only the parts of the API needed for this app
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;
use tokio::sync::RwLock as TokioRwLock;
use tracing::trace;

use crate::now_playing::NowPlaying;

pub mod auth;

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
    #[error("Forbidden - token lacks the required permission, reauthenticating needed")]
    Forbidden,
    #[error("BadRequest, reauthentication won't help you, I don't know what will")]
    BadRequest,
    #[error("Exceeded spotify rate limits")]
    RateLimitsExceeded,
    #[error("Reqwest error: {0}")]
    ReqwestError(#[from] reqwest::Error),
}

#[derive(Debug, Deserialize, Clone)]
/// (Partial) Response of the spotify currently playing song endpoint. Used both to
/// cross-reference an SMTC-detected Spotify session against a Spotify track id for the
/// bonus lyrics lookup, and - when the user's opted into the Spotify fallback source - as
/// a full now-playing reading in its own right, for playback SMTC can't see locally (e.g.
/// Spotify Connect streaming to another device). See `crate::smtc::poller`.
pub(crate) struct CurrentlyPlayingResponse {
    /// Type of the included item, we only care if this matches "track"
    currently_playing_type: String,
    /// Item, can also be a podcast ep, but we only care about track
    item: Option<Track>,
    /// Are we currently playing this song?
    is_playing: bool,
    /// Playback progress
    progress_ms: usize,
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

#[derive(Debug, Deserialize)]
/// Error body Spotify sends back for a failed player command (play/pause/next/previous),
/// e.g. `{"error": {"status": 403, "reason": "PREMIUM_REQUIRED", "message": "..."}}`.
/// `reason` is undocumented but the values seen in practice are what we key off of.
struct PlayerErrorBody {
    error: PlayerErrorDetail,
}
#[derive(Debug, Deserialize)]
struct PlayerErrorDetail {
    #[serde(default)]
    reason: String,
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

    /// Shared by `get_current_track` and `get_now_playing`: fetches the raw response plus
    /// the `Instant` its `progress_ms` was accurate for, compensated for request
    /// round-trip latency (Spotify measures progress roughly when it receives our
    /// request, i.e. about halfway through the round trip, not when we finish receiving
    /// the response).
    async fn fetch(&self) -> Result<(CurrentlyPlayingResponse, Instant), SpotifyClientTrackError> {
        let token_opt = self.access_token.read().await.clone();

        let Some(token) = token_opt else {
            return Err(SpotifyClientTrackError::NotAuthenticated);
        };

        let request_sent = Instant::now();
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
            // Token doesn't carry the required scope, e.g. a refresh token minted before a
            // newly required scope was added. Re-authenticating fixes this.
            return Err(SpotifyClientTrackError::Forbidden);
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

        let round_trip = request_sent.elapsed();
        let measured_at = Instant::now()
            .checked_sub(round_trip / 2)
            .unwrap_or_else(Instant::now);

        Ok((playing, measured_at))
    }

    pub async fn get_current_track(
        &self,
    ) -> Result<CurrentlyPlayingResponse, SpotifyClientTrackError> {
        self.fetch().await.map(|(response, _)| response)
    }

    /// Full now-playing reading sourced from Spotify's own API, for the opt-in fallback
    /// path used when SMTC has no local session to read from (e.g. playback via Spotify
    /// Connect on another device/speaker).
    pub async fn get_now_playing(&self) -> Result<NowPlaying, SpotifyClientTrackError> {
        let (response, measured_at) = self.fetch().await?;
        let item = response.item.ok_or(SpotifyClientTrackError::NotATrack)?;

        Ok(NowPlaying {
            track_title: item.name.clone(),
            artist: item.get_artist(),
            album: item.get_album(),
            duration_sec: item.get_duration_sec(),
            is_playing: response.is_playing,
            progress_ms: response.progress_ms,
            measured_at,
            spotify_id: Some(item.id),
        })
    }

    async fn send_player_command(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<(), SpotifyClientTrackError> {
        let token_opt = self.access_token.read().await.clone();
        let Some(token) = token_opt else {
            return Err(SpotifyClientTrackError::NotAuthenticated);
        };

        let response = self
            .client
            .request(method, format!("https://api.spotify.com/v1/me/player/{path}"))
            .bearer_auth(token)
            .send()
            .await?;

        let status = response.status().as_u16();
        if status == 403 {
            // Spotify's play/pause/next/previous endpoints are documented to return 403
            // for a *redundant* command (e.g. Play while already playing, or controlling
            // a session on another device) even though playback ends up in the requested
            // state regardless - not an auth/permission problem. Reauthenticating won't
            // "fix" that, so only treat it as a real error when the body says the account
            // genuinely lacks Premium (the one 403 reason that IS a real, persistent
            // problem). See https://github.com/spotify/web-api/issues/776.
            let reason = response
                .json::<PlayerErrorBody>()
                .await
                .ok()
                .map(|body| body.error.reason);
            return if reason.as_deref() == Some("PREMIUM_REQUIRED") {
                Err(SpotifyClientTrackError::Forbidden)
            } else {
                Ok(())
            };
        }

        match status {
            204 => Ok(()),
            401 => Err(SpotifyClientTrackError::TokenError),
            429 => Err(SpotifyClientTrackError::RateLimitsExceeded),
            _ => Err(SpotifyClientTrackError::BadRequest),
        }
    }

    pub async fn play(&self) -> Result<(), SpotifyClientTrackError> {
        self.send_player_command(reqwest::Method::PUT, "play").await
    }
    pub async fn pause(&self) -> Result<(), SpotifyClientTrackError> {
        self.send_player_command(reqwest::Method::PUT, "pause").await
    }
    pub async fn next_track(&self) -> Result<(), SpotifyClientTrackError> {
        self.send_player_command(reqwest::Method::POST, "next").await
    }
    pub async fn previous_track(&self) -> Result<(), SpotifyClientTrackError> {
        self.send_player_command(reqwest::Method::POST, "previous")
            .await
    }
}
