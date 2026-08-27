//! Thin wrapper around Windows System Media Transport Controls (SMTC) - the API behind
//! the volume flyout's "now playing" widget. Works with whatever app is currently
//! playing audio (Spotify, a browser tab, VLC, ...), no authentication required.

pub mod poller;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use windows::Foundation::DateTime;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};

use crate::now_playing::NowPlaying;

#[derive(Error, Debug)]
pub enum SmtcError {
    #[error("No active media session")]
    NoActiveSession,
    #[error("The session rejected the command")]
    CommandRejected,
    #[error("Windows Media API error: {0}")]
    Windows(#[from] windows::core::Error),
}

pub struct SmtcClient {
    manager: SessionManager,
}

impl SmtcClient {
    pub async fn new() -> windows::core::Result<Self> {
        let manager = SessionManager::RequestAsync()?.await?;
        Ok(Self { manager })
    }

    /// The OS's notion of the "current" session (matches what the volume flyout shows).
    /// Any error here (including "nothing is playing anywhere", the common idle state)
    /// is treated as `None` rather than propagated - there's nothing actionable to show.
    fn current_session(&self) -> Option<Session> {
        self.manager.GetCurrentSession().ok()
    }

    /// The active session's app identifier (e.g. "Spotify.exe"), used to decide whether
    /// to attempt the Spotify lyrics cross-reference. `None` if nothing is playing.
    pub fn current_source_app_id(&self) -> Option<String> {
        self.current_session()?
            .SourceAppUserModelId()
            .ok()
            .map(|s| s.to_string())
    }

    /// Whether SMTC currently sees a local session at all, regardless of what it is - used
    /// to decide whether the optional Spotify fallback source/controls should kick in
    /// (only when SMTC has nothing, e.g. playback via Spotify Connect on another device).
    pub fn has_active_session(&self) -> bool {
        self.current_session().is_some()
    }

    /// Reads the currently active session's metadata and position, if any.
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    pub async fn read_now_playing(&self) -> Option<NowPlaying> {
        let session = self.current_session()?;
        let props = session.TryGetMediaPropertiesAsync().ok()?.await.ok()?;

        let title = props.Title().ok()?.to_string();
        if title.trim().is_empty() {
            // A session can exist (e.g. just opened) before it has real track info.
            return None;
        }
        let artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
        let album = props.AlbumTitle().map(|s| s.to_string()).unwrap_or_default();

        let (duration_sec, progress_ms, measured_at) = session
            .GetTimelineProperties()
            .ok()
            .map_or((0.0, 0, Instant::now()), |timeline| {
                let start = timeline.StartTime().map_or(0, |ts| ts.Duration);
                let end = timeline.EndTime().map_or(0, |ts| ts.Duration);
                let position = timeline.Position().map_or(0, |ts| ts.Duration);

                let duration_sec = ticks_to_secs((end - start).max(0));
                let progress_ms = ticks_to_secs((position - start).max(0)) * 1000.0;
                let measured_at = timeline
                    .LastUpdatedTime()
                    .ok().map_or_else(Instant::now, filetime_to_instant);

                (duration_sec, progress_ms as usize, measured_at)
            });

        let is_playing = session
            .GetPlaybackInfo()
            .and_then(|info| info.PlaybackStatus())
            .is_ok_and(|status| status == PlaybackStatus::Playing);

        Some(NowPlaying {
            track_title: title,
            artist,
            album,
            duration_sec,
            is_playing,
            progress_ms,
            measured_at,
            spotify_id: None, // resolved separately, see smtc::poller
        })
    }

    pub async fn play(&self) -> Result<(), SmtcError> {
        let session = self.current_session().ok_or(SmtcError::NoActiveSession)?;
        Self::finish(session.TryPlayAsync()?.await?)
    }

    pub async fn pause(&self) -> Result<(), SmtcError> {
        let session = self.current_session().ok_or(SmtcError::NoActiveSession)?;
        Self::finish(session.TryPauseAsync()?.await?)
    }

    pub async fn next(&self) -> Result<(), SmtcError> {
        let session = self.current_session().ok_or(SmtcError::NoActiveSession)?;
        Self::finish(session.TrySkipNextAsync()?.await?)
    }

    pub async fn previous(&self) -> Result<(), SmtcError> {
        let session = self.current_session().ok_or(SmtcError::NoActiveSession)?;
        Self::finish(session.TrySkipPreviousAsync()?.await?)
    }

    /// SMTC commands return `Ok(false)` when the session rejects the command (e.g. no
    /// play capability right now) - that's a rejection, not a success.
    fn finish(accepted: bool) -> Result<(), SmtcError> {
        if accepted {
            Ok(())
        } else {
            Err(SmtcError::CommandRejected)
        }
    }
}

#[allow(clippy::cast_precision_loss)]
fn ticks_to_secs(ticks: i64) -> f64 {
    ticks as f64 / 10_000_000.0
}

/// Converts a `WinRT` `DateTime` (100ns ticks since 1601-01-01) to an `Instant` on the same
/// clock the rest of the app uses, the same "elapsed-then-subtract" trick
/// `spotify::SpotifyClient::get_current_track` uses for its own round-trip compensation.
fn filetime_to_instant(dt: DateTime) -> Instant {
    const TICKS_PER_SEC: i64 = 10_000_000;
    const UNIX_EPOCH_OFFSET_SECS: i64 = 11_644_473_600;

    let unix_secs = dt.UniversalTime / TICKS_PER_SEC - UNIX_EPOCH_OFFSET_SECS;
    let system_time = if unix_secs >= 0 {
        UNIX_EPOCH + Duration::from_secs(unix_secs.cast_unsigned())
    } else {
        UNIX_EPOCH - Duration::from_secs(unix_secs.unsigned_abs())
    };

    match SystemTime::now().duration_since(system_time) {
        Ok(elapsed) => Instant::now()
            .checked_sub(elapsed)
            .unwrap_or_else(Instant::now),
        Err(_) => Instant::now(), // timestamp is somehow in the future; just use now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_to_instant_is_close_to_now_for_a_recent_timestamp() {
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let five_secs_ago_ticks = (now_unix - 5 + 11_644_473_600) * 10_000_000;

        let instant = filetime_to_instant(DateTime {
            UniversalTime: five_secs_ago_ticks,
        });

        let elapsed = instant.elapsed().as_secs_f64();
        assert!((4.0..6.0).contains(&elapsed), "elapsed = {elapsed}");
    }
}
