//! Records songs playing in the local Spotify app into their lyrics cache entry, so the
//! alignment tool (`tools/align`) can add syllable timing to them later.
//!
//! Only Spotify's own audio is captured (Windows per-process loopback), so notification
//! sounds and other apps don't end up in the recording. A recording is only kept if it
//! covers the whole song in one uninterrupted play: capture starts when a track starts,
//! the playback position readings during the song pin down where in the song the file
//! begins, and a pause or seek (readings that disagree) discards it, since either would
//! put the audio out of step with the lyrics' timestamps. Recordings stay in the cache entry after alignment,
//! so they can be re-aligned with different settings later.

// Millisecond and frame counts here are far from the limits of the types they're cast between.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Instant,
};

use tokio::sync::RwLock as TokioRwLock;
use tracing::{debug, info, warn};
use windows::Win32::{
    Foundation::CloseHandle,
    System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    },
};

use crate::{
    lyrics_fetch::LyricsRequestInfo, lyrics_parser::SongLyrics, now_playing::NowPlaying, paths,
    settings::Settings,
};

/// Name of a finished recording inside a cache entry; the alignment tool looks for it.
pub const RECORDING_FILE: &str = "recording.wav";
/// Next to the recording: `{"offset_ms": n}`, the song position at the file's first
/// sample (negative if the file starts before the song).
const RECORDING_META_FILE: &str = "recording.json";
/// Songs in progress are recorded into `.recording-<n>.wav.part` in the cache root: the
/// song's own folder is only looked up once it's done, since its lyrics may still be
/// being fetched now. Each recording gets its own file, because the next song's capture
/// starts while the previous one is still being saved.
fn partial_file_name() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(".recording-{nanos}.wav.part")
}
/// Left by the alignment tool when it processed a recording but couldn't align any line,
/// so the same song isn't recorded again and again.
const ATTEMPTED_MARKER: &str = ".align-attempted";

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
/// Where in the song the recording may start (song position at the file's first sample):
/// a little after the start is padded with silence, a little before (when the capture
/// started on the track change, before the song's audio) is trimmed.
const START_OFFSET_RANGE_MS: std::ops::RangeInclusive<i64> = -10_000..=3_000;
/// Position readings during the recording must agree on that offset within this; more
/// spread means a seek, or a pause we didn't see.
const MAX_OFFSET_SPREAD_MS: i64 = 750;
/// Share of position readings that must agree for the recording to be kept.
const MIN_CONSISTENT_SHARE: f64 = 0.8;
/// Consecutive not-playing readings tolerated before giving up: Spotify can briefly
/// report a non-playing state around track changes.
const MAX_PAUSED_POLLS: u32 = 2;
/// A recording counts as complete if it reaches this close to the song's end.
const END_TOLERANCE_MS: i64 = 2_500;
/// Skip very short tracks (intros, skits, ads).
const MIN_DURATION_MS: u64 = 30_000;
/// A sample above this (about -60 dBFS) counts as sound rather than silence.
const AUDIBLE_LEVEL: i16 = 32;
/// Share of a recording that must have sound in it. When the Spotify app is controlling
/// playback on another device (Spotify Connect), Windows still reports it as playing
/// here, but no audio comes out of this PC: those recordings are near-silent.
const MIN_AUDIBLE_SHARE: f64 = 0.3;
/// Safety cap on one recording.
const MAX_RECORDING_MS: u64 = 20 * 60 * 1000;

type Track = (String, String, String);

struct ActiveRecording {
    track: Track,
    request: LyricsRequestInfo,
    cache_root: PathBuf,
    /// This recording's own in-progress file, see `partial_file_name`.
    partial: PathBuf,
    started_at: Instant,
    duration_ms: u64,
    /// Song position minus time since the capture started, per reading while playing.
    /// For an uninterrupted play these are all (about) the song position at the
    /// recording's first sample.
    offsets_ms: Vec<i64>,
    paused_polls: u32,
    abandoned: Option<&'static str>,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<Result<Captured, String>>,
}

pub struct SongRecorder {
    settings: Arc<TokioRwLock<Settings>>,
    active: Option<ActiveRecording>,
    /// The track of the previous reading: a recording only starts when a track starts.
    last_track: Option<Track>,
}

impl SongRecorder {
    pub fn new(settings: Arc<TokioRwLock<Settings>>) -> Self {
        Self {
            settings,
            active: None,
            last_track: None,
        }
    }

