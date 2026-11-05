"""Add syllable timing to LyricsOverlay's cached lyrics by aligning them to the audio.

Pipeline: find the song's cache entry -> separate vocals (Demucs) -> per lyric line,
force-align the line's syllables to the vocals with the multilingual MMS aligner ->
write the timing back into the cache entry, which the overlay picks up the next time
the song plays.

Usage: uv run align.py "path/to/song.flac"   (see README.md)
       uv run align.py --pending            (songs the overlay recorded from Spotify)
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tomllib
import unicodedata
from dataclasses import dataclass
from pathlib import Path

import numpy as np

from ctc import forced_align
from text import BLANK, STAR, Syllable, syllabify_line

ALIGN_SR = 16_000
LINE_PAD_BEFORE_MS = 400
LINE_PAD_AFTER_MS = 300
MAX_WINDOW_MS = 30_000
# Mean log-prob per aligned frame below which a line's timing is dropped (the overlay
# then shows that line without karaoke). Tuned loosely; see --min-confidence.
DEFAULT_MIN_CONFIDENCE = -4.0


# ---------------------------------------------------------------------------
# Finding the cache entry


def data_dir() -> Path:
    appdata = os.environ.get("APPDATA")
    return Path(appdata) / "LyricsOverlay" if appdata else Path(".")


def cache_root() -> Path:
    folder = "cache"
    config = data_dir() / "config.toml"
    if config.exists():
        try:
            folder = tomllib.loads(config.read_text(encoding="utf-8")).get("cache_folder", folder)
        except tomllib.TOMLDecodeError:
            pass
    return data_dir() / folder


def norm(s: str) -> str:
    s = unicodedata.normalize("NFKC", s).casefold()
    # Drop "(feat. ...)", "- Remastered 2011" style suffixes and punctuation.
    s = re.sub(r"\s*[\(\[].*?[\)\]]", "", s)
    s = re.sub(r"\s+-\s+.*$", "", s)
    return re.sub(r"[\W_]+", "", s)


@dataclass
class Tags:
    title: str
    artist: str
    duration_sec: float | None


def read_tags(audio: Path) -> Tags:
    title = artist = ""
    duration = None
    try:
        import mutagen

        f = mutagen.File(audio, easy=True)
        if f is not None:
            title = (f.get("title") or [""])[0]
            artist = (f.get("artist") or [""])[0]
            duration = getattr(f.info, "length", None)
    except Exception as e:  # noqa: BLE001 - tags are best effort
        print(f"Could not read tags: {e}", file=sys.stderr)
    if not title:
        # Fall back to "Artist - Title.ext"
        stem = audio.stem
        if " - " in stem:
            artist, title = stem.split(" - ", 1)
        else:
            title = stem
    return Tags(title, artist, duration)


def find_cache_entry(root: Path, tags: Tags) -> Path:
    candidates = []
    for meta_file in root.glob("*/.meta"):
        try:
            meta = json.loads(meta_file.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if norm(meta.get("track_name", "")) != norm(tags.title):
            continue
        a, b = norm(meta.get("artist_name", "")), norm(tags.artist)
        if a and b and a not in b and b not in a:
            continue
        diff = abs(meta.get("duration_sec", 0) - tags.duration_sec) if tags.duration_sec else 0
        if diff > 5:
            continue
        candidates.append((diff, meta_file.parent))
    if not candidates:
        raise SystemExit(
            f"No cached lyrics found for '{tags.artist} - {tags.title}' in {root}.\n"
            "Play the song once with LyricsOverlay running (and caching on) so its lyrics "
            "get cached, or point at the entry with --entry."
        )
    candidates.sort(key=lambda c: c[0])
    return candidates[0][1]


# ---------------------------------------------------------------------------
# Audio


def load_audio(path: Path, sr: int):
    """Load as a float32 torch tensor (channels, samples) at `sr`."""
    import torch
    import torchaudio

    try:
        import soundfile as sf

        data, file_sr = sf.read(str(path), dtype="float32", always_2d=True)
        wav = torch.from_numpy(data.T.copy())
    except Exception:  # noqa: BLE001 - e.g. m4a/aac, which libsndfile can't read
        if not shutil.which("ffmpeg"):
            raise SystemExit(f"Can't read {path.name}; install ffmpeg or convert it to flac/wav/mp3.")
        raw = subprocess.run(
            ["ffmpeg", "-v", "error", "-i", str(path), "-f", "f32le", "-ac", "2", "-ar", str(sr), "-"],
            check=True, capture_output=True,
        ).stdout
        wav = torch.from_numpy(np.frombuffer(raw, dtype=np.float32).reshape(-1, 2).T.copy())
        file_sr = sr
    if file_sr != sr:
        wav = torchaudio.functional.resample(wav, file_sr, sr)
    return wav


def separate_vocals(wav, sr: int, device: str, model):
    """Returns mono vocals at 16 kHz."""
    import torch
    import torchaudio
    from demucs.apply import apply_model

    if wav.shape[0] == 1:
        wav = wav.repeat(2, 1)
    ref = wav.mean(0)
    mean, std = ref.mean(), ref.std() + 1e-8
    with torch.inference_mode():
        sources = apply_model(model, ((wav - mean) / std)[None], device=device,
                              split=True, overlap=0.25, progress=True)[0]
    vocals = sources[model.sources.index("vocals")] * std + mean
    return torchaudio.functional.resample(vocals.mean(0, keepdim=True), sr, ALIGN_SR)


# ---------------------------------------------------------------------------
# Alignment


class Aligner:
    def __init__(self, device: str):
        import torch
        import torchaudio

        self.torch = torch
        self.device = device
        bundle = torchaudio.pipelines.MMS_FA
        labels = bundle.get_labels(star="*")
        assert labels[BLANK] == "-" and labels[STAR] == "*", labels
        self.model = bundle.get_model(with_star=True).to(device).eval()

    def log_probs(self, wav16k) -> tuple[np.ndarray, float]:
        """(frames x labels) log-probabilities and seconds per frame."""
        torch = self.torch
        with torch.inference_mode():
            emission, _ = self.model(wav16k.to(self.device))
            emission = torch.log_softmax(emission[0].float(), dim=-1)
        frames = emission.shape[0]
        return emission.cpu().numpy(), wav16k.shape[-1] / ALIGN_SR / max(frames, 1)


def align_line(aligner: Aligner, vocals16k, start_ms: int, end_ms: int,
               syllables: list[Syllable]):
    """Times (start_ms, end_ms) per syllable, or None for syllables with no tokens.
    Returns (times, confidence) or None if alignment failed."""
    a = max(0, int(start_ms * ALIGN_SR / 1000))
    b = min(vocals16k.shape[-1], int(end_ms * ALIGN_SR / 1000))
    if b - a < ALIGN_SR // 10:
        return None
    log_probs, sec_per_frame = aligner.log_probs(vocals16k[:, a:b])

    # A leading/trailing star token soaks up whatever else is in the window (the end of
    # the previous line, ad-libs) so it doesn't get forced onto our first/last syllable.
    targets, owner = [STAR], [-1]
    for i, syl in enumerate(syllables):
        targets += syl.tokens
        owner += [i] * len(syl.tokens)
    targets.append(STAR)
    owner.append(-1)
    if len(targets) == 2:
        return None

    result = forced_align(log_probs, targets, blank=BLANK)
    if result is None:
        return None
    spans, confidence = result

    def to_ms(frame: int) -> int:
        return start_ms + round(frame * sec_per_frame * 1000)

    times: list[tuple[int, int] | None] = [None] * len(syllables)
    for (f0, f1), i in zip(spans, owner):
        if i < 0:
            continue
        t0, t1 = to_ms(f0), to_ms(f1 + 1)
        if times[i] is None:
            times[i] = (t0, t1)
        else:
            times[i] = (times[i][0], t1)
    return times, confidence


def build_segments(syllables: list[Syllable], times, line_start_ms: int) -> list[dict]:
    """Turn per-syllable times into the overlay's segment format, filling in syllables
    that had no tokens (numbers, symbols) from their neighbours, and making each
    syllable run until the next one starts when they're in the same word."""
    known = [i for i, t in enumerate(times) if t is not None]
    filled: list[tuple[int, int]] = []
    for i, t in enumerate(times):
        if t is not None:
            filled.append(t)
            continue
        prev = max((k for k in known if k < i), default=None)
        nxt = min((k for k in known if k > i), default=None)
        if prev is not None and nxt is not None:
            s = times[prev][1]
            filled.append((s, max(s, times[nxt][0])))
        elif prev is not None:
            s = times[prev][1]
            filled.append((s, s))
        elif nxt is not None:
            s = times[nxt][0]
            filled.append((s, s))
        else:
            filled.append((line_start_ms, line_start_ms))

    segments = []
    for i, (syl, (s, e)) in enumerate(zip(syllables, filled)):
        if i + 1 < len(syllables) and syllables[i + 1].word_index == syl.word_index:
            e = max(e, filled[i + 1][0])
        if segments:
            s = max(s, segments[-1]["start_ms"])
        segments.append({"start_ms": int(s), "end_ms": int(max(s, e)), "text": syl.text})
    return segments


