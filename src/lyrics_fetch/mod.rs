//! Module for fetching (cached) lyrics files for songs

use std::{
    fmt::Display,
    sync::{Arc, RwLock},
};

use tracing::{debug, error};

use thiserror::Error;
use tracing::trace;

use crate::{
    MessageToUI,
    lyrics_fetch::cache::LyricsCacheCheckErr,
    lyrics_parser::{SongLyrics, parse_lrc},
    runtime::{Messages, RuntimeError},
    settings::Settings,
    spotify::CurrentlyPlayingResponse,
};

mod cache;
mod lrc;
mod spotify;

pub struct LyricsFetcher {
    client: reqwest::Client,
    settings: Arc<RwLock<Settings>>,
}

#[derive(Error, Debug)]
pub enum LyricsFetcherErr {
    #[error("Reqwest error: {0}")]
    ReqwestError(#[from] reqwest::Error),
    #[error("Json: {0}")]
    JsonError(#[from] serde_json::Error),
    #[error("No track in current response for fetcher")]
    NoTrack(),
    #[error("Song lyrics could not be found")]
    SongLyricsNotFound(),
}

#[derive(Debug)]
pub struct SongWithLyrics {
    pub lyrics: SongLyrics,
    duration_sec: f64,
    track_name: String,
    artist_name: String,
    album_name: String,
}

impl Display for SongWithLyrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "Lyrics for {} - {}. From {}, {}s",
            self.track_name, self.artist_name, self.album_name, self.duration_sec
        ))
    }
}
impl SongWithLyrics {
    pub fn new(lyrics: SongLyrics, req: LyricsRequestInfo) -> Self {
        Self {
            lyrics,
            duration_sec: req.duration_sec,
            track_name: req.track_name,
            artist_name: req.artist_name,
            album_name: req.album_name,
        }
    }
}

#[derive(Error, Debug, Clone)]
pub struct LyricsRequestInfo {
    spotify_id: Option<String>,
    duration_sec: f64,
    track_name: String,
    artist_name: String,
    album_name: String,
}
impl Display for LyricsRequestInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "{} - {}. From {}, {}s",
            self.track_name, self.artist_name, self.album_name, self.duration_sec
        ))
    }
}
impl LyricsRequestInfo {
    pub fn from_spotify_response(
        response: &CurrentlyPlayingResponse,
    ) -> Result<Self, LyricsFetcherErr> {
        if !response.is_track() {
            return Err(LyricsFetcherErr::NoTrack());
        }

        // we can safely unwrap here because all these fields are valid if response is a track
        Ok(Self {
            spotify_id: Some(response.get_spotify_id().unwrap()),
            duration_sec: response.get_duration_sec().unwrap(),
            track_name: response.get_track_title().unwrap(),
            artist_name: response.get_artist().unwrap(),
            album_name: response.get_album().unwrap(),
        })
    }

    pub fn get_track_identifier(&self) -> String {
        format!(
            "{}-{}.{}",
            self.track_name.clone(),
            self.artist_name.clone(),
            self.duration_sec.clone()
        )
    }
}

impl LyricsFetcher {
    pub fn new(settings: Arc<RwLock<Settings>>) -> Self {
        Self {
            client: {
                reqwest::Client::builder()
                    //  .user_agent(super::APP_USER_AGENT)
                    .build()
                    .unwrap()
            },
            settings,
        }
    }

    pub async fn get_lyrics(&self, req: LyricsRequestInfo) -> Result<Messages, RuntimeError> {
        if self.settings.read().unwrap().caching_enabled {
            let cache_res = self.check_cache(&req);
            match cache_res {
                Ok(lyrics) => {
                    return Ok(Messages::to_ui(MessageToUI::GotLyrics(
                        SongWithLyrics::new(lyrics, req),
                    )));
                }
                Err(cache_err) => match cache_err {
                    LyricsCacheCheckErr::NotInCache() => (),
                    _ => {
                        trace!("{cache_err}");
                    }
                },
            }
        }

        // Try Spotify first
        if let Some(ref spotify_id) = req.spotify_id {
            match self.request_track_spotify(spotify_id).await {
                Ok(parsed) => {
                    debug!("Succesfully retreived parsed spotify lyrics");
                    let cache_store_res = self.store_in_cache(&req, None, &parsed);
                    if let Err(cache_err) = cache_store_res {
                        error!("Failed creating cache entry: {:?}", cache_err);
                    }
                    return Ok(Messages::to_ui(MessageToUI::GotLyrics(
                        SongWithLyrics::new(parsed, req),
                    )));
                }
                Err(e) => trace!("Spotify lyrics unavailable, falling back to LRCLib: {e}"),
            }
        }

        let lrc_response = self
            .request_track_lrc(
                &req.duration_sec,
                &req.track_name,
                &req.artist_name,
                &req.album_name,
            )
            .await;

        let lrc_response = match lrc_response {
            Ok(value) => value,
            Err(err) => {
                return Ok(Messages::to_ui(MessageToUI::DisplayError(format!(
                    "Failed to fetch lyrics: LRC: {err}"
                ))));
            }
        };

        let parsed = parse_lrc(&lrc_response.synced_lyrics, false);

        let cache_store_res = self.store_in_cache(&req, Some(lrc_response.id), &parsed);
        if let Err(cache_err) = cache_store_res {
            error!("Failed creating cache entry: {:?}", cache_err);
        }

        Ok(Messages::to_ui(MessageToUI::GotLyrics(
            SongWithLyrics::new(parsed, req),
        )))
    }
}