    /// Called on every now-playing poll with the Spotify app's reading when the Spotify
    /// app on this machine is what's playing (the only thing we can capture), else `None`.
    pub async fn observe(&mut self, local_spotify: Option<&NowPlaying>) {
        let (enabled, cache_folder) = {
            let s = self.settings.read().await;
            (
                s.record_spotify_for_alignment && s.caching_enabled,
                s.cache_folder.clone(),
            )
        };
        let reading = local_spotify.filter(|_| enabled);

        if let Some(active) = self.active.as_mut() {
            match reading {
                Some(np) if track_key(np) == active.track => {
                    active.observe(np);
                    if active.abandoned.is_some() || active.handle.is_finished() {
                        self.finish();
                    }
                }
                _ => self.finish(),
            }
        }

        let previous_track = std::mem::replace(&mut self.last_track, reading.map(track_key));
        let Some(np) = reading else { return };
        if self.active.is_some() || previous_track.as_ref() == Some(&track_key(np)) {
            return;
        }

        let duration_ms = (np.duration_sec * 1000.0) as u64;
        if duration_ms > 0 && duration_ms < MIN_DURATION_MS {
            debug!("Not recording '{}': shorter than 30s", np.track_title);
            return;
        }
        let cache_root = paths::resolve(&cache_folder);
        let mut request = LyricsRequestInfo::from_now_playing(np);
        request.match_existing_cache_entry(&cache_root);
        if !needs_recording(&request.cache_dir(&cache_root)) {
            debug!(
                "Not recording '{}': already recorded or aligned, or no synced lyrics",
                np.track_title
            );
            return;
        }
        self.start(np, request, cache_root, duration_ms);
    }

    fn start(
        &mut self,
        np: &NowPlaying,
        request: LyricsRequestInfo,
        cache_root: PathBuf,
        duration_ms: u64,
    ) {
        let Some(pid) = spotify_pid() else {
            info!("Spotify process not found, not recording");
            return;
        };
        if let Err(e) = fs::create_dir_all(&cache_root) {
            warn!("Can't create cache folder for recording: {e}");
            return;
        }
        info!(
            "Recording '{} - {}' for syllable alignment",
            np.artist, np.track_title
        );
        let stop = Arc::new(AtomicBool::new(false));
        let partial = cache_root.join(partial_file_name());
        let path = partial.clone();
        let thread_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("song-recorder".into())
            .spawn(move || capture(pid, &path, MAX_RECORDING_MS, &thread_stop))
            .expect("failed to spawn recorder thread");
        let mut active = ActiveRecording {
            track: track_key(np),
            request,
            cache_root,
            partial,
            started_at: Instant::now(),
            duration_ms,
            offsets_ms: Vec::new(),
            paused_polls: 0,
            abandoned: None,
            stop,
            handle,
        };
        active.observe(np);
        self.active = Some(active);
    }

    /// Stops recording, and keeps the file if it covers the whole song in one play.
    fn finish(&mut self) {
        let Some(active) = self.active.take() else {
            return;
        };
        active.stop.store(true, Ordering::Relaxed);
        // Joining waits at most one capture wait (~100ms) plus the WAV finalize, so do
        // it off the async runtime.
        tokio::task::spawn_blocking(move || active.save_or_discard());
    }
}

impl ActiveRecording {
    fn observe(&mut self, np: &NowPlaying) {
        if np.duration_sec > 0.0 {
            self.duration_ms = (np.duration_sec * 1000.0) as u64;
        }
        if np.is_playing {
            self.paused_polls = 0;
            #[allow(clippy::cast_possible_wrap)]
            self.offsets_ms.push(
                current_position_ms(np) as i64 - self.started_at.elapsed().as_millis() as i64,
            );
        } else {
            self.paused_polls += 1;
            if self.paused_polls > MAX_PAUSED_POLLS {
                self.abandoned = Some("playback paused");
            }
        }
    }

