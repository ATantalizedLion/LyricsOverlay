use std::sync::Arc;

use tokio::sync::RwLock as TokioRwLock;
use tokio::sync::mpsc;
use tracing::debug;

use crate::{
    MessageToUI, now_playing::NowPlaying, runtime::Messages, settings::NowPlayingSource,
    settings::Settings, spotify::SpotifyClient,
};

use super::SmtcClient;

/// Polls whichever of SMTC / Spotify's own API the user's `NowPlayingSource` preference
/// puts first, falling back to the other when the preferred one has nothing - covering
/// the one gap each source has structurally: SMTC only sees local media sessions (so it
/// misses Spotify Connect playback on another device/speaker), while Spotify's API only
/// knows about Spotify (so it misses everything else). Also - best effort, only when the
/// active SMTC session looks like Spotify - cross-references Spotify's Web API to resolve
/// a track id, unlocking Spotify's own (richer) lyrics as a bonus on top of the universal
/// `LRCLib` fallback.
pub struct SmtcPoller {
    smtc: Arc<SmtcClient>,
    spotify: Arc<SpotifyClient>,
    settings: Arc<TokioRwLock<Settings>>,
    last_track_key: Option<(String, String, String)>,
    resolved_spotify_id: Option<String>,
}

impl SmtcPoller {
    pub fn new(
        smtc: Arc<SmtcClient>,
        spotify: Arc<SpotifyClient>,
        settings: Arc<TokioRwLock<Settings>>,
    ) -> Self {
        Self {
            smtc,
            spotify,
            settings,
            last_track_key: None,
            resolved_spotify_id: None,
        }
    }

    pub async fn run(mut self, tx_ui: mpsc::Sender<MessageToUI>) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(
            self.settings.read().await.smtc_poll_interval_ms,
        ));
        loop {
            interval.tick().await;
            let msg = self.poll().await;
            Messages::to_ui(msg).send(tx_ui.clone()).await;
        }
    }

    async fn poll(&mut self) -> MessageToUI {
        let preference = self.settings.read().await.now_playing_source;

        if preference == NowPlayingSource::PreferSpotify
            && let Ok(now_playing) = self.spotify.get_now_playing().await
        {
            // A Spotify-sourced reading needs none of SMTC's own cross-reference state -
            // it already carries a real spotify_id straight from the API.
            self.last_track_key = None;
            self.resolved_spotify_id = None;
            return MessageToUI::CurrentlyPlaying(now_playing);
        }

        let Some(mut now_playing) = self.smtc.read_now_playing().await else {
            self.last_track_key = None;
            self.resolved_spotify_id = None;
            return match preference {
                // PreferSpotify already tried Spotify above and it had nothing either.
                NowPlayingSource::SmtcOnly | NowPlayingSource::PreferSpotify => {
                    MessageToUI::NotCurrentlyPlaying("Nothing playing".to_owned())
                }
                NowPlayingSource::PreferSmtc => self.poll_spotify_fallback().await,
            };
        };

        let track_key = (
            now_playing.track_title.clone(),
            now_playing.artist.clone(),
            now_playing.album.clone(),
        );
        if self.last_track_key.as_ref() != Some(&track_key) {
            self.last_track_key = Some(track_key);
            self.resolved_spotify_id = None;
        }

        if self.resolved_spotify_id.is_none() {
            let looks_like_spotify = self
                .smtc
                .current_source_app_id()
                .is_some_and(|id| id.to_lowercase().contains("spotify"));

            if looks_like_spotify {
                self.resolved_spotify_id = self.try_resolve_spotify_id(&now_playing).await;
            }
        }

        now_playing.spotify_id.clone_from(&self.resolved_spotify_id);
        MessageToUI::CurrentlyPlaying(now_playing)
    }

    /// Called only when SMTC has no local session and the preference is `PreferSmtc`.
    /// Already carries a resolved `spotify_id`, so no cross-reference step is needed here,
    /// unlike the SMTC bonus-lyrics path above.
    async fn poll_spotify_fallback(&self) -> MessageToUI {
        if let Ok(now_playing) = self.spotify.get_now_playing().await {
            return MessageToUI::CurrentlyPlaying(now_playing);
        }
        MessageToUI::NotCurrentlyPlaying("Nothing playing".to_owned())
    }

    /// Best-effort: silently gives up (falls back to `None`, i.e. LRCLib-only) on any
    /// missing config, request failure, or a title/artist mismatch - the latter guards
    /// against a race where SMTC and the Web API haven't caught up to the same track yet.
    /// Safe to call even when Spotify isn't configured at all: `get_current_track`
    /// already short-circuits to an error before any network request in that case.
    async fn try_resolve_spotify_id(&self, now_playing: &NowPlaying) -> Option<String> {
        let spotify_now = self.spotify.get_current_track().await.ok()?;
        if !spotify_now.is_track() {
            return None;
        }

        let title_matches = spotify_now
            .get_track_title()
            .is_some_and(|t| t.eq_ignore_ascii_case(&now_playing.track_title));
        let artist_matches = spotify_now
            .get_artist()
            .is_some_and(|a| a.eq_ignore_ascii_case(&now_playing.artist));
        if !title_matches || !artist_matches {
            return None;
        }

        let id = spotify_now.get_spotify_id();
        if id.is_some() {
            debug!("Resolved Spotify track id for bonus lyrics lookup");
        }
        id
    }
}
