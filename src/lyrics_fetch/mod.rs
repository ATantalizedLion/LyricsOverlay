//! Module for fetching (cached) lyrics files for songs

use std::{
    fmt::Display,
    path::{Path, PathBuf},
    sync::Arc,
};

use tracing::{debug, error, warn};

use tokio::sync::RwLock as TokioRwLock;

use thiserror::Error;
use tracing::trace;

use crate::{
    MessageToUI,
    lyrics_fetch::cache::LyricsCacheCheckErr,
    lyrics_parser::{SongLyrics, parse_lrc},
    translation::Translation,
    now_playing::NowPlaying,
    paths,
    runtime::{Messages, RuntimeError},
    settings::Settings,
};

mod cache;
mod lrc;
mod spotify;

pub struct LyricsFetcher {
    client: reqwest::Client,
    settings: Arc<TokioRwLock<Settings>>,
}

#[derive(Error, Debug)]
pub enum LyricsFetcherErr {
    #[error("Reqwest error: {0}")]
    ReqwestError(#[from] reqwest::Error),
    #[error("Json: {0}")]
    JsonError(#[from] serde_json::Error),
    #[error("Song lyrics could not be found")]
    SongLyricsNotFound(),
}

#[derive(Debug)]
pub struct SongWithLyrics {
    pub lyrics: SongLyrics,
    pub duration_sec: f64,
    pub track_name: String,
    pub artist_name: String,
    album_name: String,
    /// See `LyricsRequestInfo::get_track_identifier`
    pub track_identifier: String,
    /// Arrives separately after the lyrics themselves, if translation applies at all
    pub translation: Option<Translation>,
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
            track_identifier: req.get_track_identifier(),
            translation: None,
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
    pub fn from_now_playing(now_playing: &NowPlaying) -> Self {
        Self {
            spotify_id: now_playing.spotify_id.clone(),
            duration_sec: now_playing.duration_sec,
            track_name: now_playing.track_title.clone(),
            artist_name: now_playing.artist.clone(),
            album_name: now_playing.album.clone(),
        }
    }

    pub fn get_track_identifier(&self) -> String {
        format!("{}{}s", self.identifier_prefix(), self.duration_sec)
    }

    /// Everything in the identifier except the duration.
    fn identifier_prefix(&self) -> String {
        format!(
            "{}-{} ({}) ",
            self.artist_name, self.track_name, self.album_name
        )
    }

    /// The track's cache folder under `cache_root`. The duration Windows reports for the
    /// same song can differ by a millisecond or so between plays (e.g. 213.906s vs
    /// 213.907s), so if a folder for this track with a duration within a couple of
    /// seconds already exists, this adopts that folder's duration. That keeps the lyrics,
    /// translations and recordings of one song in a single folder.
    pub fn match_existing_cache_entry(&mut self, cache_root: &Path) {
        const MAX_DURATION_DIFF_SEC: f64 = 2.0;
        if cache_root.join(self.get_track_identifier()).exists() {
            return;
        }
        let prefix = self.identifier_prefix();
        let Ok(entries) = std::fs::read_dir(cache_root) else {
            return;
        };
        let closest = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let duration: f64 = name.strip_prefix(&prefix)?.strip_suffix('s')?.parse().ok()?;
                Some(duration)
            })
            .filter(|d| (d - self.duration_sec).abs() <= MAX_DURATION_DIFF_SEC)
            .min_by(|a, b| {
                (a - self.duration_sec)
                    .abs()
                    .total_cmp(&(b - self.duration_sec).abs())
            });
        if let Some(duration) = closest {
            self.duration_sec = duration;
        }
    }

    pub fn cache_dir(&self, cache_root: &Path) -> PathBuf {
        cache_root.join(self.get_track_identifier())
    }
}

impl LyricsFetcher {
    pub fn new(settings: Arc<TokioRwLock<Settings>>) -> Self {
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

    pub async fn get_lyrics(&self, mut req: LyricsRequestInfo) -> Result<Messages, RuntimeError> {
        if self.settings.read().await.caching_enabled {
            let cache_root = paths::resolve(&self.settings.read().await.cache_folder);
            req.match_existing_cache_entry(&cache_root);
            let cache_res = self.check_cache(&req).await;
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
                    let cache_store_res = self.store_in_cache(&req, None, &parsed).await;
                    if let Err(cache_err) = cache_store_res {
                        error!("Failed creating cache entry: {:?}", cache_err);
                    }
                    return Ok(Messages::to_ui(MessageToUI::GotLyrics(
                        SongWithLyrics::new(parsed, req),
                    )));
                }
                Err(e) => warn!("Spotify lyrics unavailable, falling back to LRCLib: {e}"),
            }
        }

        match self
            .request_track_lrc(
                &req.duration_sec,
                &req.track_name,
                &req.artist_name,
                &req.album_name,
            )
            .await
        {
            Ok(lrc_response) => {
                let synced = lrc_response
                    .synced_lyrics
                    .as_deref()
                    .filter(|s| !s.trim().is_empty());
                let plain = lrc_response
                    .plain_lyrics
                    .as_deref()
                    .filter(|s| !s.trim().is_empty());

                let parsed = if let Some(synced) = synced {
                    Some(parse_lrc(synced, false))
                } else if let Some(plain) = plain {
                    debug!("LRC has no synced lyrics, falling back to plain lyrics");
                    Some(SongLyrics::plain(
                        plain.lines().map(str::to_owned).collect(),
                    ))
                } else {
                    // Instrumental or genuinely no lyrics text - fall through to the
                    // "could not find lyrics" message below.
                    None
                };

                if let Some(parsed) = parsed {
                    let cache_store_res = self
                        .store_in_cache(&req, Some(lrc_response.id), &parsed)
                        .await;
                    if let Err(cache_err) = cache_store_res {
                        error!("Failed creating cache entry: {:?}", cache_err);
                    }
                    return Ok(Messages::to_ui(MessageToUI::GotLyrics(
                        SongWithLyrics::new(parsed, req),
                    )));
                }
            }
            Err(err) => {
                warn!("Failed to fetch lyrics from LRC: {err}");
            }
        }

        #[allow(clippy::cast_possible_truncation)]
        #[allow(clippy::cast_sign_loss)]
        Ok(Messages::to_ui(MessageToUI::GotLyrics(
            SongWithLyrics::new(
                SongLyrics::display_text_as_lyrics(
                    "Could not find lyrics for this song".to_owned(),
                    (req.duration_sec * 1000.) as usize,
                ),
                req,
            ),
        )))
    }
}
