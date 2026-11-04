//! Records songs playing in the local Spotify app into their lyrics cache entry, so the
//! alignment tool (`tools/align`) can add syllable timing to them later.
//!
//! Only Spotify's own audio is captured (Windows per-process loopback), so notification
//! sounds and other apps don't end up in the recording. A recording is only kept if it
//! covers the whole song in one uninterrupted play: it has to start within the first few
//! seconds, and pausing or seeking abandons it, since either would put the audio out of
//! step with the lyrics' timestamps. Recordings stay in the cache entry after alignment,
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
const PARTIAL_FILE: &str = "recording.wav.part";
/// Left by the alignment tool when it processed a recording but couldn't align any line,
/// so the same song isn't recorded again and again.
const ATTEMPTED_MARKER: &str = ".align-attempted";

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
/// Only start recording if we're at most this far into the song; the missing start is
/// filled with silence so the file's timeline still matches the song's.
const MAX_START_OFFSET_MS: u64 = 3_000;
/// Playback position may drift this far from what our own clock expects before we treat
/// it as a seek.
const MAX_DRIFT_MS: i64 = 2_000;
/// A recording counts as complete if it reaches this close to the song's end.
const END_TOLERANCE_MS: u64 = 2_500;
/// Skip very short tracks (intros, skits, ads).
const MIN_DURATION_MS: u64 = 30_000;

type Track = (String, String, String);

struct ActiveRecording {
    track: Track,
    dir: PathBuf,
    started_at: Instant,
    start_song_ms: u64,
    duration_ms: u64,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<Result<u64, String>>,
}

pub struct SongRecorder {
    settings: Arc<TokioRwLock<Settings>>,
    active: Option<ActiveRecording>,
}

impl SongRecorder {
    pub fn new(settings: Arc<TokioRwLock<Settings>>) -> Self {
        Self {
            settings,
            active: None,
        }
    }

    /// Called on every now-playing poll. `local_spotify` says whether the reading comes
    /// from the Spotify app on this machine (the only thing we can capture).
    pub async fn observe(&mut self, now_playing: Option<&NowPlaying>, local_spotify: bool) {
        let (enabled, cache_folder) = {
            let s = self.settings.read().await;
            (
                s.record_spotify_for_alignment && s.caching_enabled,
                s.cache_folder.clone(),
            )
        };

        if let Some(active) = &self.active {
            let keep_going = enabled
                && local_spotify
                && now_playing.is_some_and(|np| track_key(np) == active.track);
            match now_playing {
                _ if !keep_going => self.finish(),
                Some(np) if !np.is_playing => self.abandon("playback paused"),
                Some(np) if drift_ms(active, np).abs() > MAX_DRIFT_MS => {
                    self.abandon("playback position jumped");
                }
                _ if active.handle.is_finished() => self.finish(),
                _ => {}
            }
        }

        if self.active.is_some() || !enabled || !local_spotify {
            return;
        }
        let Some(np) = now_playing else { return };
        let position_ms = current_position_ms(np);
        let duration_ms = (np.duration_sec * 1000.0) as u64;
        if !np.is_playing || position_ms > MAX_START_OFFSET_MS || duration_ms < MIN_DURATION_MS {
            return;
        }
        let dir = paths::resolve(&cache_folder)
            .join(LyricsRequestInfo::from_now_playing(np).get_track_identifier());
        if !needs_recording(&dir) {
            return;
        }
        self.start(np, dir, position_ms, duration_ms);
    }

    fn start(&mut self, np: &NowPlaying, dir: PathBuf, position_ms: u64, duration_ms: u64) {
        let Some(pid) = spotify_pid() else {
            debug!("Spotify process not found, not recording");
            return;
        };
        if let Err(e) = fs::create_dir_all(&dir) {
            warn!("Can't create cache folder for recording: {e}");
            return;
        }
        info!(
            "Recording '{} - {}' for syllable alignment",
            np.artist, np.track_title
        );
        let stop = Arc::new(AtomicBool::new(false));
        let path = dir.join(PARTIAL_FILE);
        let thread_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("song-recorder".into())
            .spawn(move || capture(pid, &path, position_ms, duration_ms, &thread_stop))
            .expect("failed to spawn recorder thread");
        self.active = Some(ActiveRecording {
            track: track_key(np),
            dir,
            started_at: Instant::now(),
            start_song_ms: position_ms,
            duration_ms,
            stop,
            handle,
        });
    }

