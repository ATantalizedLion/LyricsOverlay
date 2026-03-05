//! Caching module for the fetched lyrics, so we don't spam all our friendly APIs

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::trace;

use crate::{
    lyrics_fetch::{LyricsFetcher, LyricsRequestInfo},
    lyrics_parser::SongLyrics,
};

#[derive(Deserialize, Serialize, Debug)]
struct LyricCacheMeta {
    pub spotify_id: Option<String>,
    pub lrc_id: Option<usize>,
    pub track_name: String,
    pub artist_name: String,
    pub album_name: String,
    pub duration_sec: f64,
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

impl LyricsFetcher {
    fn track_cache_dir(&self, req: &LyricsRequestInfo) -> PathBuf {
        let binding = self.settings.read().unwrap().cache_folder.clone();
        Path::new(&binding).join(req.get_track_identifier())
    }

    pub(super) fn check_cache(
        &self,
        req: &LyricsRequestInfo,
    ) -> Result<SongLyrics, LyricsCacheCheckErr> {
        trace!("Checking cache for {req}");
        let lrc_file_path = self.track_cache_dir(req).join("lyrics.lrc");

        if !fs::exists(&lrc_file_path)? {
            return Err(LyricsCacheCheckErr::NotInCache());
        }

        let lrc_file = fs::File::open(lrc_file_path)?;

        let lyrics: SongLyrics = serde_json::from_reader(lrc_file)?;

        Ok(lyrics)
    }

    pub(super) fn store_in_cache(
        &self,
        req: &LyricsRequestInfo,
        lrc_id: Option<usize>,
        song_lyrics: &SongLyrics,
    ) -> Result<(), LyricsCacheCreateErr> {
        trace!("Creating cache entry for {req}");
        let track_folder = self.track_cache_dir(req);
        trace!("Cache dir: {track_folder:?}");

        let meta = LyricCacheMeta {
            spotify_id: req.spotify_id.clone(),
            lrc_id,
            track_name: req.track_name.clone(),
            artist_name: req.artist_name.clone(),
            album_name: req.album_name.clone(),
            duration_sec: req.duration_sec,
        };

        fs::create_dir_all(&track_folder)?;

        // Write meta file
        let meta_str = serde_json::to_string_pretty(&meta)?;
        fs::write(track_folder.join(".meta"), meta_str)?;

        // Write lyrics file
        let lyrics_str = serde_json::to_string_pretty(song_lyrics)?;
        fs::write(track_folder.join("lyrics.lrc"), lyrics_str)?;

        Ok(())
    }
}