# ---------------------------------------------------------------------------


def detect_language(lines: list[str]) -> str | None:
    try:
        from langdetect import DetectorFactory, detect

        DetectorFactory.seed = 0
        return detect("\n".join(lines))
    except Exception:  # noqa: BLE001
        return None


def fmt_ms(ms: int) -> str:
    return f"{ms // 60000:02}:{(ms % 60000) / 1000:05.2f}"


def to_enhanced_lrc(lines: list[dict]) -> str:
    out = []
    for line in lines:
        segs = line.get("segments") or []
        if not segs:
            out.append(f"[{fmt_ms(line['time_ms'])}]{line['text']}")
            continue
        body = "".join(f"<{fmt_ms(s['start_ms'])}>{s['text']}" for s in segs)
        out.append(f"[{fmt_ms(line['time_ms'])}]{body}<{fmt_ms(segs[-1]['end_ms'])}>")
    return "\n".join(out) + "\n"


RECORDING_FILE = "recording.wav"
# Written when an entry has been processed but no line could be aligned, so the overlay
# doesn't keep recording the song (see src/recorder.rs).
ATTEMPTED_MARKER = ".align-attempted"


class Models:
    """Loaded lazily and shared across songs in --pending mode."""

    def __init__(self, args):
        import torch
        from uroman import Uroman

        self.args = args
        self.device = args.device or ("cuda" if torch.cuda.is_available() else "cpu")
        self.romanise = Uroman().romanize_string
        self._aligner = None
        self._demucs = None

    @property
    def aligner(self) -> Aligner:
        if self._aligner is None:
            self._aligner = Aligner(self.device)
        return self._aligner

    def vocals(self, audio: Path):
        offset_ms = recording_offset_ms(audio)
        if self.args.no_separation:
            return shift_to_song_time(load_audio(audio, ALIGN_SR), ALIGN_SR, offset_ms).mean(0, keepdim=True)
        if self._demucs is None:
            from demucs.pretrained import get_model

            self._demucs = get_model(self.args.demucs_model)
            self._demucs.eval()
        sep_sr = self._demucs.samplerate
        wav = shift_to_song_time(load_audio(audio, sep_sr), sep_sr, offset_ms)
        return separate_vocals(wav, sep_sr, self.device, self._demucs)