    /// The song position at the recording's first sample, if the position readings
    /// agree on one (they don't after a seek).
    fn start_offset_ms(&self) -> Result<i64, String> {
        if self.offsets_ms.is_empty() {
            return Err("no position readings".into());
        }
        let mut sorted = self.offsets_ms.clone();
        sorted.sort_unstable();
        let median = sorted[sorted.len() / 2];
        let agreeing = sorted
            .iter()
            .filter(|o| (*o - median).abs() <= MAX_OFFSET_SPREAD_MS)
            .count();
        #[allow(clippy::cast_precision_loss)]
        let share = agreeing as f64 / sorted.len() as f64;
        if share < MIN_CONSISTENT_SHARE {
            return Err(format!(
                "playback position jumped ({:.0}% of readings consistent)",
                share * 100.0
            ));
        }
        if !START_OFFSET_RANGE_MS.contains(&median) {
            return Err(format!(
                "didn't start at the beginning of the song (started at {median} ms)"
            ));
        }
        Ok(median)
    }

    fn save_or_discard(self) {
        let offset = match self.abandoned {
            Some(reason) => Err(reason.to_string()),
            None => self.start_offset_ms(),
        };
        let partial = self.partial.clone();
        let captured = match self.handle.join() {
            Ok(Ok(captured)) => captured,
            Ok(Err(e)) => {
                warn!("Recording failed: {e}");
                let _ = fs::remove_file(&partial);
                return;
            }
            Err(_) => {
                warn!("Recorder thread panicked");
                let _ = fs::remove_file(&partial);
                return;
            }
        };
        let mut request = self.request.clone();
        request.match_existing_cache_entry(&self.cache_root);
        let dir = request.cache_dir(&self.cache_root);

        #[allow(clippy::cast_possible_wrap)]
        let recorded_ms = (captured.frames * 1000 / u64::from(SAMPLE_RATE)) as i64;
        #[allow(clippy::cast_precision_loss)]
        let audible_share = captured.audible_frames as f64 / captured.frames.max(1) as f64;
        #[allow(clippy::cast_possible_wrap)]
        let duration_ms = self.duration_ms as i64;
        let verdict = offset.and_then(|offset| {
            if duration_ms == 0 || offset + recorded_ms + END_TOLERANCE_MS < duration_ms {
                return Err(format!(
                    "incomplete ({} of {duration_ms} ms)",
                    offset + recorded_ms
                ));
            }
            if audible_share < MIN_AUDIBLE_SHARE {
                return Err(format!(
                    "almost no sound ({:.0}% audible), probably playing on another device",
                    audible_share * 100.0
                ));
            }
            if !has_synced_lyrics(&dir) {
                return Err("no line-synced lyrics cached".into());
            }
            Ok(offset)
        });

        match verdict {
            Ok(offset) => {
                let meta = serde_json::json!({ "offset_ms": offset });
                let saved = fs::write(dir.join(RECORDING_META_FILE), meta.to_string())
                    .and_then(|()| fs::rename(&partial, dir.join(RECORDING_FILE)));
                match saved {
                    Ok(()) => info!(
                        "Saved recording to {} (song starts {} ms into the file)",
                        dir.display(),
                        -offset
                    ),
                    Err(e) => warn!("Couldn't save recording: {e}"),
                }
            }
            Err(reason) => {
                info!("Discarding recording of '{}': {reason}", self.track.0);
                let _ = fs::remove_file(&partial);
            }
        }
    }
}

fn track_key(np: &NowPlaying) -> Track {
    (np.track_title.clone(), np.artist.clone(), np.album.clone())
}

fn current_position_ms(np: &NowPlaying) -> u64 {
    let elapsed = if np.is_playing {
        np.measured_at.elapsed().as_millis() as u64
    } else {
        0
    };
    np.progress_ms as u64 + elapsed
}

/// Worth recording: no recording yet, and the cached lyrics (if fetched already) are
/// line-synced without syllable timing.
fn needs_recording(dir: &Path) -> bool {
    if dir.join(RECORDING_FILE).exists() || dir.join(ATTEMPTED_MARKER).exists() {
        return false;
    }
    match read_cached_lyrics(dir) {
        None => !dir.join("lyrics.lrc").exists(), // not fetched yet: decide at the end
        Some(lyrics) => {
            !lyrics.synced_lyrics.is_empty()
                && lyrics.synced_lyrics.iter().all(|l| l.segments.is_empty())
        }
    }
}

fn has_synced_lyrics(dir: &Path) -> bool {
    read_cached_lyrics(dir).is_some_and(|l| !l.synced_lyrics.is_empty())
}

fn read_cached_lyrics(dir: &Path) -> Option<SongLyrics> {
    let file = fs::File::open(dir.join("lyrics.lrc")).ok()?;
    serde_json::from_reader(file).ok()
}

