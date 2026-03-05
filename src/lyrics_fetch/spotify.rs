use crate::lyrics_fetch::{LyricsFetcher, LyricsFetcherErr};

use tracing::{debug, error};

use serde::Deserialize;
use tracing::trace;

use crate::lyrics_parser::{SongLyrics, parse_lrc};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::time::{SystemTime, UNIX_EPOCH};

const TOKEN_URL: &str = "https://open.spotify.com/api/token";
const SERVER_TIME_URL: &str = "https://open.spotify.com/api/server-time";
const SECRET_KEY_URL: &str =
    "https://github.com/xyloflake/spot-secrets-go/blob/main/secrets/secretDict.json?raw=true";
const SPOTIFY_LYRICS_URL: &str = "https://spclient.wg.spotify.com/color-lyrics/v2/track";

#[derive(Deserialize, Debug)]
struct SpotifyLyricsResponse {
    lyrics: SpotifyLyricsBody,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SpotifyLyricsBody {
    lines: Vec<SpotifyLyricsLine>,
    sync_type: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SpotifyLyricsLine {
    start_time_ms: String,
    words: String,
}

impl LyricsFetcher {
    #[allow(clippy::cast_possible_truncation)]
    async fn get_secret_key(
        client: &reqwest::Client,
    ) -> Result<(String, String), LyricsFetcherErr> {
        let resp: serde_json::Value = client.get(SECRET_KEY_URL).send().await?.json().await?;

        let obj = resp
            .as_object()
            .ok_or(LyricsFetcherErr::SongLyricsNotFound())?;
        let (version, secret_raw) = obj
            .iter()
            .next_back()
            .ok_or(LyricsFetcherErr::SongLyricsNotFound())?;
        let arr = secret_raw
            .as_array()
            .ok_or(LyricsFetcherErr::SongLyricsNotFound())?;

        let secret = arr
            .iter()
            .enumerate()
            .filter_map(|(i, v)| Some((v.as_u64()? as u8 ^ ((i % 33) + 9) as u8).to_string()))
            .collect::<String>();
        Ok((secret, version.clone()))
    }

    fn generate_totp(server_time_secs: u64, secret: &str) -> String {
        let counter = (server_time_secs / 30).to_be_bytes();

        let result = Hmac::<Sha1>::new_from_slice(secret.as_bytes())
            .map(|mut mac| {
                mac.update(&counter);
                mac.finalize().into_bytes()
            })
            .unwrap();

        let offset = (result.last().unwrap() & 0x0F) as usize;
        let code = u32::from_be_bytes(result[offset..offset + 4].try_into().unwrap()) & 0x7FFF_FFFF;

        format!("{:06}", code % 1_000_000)
    }

    async fn get_sp_token(
        client: &reqwest::Client,
        sp_dc: &str,
    ) -> Result<String, LyricsFetcherErr> {
        debug!("Fetching server time from {SERVER_TIME_URL}");
        let time_resp: serde_json::Value = client.get(SERVER_TIME_URL).send().await?.json().await?;
        debug!("Server time response: {:?}", time_resp);

        let server_time = time_resp["serverTime"].as_u64().ok_or_else(|| {
            error!(
                "Missing or invalid 'serverTime' in response: {:?}",
                time_resp
            );
            LyricsFetcherErr::SongLyricsNotFound()
        })?;
        debug!("Server time: {server_time}");

        let (secret, version) = Self::get_secret_key(client).await?;
        debug!("Got secret key version: {version}");

        let totp = Self::generate_totp(server_time, &secret);
        debug!("Generated TOTP: {totp} for version {version}");

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let url = format!(
            "{TOKEN_URL}?reason=transport&productType=web-player&totp={totp}&totpVer={version}&ts={ts}"
        );
        debug!("Fetching token from: {url}");

        let response = client
            .get(&url)
            .header("Cookie", format!("sp_dc={sp_dc}"))
            .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
            .header("Accept", "application/json")
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Referer", "https://open.spotify.com/")
            .header("Origin", "https://open.spotify.com")
            .send()
            .await?;

        debug!("Token response status: {}", response.status());
        debug!("Token response headers: {:?}", response.headers());

        let token_resp: serde_json::Value = response.json().await?;
        debug!("Token response body: {:?}", token_resp);

        if let Some(is_anon) = token_resp["isAnonymous"].as_bool()
            && is_anon
        {
            error!("Token is anonymous — sp_dc is likely invalid or expired");
            return Err(LyricsFetcherErr::SongLyricsNotFound());
        }

        token_resp["accessToken"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| {
                error!("No 'accessToken' in token response: {:?}", token_resp);
                LyricsFetcherErr::SongLyricsNotFound()
            })
    }

    pub(super) async fn request_track_spotify(
        &self,
        spotify_id: &str,
    ) -> Result<SongLyrics, LyricsFetcherErr> {
        let sp_dc = self.settings.read().unwrap().sp_dc.clone();
        debug!(
            "sp_dc is {} chars, starts with: {}",
            sp_dc.len(),
            &sp_dc[..sp_dc.len().min(10)]
        );

        let token = Self::get_sp_token(&self.client, &sp_dc).await?;
        debug!(
            "Got sp token, starts with: {}",
            &token[..token.len().min(20)]
        );

        let url = format!("{SPOTIFY_LYRICS_URL}/{spotify_id}?format=json&market=from_token");
        debug!("Fetching lyrics from: {url}");

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("App-Platform", "WebPlayer")
            .header("Cookie", format!("sp_dc={sp_dc}"))
            .header("Spotify-App-Version", "1.2.46.25.g7f189073")
            .send()
            .await?;

        debug!("Lyrics response status: {}", response.status());
        debug!("Lyrics response headers: {:?}", response.headers());

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            error!("Lyrics request failed with status {status}: {body}");
            return Err(LyricsFetcherErr::SongLyricsNotFound());
        }

        let body: SpotifyLyricsResponse = response.json().await?;
        trace!("Response body for spotify track request: {:?}", body);

        if body.lyrics.sync_type != "LINE_SYNCED" {
            debug!(
                "Sync type is '{}', not LINE_SYNCED — skipping",
                body.lyrics.sync_type
            );
            return Err(LyricsFetcherErr::SongLyricsNotFound());
        }

        #[allow(clippy::cast_precision_loss)]
        let lrc_string: String = body
            .lyrics
            .lines
            .iter()
            .filter_map(|line| {
                let ms: u64 = line.start_time_ms.parse().ok()?;
                let minutes = ms / 60_000;
                let seconds = (ms % 60_000) as f64 / 1000.0;
                Some(format!("[{minutes:02}:{seconds:05.2}] {}", line.words))
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(parse_lrc(&lrc_string, false))
    }
}