def recording_offset_ms(audio: Path) -> int:
    """For a recording made by the overlay, the song position at its first sample (from
    recording.json next to it); 0 for any other file."""
    meta = audio.parent / "recording.json"
    if audio.name != RECORDING_FILE or not meta.exists():
        return 0
    try:
        return int(json.loads(meta.read_text(encoding="utf-8")).get("offset_ms", 0))
    except (OSError, ValueError, json.JSONDecodeError):
        return 0


def shift_to_song_time(wav, sr: int, offset_ms: int):
    """Pads (offset > 0) or trims (offset < 0) the start so sample 0 is song time 0."""
    import torch

    n = abs(offset_ms) * sr // 1000
    if offset_ms > 0:
        return torch.cat([torch.zeros(wav.shape[0], n, dtype=wav.dtype), wav], dim=1)
    return wav[:, n:]


def process_entry(entry: Path, audio: Path, models: Models) -> int:
    """Aligns one cache entry's lyrics to `audio`. Returns the number of lines aligned."""
    args = models.args
    lyrics_file = entry / "lyrics.lrc"
    song = json.loads(lyrics_file.read_text(encoding="utf-8"))
    lines = song.get("synced_lyrics") or []
    if not lines:
        print(f"{entry.name}: no line-synced lyrics to align, skipping.")
        return 0
    lang = None if args.words else (args.lang or detect_language([l["text"] for l in lines]))
    print(f"Aligning {entry.name} ({len(lines)} lines, device {models.device}, "
          f"syllable language {lang or 'none (whole words)'})")

    if not args.no_separation:
        print("Separating vocals...")
    vocals = models.vocals(audio)

    print("Aligning lines...")
    song_end_ms = int(vocals.shape[-1] * 1000 / ALIGN_SR)
    aligned = skipped = 0
    for i, line in enumerate(lines):
        line.pop("segments", None)
        text = line["text"]
        if not text.strip():
            continue
        next_ms = lines[i + 1]["time_ms"] if i + 1 < len(lines) else song_end_ms
        start = max(0, line["time_ms"] - LINE_PAD_BEFORE_MS)
        end = min(next_ms + LINE_PAD_AFTER_MS, start + MAX_WINDOW_MS, song_end_ms)
        syllables = syllabify_line(text, lang, models.romanise)
        result = align_line(models.aligner, vocals, start, end, syllables)
        if result is None or result[1] < args.min_confidence:
            score = "failed" if result is None else f"{result[1]:.2f}"
            print(f"  skip  [{fmt_ms(line['time_ms'])}] ({score}) {text}")
            skipped += 1
            continue
        times, confidence = result
        line["segments"] = build_segments(syllables, times, line["time_ms"])
        line["text"] = "".join(s["text"] for s in line["segments"])
        print(f"  ok    [{fmt_ms(line['time_ms'])}] ({confidence:.2f}) "
              + "|".join(s["text"].strip() for s in line["segments"]))
        aligned += 1

    print(f"Aligned {aligned} lines, skipped {skipped}.")
    lrc = to_enhanced_lrc(lines)
    if args.lrc_out:
        args.lrc_out.write_text(lrc, encoding="utf-8")
        print(f"Wrote {args.lrc_out}")
    if args.dry_run:
        print(lrc)
        return aligned

    if aligned:
        backup = entry / "lyrics.lrc.bak"
        if not backup.exists():
            shutil.copyfile(lyrics_file, backup)
        lyrics_file.write_text(json.dumps(song, indent=2, ensure_ascii=False), encoding="utf-8")
        print(f"Updated {lyrics_file} (original kept as {backup.name}). "
              "It shows up the next time the song starts.")
    return aligned


