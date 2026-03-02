use crate::lyrics_fetch::{LyricsFetcher, LyricsFetcherErr};

use tracing::debug;

use serde::{Deserialize, Serialize};
use tracing::trace;

static LRC_USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    " (github.com/ATantalizedLion/LyricsOverlay)"
);
const LRC_LIB_URL: &str = "https://lrclib.net/api/get";

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(super) struct LRCOkResponse {
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

impl LyricsFetcher {
    pub(super) async fn request_track_lrc(
        &self,
        duration_sec: &f64,
        track_name: &str,
        artist_name: &str,
        album_name: &str,
    ) -> Result<LRCOkResponse, LyricsFetcherErr> {
        let url = format!(
            "{LRC_LIB_URL}?artist_name={artist_name}&track_name={track_name}&album_name={album_name}&duration={duration_sec}"
        );
        let response: reqwest::Response = self
            .client
            .get(url)
            .header("User-Agent", LRC_USER_AGENT)
            .send()
            .await?;
        debug!("Response for track request: {:?}", response);

        if response.status().as_u16() == 404 {
            return Err(LyricsFetcherErr::SongLyricsNotFound());
        }

        let lyrics: LRCOkResponse = response.json().await?;

        trace!("Response for track request: {:?}", lyrics);

        Ok(lyrics)
    }
}
