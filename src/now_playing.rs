//! The app's source-agnostic "what's playing right now" model. Produced by the SMTC
//! poller (see `crate::smtc`), independent of which app actually owns the media session.

use std::time::Instant;

#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub track_title: String,
    pub artist: String,
    pub album: String,
    pub duration_sec: f64,
    pub is_playing: bool,
    pub progress_ms: usize,
    /// Instant at which `progress_ms` was known to be accurate - not "when we polled",
    /// since some sources only push position updates sporadically. See
    /// `crate::smtc::filetime_to_instant`.
    pub measured_at: Instant,
    /// Spotify track ID, resolved via best-effort cross-reference against Spotify's Web
    /// API when the active session looks like Spotify and OAuth is configured. `None`
    /// otherwise; `LyricsFetcher::get_lyrics` already falls back to `LRCLib` in that case.
    pub spotify_id: Option<String>,
}

impl NowPlaying {
    /// Same-track identity used to decide whether to re-fetch lyrics.
    pub fn is_same_track(&self, other: &NowPlaying) -> bool {
        self.track_title == other.track_title
            && self.artist == other.artist
            && self.album == other.album
    }
}