/// The top-level Spotify.exe process (Spotify runs several; capturing the parent with
/// its process tree gets the one that actually plays audio).
fn spotify_pid() -> Option<u32> {
    let mut procs: Vec<(u32, u32)> = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snapshot, &raw mut entry).is_ok();
        while ok {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if name.eq_ignore_ascii_case("spotify.exe") {
                procs.push((entry.th32ProcessID, entry.th32ParentProcessID));
            }
            ok = Process32NextW(snapshot, &raw mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    procs
        .iter()
        .find(|(_, parent)| !procs.iter().any(|(pid, _)| pid == parent))
        .map(|(pid, _)| *pid)
}

/// Captures Spotify's audio into a WAV file until `stop` is set or `max_ms` is reached.
/// Pads with silence whenever capture falls behind the wall clock (WASAPI may deliver
/// nothing while the app is silent), so the file stays in step with real time. Returns
/// the number of frames written.
#[allow(clippy::cast_possible_truncation)]
fn capture(pid: u32, path: &Path, max_ms: u64, stop: &AtomicBool) -> Result<Captured, String> {
    let err = |e: wasapi::WasapiError| e.to_string();
    let _ = wasapi::initialize_mta().ok();

    let format = wasapi::WaveFormat::new(
        16,
        16,
        &wasapi::SampleType::Int,
        SAMPLE_RATE as usize,
        CHANNELS as usize,
        None,
    );
    let block_align = format.get_blockalign() as usize;
    let mut client =
        wasapi::AudioClient::new_application_loopback_client(pid, true).map_err(err)?;
    client
        .initialize_client(
            &format,
            &wasapi::Direction::Capture,
            &wasapi::StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 0,
            },
        )
        .map_err(err)?;
    let event = client.set_get_eventhandle().map_err(err)?;
    let capture_client = client.get_audiocaptureclient().map_err(err)?;

    let spec = hound::WavSpec {
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    let frames_for = |ms: u64| ms * u64::from(SAMPLE_RATE) / 1000;
    let max_frames = frames_for(max_ms);

    let mut written: u64 = 0;
    let mut audible_frames: u64 = 0;
    let write_silence = |writer: &mut hound::WavWriter<_>, frames: u64| {
        for _ in 0..frames * u64::from(CHANNELS) {
            writer.write_sample(0i16)?;
        }
        Ok::<u64, hound::Error>(frames)
    };

    client.start_stream().map_err(err)?;
    let started = Instant::now();
    let mut queue: VecDeque<u8> = VecDeque::new();

    while !stop.load(Ordering::Relaxed) && written < max_frames {
        let _ = event.wait_for_event(100); // a timeout just means nothing new yet
        while let Some(n) = capture_client.get_next_packet_size().map_err(err)? {
            if n == 0 {
                break;
            }
            let before = queue.len();
            let info = capture_client
                .read_from_device_to_deque(&mut queue)
                .map_err(err)?;
            if info.flags.silent {
                for b in queue.range_mut(before..) {
                    *b = 0;
                }
            }
        }

        // Keep the file on the wall clock: fill gaps where nothing was delivered.
        let expected = frames_for(started.elapsed().as_millis() as u64);
        let queued = (queue.len() / block_align) as u64;
        let behind = expected.saturating_sub(written + queued);
        if behind > frames_for(100) {
            let pad = behind.min(max_frames.saturating_sub(written));
            written += write_silence(&mut writer, pad).map_err(|e| e.to_string())?;
        }

        let frames = ((queue.len() / block_align) as u64).min(max_frames.saturating_sub(written));
        for _ in 0..frames {
            let mut audible = false;
            for _ in 0..CHANNELS {
                let lo = queue.pop_front().unwrap_or(0);
                let hi = queue.pop_front().unwrap_or(0);
                let sample = i16::from_le_bytes([lo, hi]);
                audible |= sample.unsigned_abs() > AUDIBLE_LEVEL.unsigned_abs();
                writer.write_sample(sample).map_err(|e| e.to_string())?;
            }
            audible_frames += u64::from(audible);
        }
        written += frames;
    }

    let _ = client.stop_stream();
    writer.finalize().map_err(|e| e.to_string())?;
    Ok(Captured {
        frames: written,
        audible_frames,
    })
}

struct Captured {
    frames: u64,
    /// Frames with any sound in them, see `MIN_AUDIBLE_SHARE`.
    audible_frames: u64,
}