AUDIBLE_LEVEL = 32  # int16, about -60 dBFS; matches src/recorder.rs
MIN_AUDIBLE_SHARE = 0.3


def audible_share(path: Path) -> float:
    """Share of frames with any sound. Recordings the overlay made while Spotify played on
    another device (Spotify Connect) are near-silent."""
    import soundfile as sf

    audible = total = 0
    for block in sf.blocks(str(path), blocksize=1 << 16, dtype="int16", always_2d=True):
        audible += int((np.abs(block.astype(np.int32)).max(axis=1) > AUDIBLE_LEVEL).sum())
        total += block.shape[0]
    return audible / total if total else 0.0


MAX_LENGTH_MISMATCH_SEC = 8.0


def recording_problem(entry: Path) -> str | None:
    """Why the overlay's recording in `entry` can't be used, if it can't: near-silent
    (Spotify was playing on another device), or not the length of the song (older builds
    could save the next song's audio into the wrong folder)."""
    import soundfile as sf

    recording = entry / RECORDING_FILE
    share = audible_share(recording)
    if share < MIN_AUDIBLE_SHARE:
        return f"recording is almost silent ({share:.0%} audible), probably played on another device"
    try:
        duration = float(json.loads((entry / ".meta").read_text(encoding="utf-8"))["duration_sec"])
    except (OSError, KeyError, ValueError, json.JSONDecodeError):
        return None
    covered = sf.info(str(recording)).duration + recording_offset_ms(recording) / 1000
    if abs(covered - duration) > MAX_LENGTH_MISMATCH_SEC:
        return f"recording covers {covered:.0f}s but the song is {duration:.0f}s, so it's not this song's audio"
    return None


