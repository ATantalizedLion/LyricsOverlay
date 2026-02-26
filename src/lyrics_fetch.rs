//! Module for fetching (cached) lyrics files for songs
//!
//! For now, with spotify integration in mind, we store based on spotify ID
//! Add a meta data file with extra track info. Maybe even store custom offsets here

use std::{fmt::Display, fs, io::Write, path::Path, sync::Arc};
use tracing::{debug, error};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::trace;

use crate::{
    MessageToUI,
    lyrics_parser::{SongLyrics, parse_lrc},
    runtime::RuntimeError,
    settings::Settings,
    spotify::CurrentlyPlayingResponse,
};

const LRC_LIB_URL: &str = "https://lrclib.net/api/get";

pub struct LyricsFetcher {
    client: reqwest::Client,
    settings: Arc<Settings>,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct LRCOkResponse {
    /// LRC ID
    pub id: usize,
    pub track_name: String,
    pub artist_name: String,
    pub album_name: String,
    pub duration: f32,
    pub instrumental: bool,
    pub plain_lyrics: String,
    pub synced_lyrics: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct LrcCacheMeta {
    pub spotify_id: Option<String>,
    pub lrc_id: usize,
    pub track_name: String,
    pub artist_name: String,
    pub album_name: String,
    pub duration_sec: f32,
    pub instrumental: bool,
}

#[derive(Error, Debug)]
pub enum LyricsFetcherErr {
    #[error("Reqwest error: {0}")]
    ReqwestError(#[from] reqwest::Error),
    #[error("No track in current response for fetcher")]
    NoTrack(),
}

#[derive(Error, Debug)]
pub enum LyricsCacheCheckErr {
    #[error("IO error")]
    IoError(#[from] std::io::Error),
    #[error("Track not found in cache")]
    NotInCache(),
    #[error("Serialization failed")]
    Serde(#[from] serde_json::Error),
}
#[derive(Error, Debug)]
pub enum LyricsCacheCreateErr {
    #[error("IO error")]
    IoError(#[from] std::io::Error),
    #[error("Could not serialize new cache entry")]
    SerializeErr(#[from] serde_json::Error),
}

#[derive(Error, Debug)]
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

        // we can 'safely' unwrap here because all these fields are valid if response is a track
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
    pub fn new(settings: Arc<Settings>) -> Self {
        Self {
            client: {
                reqwest::Client::builder()
                    .user_agent(super::APP_USER_AGENT)
                    .build()
                    .unwrap()
            },
            settings,
        }
    }

    fn check_cache(&self, req: &LyricsRequestInfo) -> Result<SongLyrics, LyricsCacheCheckErr> {
        trace!("Checking cache for {req}");
        let cache_folder = Path::new(&self.settings.cache_folder);
        let track_folder = Path::join(cache_folder, req.get_track_identifier());
        let lrc_file_path = Path::join(&track_folder, "lyrics.lrc");

        if !fs::exists(&lrc_file_path)? {
            return Err(LyricsCacheCheckErr::NotInCache());
        }

        let lrc_file = fs::File::create(lrc_file_path)?;

        let lyrics: SongLyrics = serde_json::from_reader(lrc_file)?;

        Ok(lyrics)
    }

    fn store_in_cache(
        &self,
        req: &LyricsRequestInfo,
        resp: LRCOkResponse,
        song_lyrics: &SongLyrics,
    ) -> Result<(), LyricsCacheCreateErr> {
        trace!("Creating cache entry for {req}");
        let cache_folder = Path::new(&self.settings.cache_folder);
        let track_folder = Path::join(cache_folder, req.get_track_identifier());
        trace!("Cache dir: {track_folder:?}");

        let meta = LrcCacheMeta {
            spotify_id: req.spotify_id.clone(),
            lrc_id: resp.id,
            track_name: resp.track_name,
            artist_name: resp.artist_name,
            album_name: resp.album_name,
            duration_sec: resp.duration,
            instrumental: resp.instrumental,
        };

        fs::create_dir_all(&track_folder)?;
        let mut meta_file = fs::File::create(Path::join(&track_folder, ".meta"))?;
        let meta_file_str = serde_json::to_string_pretty(&meta)?;
        write!(meta_file, "{meta_file_str}").unwrap();

        let mut lrc_file = fs::File::create(Path::join(&track_folder, "lyrics.lrc"))?;
        let synced_lyrics_str = serde_json::to_string_pretty(&song_lyrics)?;
        write!(lrc_file, "{synced_lyrics_str}").unwrap();

        Ok(())
    }

    pub async fn get_lyrics(&self, req: LyricsRequestInfo) -> Result<MessageToUI, RuntimeError> {
        if self.settings.caching_enabled {
            let cache_res = self.check_cache(&req);
            match cache_res {
                Ok(lyrics) => return Ok(MessageToUI::GotLyrics(SongWithLyrics::new(lyrics, req))),
                Err(cache_err) => match cache_err {
                    LyricsCacheCheckErr::NotInCache() => (),
                    _ => {
                        trace!("{cache_err}");
                    }
                },
            }
        }

        let lrc_response = self
            .request_track(
                &req.duration_sec,
                &req.track_name,
                &req.artist_name,
                &req.album_name,
            )
            .await;

        let lrc_response = match lrc_response {
            Ok(value) => value,
            Err(err) => {
                return Ok(MessageToUI::DisplayError(format!(
                    "Failed to fetch lyrics: {err}"
                )));
            }
        };

        let parsed = parse_lrc(&lrc_response.synced_lyrics, false);

        let cache_store_res = self.store_in_cache(&req, lrc_response, &parsed);
        if let Err(cache_err) = cache_store_res {
            error!("Failed creating cache entry: {:?}", cache_err);
        }

        Ok(MessageToUI::GotLyrics(SongWithLyrics::new(parsed, req)))
    }

    async fn request_track(
        &self,
        duration_sec: &f64,
        track_name: &str,
        artist_name: &str,
        album_name: &str,
    ) -> Result<LRCOkResponse, LyricsFetcherErr> {
        let url = format!(
            "{LRC_LIB_URL}?artist_name={artist_name}&track_name={track_name}&album_name={album_name}&duration={duration_sec}"
        );
        let response: reqwest::Response = self.client.get(url).send().await?;

        debug!("Response for track request: {:?}", response);

        //TODO: Sane handling of instrumental songs / could not find lyrics
        let lyrics: LRCOkResponse = response.json().await?;

        trace!("Response for track request: {:?}", lyrics);

        Ok(lyrics)
    }
}