    /// Stops recording and keeps the file if it covers the whole song.
    fn finish(&mut self) {
        let Some(active) = self.active.take() else {
            return;
        };
        active.stop.store(true, Ordering::Relaxed);
        // Joining waits at most one capture wait (~100ms) plus the WAV finalize, so do
        // it off the async runtime.
        tokio::task::spawn_blocking(move || {
            let partial = active.dir.join(PARTIAL_FILE);
            let frames = match active.handle.join() {
                Ok(Ok(frames)) => frames,
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
            let recorded_ms = frames * 1000 / u64::from(SAMPLE_RATE);
            let complete = recorded_ms + END_TOLERANCE_MS >= active.duration_ms;
            if complete && has_synced_lyrics(&active.dir) {
                match fs::rename(&partial, active.dir.join(RECORDING_FILE)) {
                    Ok(()) => info!("Saved recording to {}", active.dir.display()),
                    Err(e) => warn!("Couldn't save recording: {e}"),
                }
            } else {
                debug!(
                    "Discarding recording ({recorded_ms} of {} ms, complete: {complete})",
                    active.duration_ms
                );
                let _ = fs::remove_file(&partial);
            }
        });
    }

    fn abandon(&mut self, reason: &str) {
        if let Some(active) = self.active.as_mut() {
            debug!("Abandoning recording: {reason}");
            // Make `finish` see it as incomplete by pretending the song is endless.
            active.duration_ms = u64::MAX / 2;
        }
        self.finish();
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

/// How far the reported position is from where an uninterrupted play would be.
#[allow(clippy::cast_possible_wrap)]
fn drift_ms(active: &ActiveRecording, np: &NowPlaying) -> i64 {
    let expected = active.start_song_ms + active.started_at.elapsed().as_millis() as u64;
    current_position_ms(np) as i64 - expected as i64
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

/// Captures Spotify's audio into a WAV file until `stop` is set or the song's length is
/// reached. Starts with `lead_ms` of silence so the file lines up with song time 0, and
/// pads with silence whenever capture falls behind the wall clock (WASAPI may deliver
/// nothing while the app is silent). Returns the number of frames written.
#[allow(clippy::cast_possible_truncation)]
fn capture(
    pid: u32,
    path: &Path,
    lead_ms: u64,
    duration_ms: u64,
    stop: &AtomicBool,
) -> Result<u64, String> {
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
    let max_frames = frames_for(duration_ms);
    let lead_frames = frames_for(lead_ms);

    let mut written: u64 = 0;
    let write_silence = |writer: &mut hound::WavWriter<_>, frames: u64| {
        for _ in 0..frames * u64::from(CHANNELS) {
            writer.write_sample(0i16)?;
        }
        Ok::<u64, hound::Error>(frames)
    };
    written += write_silence(&mut writer, lead_frames).map_err(|e| e.to_string())?;

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
        let expected = lead_frames + frames_for(started.elapsed().as_millis() as u64);
        let queued = (queue.len() / block_align) as u64;
        let behind = expected.saturating_sub(written + queued);
        if behind > frames_for(100) {
            let pad = behind.min(max_frames.saturating_sub(written));
            written += write_silence(&mut writer, pad).map_err(|e| e.to_string())?;
        }

        let frames = ((queue.len() / block_align) as u64).min(max_frames.saturating_sub(written));
        for _ in 0..frames * u64::from(CHANNELS) {
            let lo = queue.pop_front().unwrap_or(0);
            let hi = queue.pop_front().unwrap_or(0);
            writer
                .write_sample(i16::from_le_bytes([lo, hi]))
                .map_err(|e| e.to_string())?;
        }
        written += frames;
    }

    let _ = client.stop_stream();
    writer.finalize().map_err(|e| e.to_string())?;
    Ok(written)
}
