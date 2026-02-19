//! Module for fetching (cached) lyrics files for songs
//!
//! For now, with spotify integration in mind, we store based on spotify ID
//! Add a meta data file with extra track info. Maybe even store custom offsets here

use reqwest;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const LRC_LIB_URL: &str = "https://lrclib.net/api/get";

//TODO: Caching
pub struct LyricsFetcher {
    client: reqwest::Client,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LRCOkResponse {
    /// LRC ID
    pub id: usize,
    pub track_name: String,
    pub artist_name: String,
    pub album_name: String,
    pub duration: usize,
    pub instrumental: bool,
    pub plain_lyrics: String,
    pub synced_lyrics: String,
}

#[derive(Error, Debug)]
pub enum LyricsFetcherErr {
    #[error("OAuth token request failed: {0}")]
    ReqwestError(#[from] reqwest::Error),
    #[error("UnknownError")]
    UnknownError(),
}

impl LyricsFetcher {
    pub fn new() -> Self {
        Self {
            client: {
                let client = reqwest::Client::builder()
                    .user_agent(super::APP_USER_AGENT)
                    .build()
                    .unwrap();
                client
            },
        }
    }

    pub async fn request_track(
        &self,
        duration_sec: usize,
        track_name: String,
        artist_name: String,
        album_name: String,
    ) -> Result<String, LyricsFetcherErr> {
        let url = format!(
            "{LRC_LIB_URL}?artist_name={artist_name}&track_name={track_name}&album_name={album_name}&duration={duration_sec}"
        );
        let response: reqwest::Response = self.client.get(url).send().await?;

        Err(LyricsFetcherErr::UnknownError())
    }
}