def entry_needs_alignment(entry: Path) -> bool:
    try:
        song = json.loads((entry / "lyrics.lrc").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return False
    lines = song.get("synced_lyrics") or []
    return bool(lines) and not any(l.get("segments") for l in lines)


def pending_entries(root: Path, redo: bool = False) -> list[Path]:
    """Cache entries with a recording from the overlay that haven't been aligned (or
    tried) yet; with `redo`, every entry that has a recording."""
    entries = (rec.parent for rec in root.glob(f"*/{RECORDING_FILE}"))
    if redo:
        return sorted(entries)
    return sorted(e for e in entries
                  if entry_needs_alignment(e) and not (e / ATTEMPTED_MARKER).exists())


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("audio", type=Path, nargs="?",
                   help="The song's audio file (flac, mp3, wav, ogg; m4a needs ffmpeg). "
                        "Can be left out with --entry if the overlay recorded the song.")
    p.add_argument("--pending", action="store_true",
                   help="Process the songs the overlay recorded from Spotify that haven't been aligned yet")
    p.add_argument("--redo", action="store_true",
                   help="With --pending, re-align every recorded song, including ones already aligned")
    p.add_argument("--delete-audio", action="store_true",
                   help="With --pending, delete each recording once it has been processed")
    p.add_argument("--entry", type=Path, help="Cache entry folder to update, instead of finding it from the file's tags")
    p.add_argument("--lang", help="Lyrics language (ISO code, e.g. en, nl, ja) for syllable splitting; detected if omitted")
    p.add_argument("--words", action="store_true", help="Time whole words instead of syllables")
    p.add_argument("--no-separation", action="store_true", help="Skip vocal separation (faster, much less accurate)")
    p.add_argument("--demucs-model", default="htdemucs", help="Demucs model name (default: htdemucs; htdemucs_ft is slower but cleaner)")
    p.add_argument("--min-confidence", type=float, default=DEFAULT_MIN_CONFIDENCE,
                   help=f"Drop timing for lines aligned less confidently than this (default {DEFAULT_MIN_CONFIDENCE})")
    p.add_argument("--device", default=None, help="cpu or cuda (default: cuda if available)")
    p.add_argument("--dry-run", action="store_true", help="Print the result as Enhanced LRC instead of writing the cache")
    p.add_argument("--lrc-out", type=Path, help="Also write the result as an Enhanced LRC file")
    args = p.parse_args()

    if args.pending:
        if args.audio or args.entry or args.lrc_out:
            p.error("--pending processes the recordings in the cache; don't pass a file, --entry or --lrc-out")
        root = cache_root()
        entries = pending_entries(root, redo=args.redo)
        if not entries:
            hint = "" if args.redo else " (use --redo to re-align songs that were already processed)"
            print(f"No recordings waiting in {root}{hint}. Turn on 'Record Spotify for syllable "
                  "timing' in the overlay's settings and play some songs from the start.")
            return
        print(f"{len(entries)} recording(s) to process.")
        models = Models(args)
        for n, entry in enumerate(entries, 1):
            print(f"\n[{n}/{len(entries)}] ", end="")
            recording = entry / RECORDING_FILE
            problem = recording_problem(entry)
            if problem:
                print(f"{entry.name}: {problem}. Deleting the recording so the song can be "
                      "recorded again.")
                if not args.dry_run:
                    recording.unlink(missing_ok=True)
                    (entry / "recording.json").unlink(missing_ok=True)
                continue
            try:
                aligned = process_entry(entry, recording, models)
            except Exception as e:  # noqa: BLE001 - keep going with the other songs
                print(f"Failed: {e}")
                continue
            if args.dry_run:
                continue
            marker = entry / ATTEMPTED_MARKER
            if aligned:
                marker.unlink(missing_ok=True)
            else:
                marker.touch()
            if args.delete_audio:
                recording.unlink(missing_ok=True)
        return

    if args.redo or args.delete_audio:
        p.error("--redo and --delete-audio only apply to --pending")
    audio = args.audio
    if audio is None:
        if args.entry is None or not (args.entry / RECORDING_FILE).exists():
            p.error("give an audio file, --entry for a song the overlay recorded, "
                    "or --pending to process all recordings")
        audio = args.entry / RECORDING_FILE
    if not audio.exists():
        raise SystemExit(f"{audio} doesn't exist")
    entry = args.entry or find_cache_entry(cache_root(), read_tags(audio))
    process_entry(entry, audio, Models(args))


if __name__ == "__main__":
    main()
