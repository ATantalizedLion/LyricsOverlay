"""Add syllable timing to LyricsOverlay's cached lyrics by aligning them to the audio.

Pipeline: find the song's cache entry -> separate vocals (Demucs) -> per lyric line,
force-align the line's syllables to the vocals with the multilingual MMS aligner ->
write the timing back into the cache entry, which the overlay picks up the next time
the song plays.

Usage: uv run align.py "path/to/song.flac"   (see README.md)
       uv run align.py --pending            (songs the overlay recorded from Spotify)
       uv run lab.py                        (tune settings and compare models in a browser)
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import sys
import tomllib
import unicodedata
from dataclasses import dataclass
from pathlib import Path

import numpy as np

from pipeline import (ALIGNERS, SEPARATORS, ModelPool, RunConfig, build_segments,  # noqa: F401
                      fmt_ms, load_defaults, run_alignment, shift_to_song_time, to_cache_lines)


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


RECORDING_FILE = "recording.wav"
# Written when an entry has been processed but no line could be aligned, so the overlay
# doesn't keep recording the song (see src/recorder.rs).
ATTEMPTED_MARKER = ".align-attempted"


def lab_dir(entry: Path) -> Path:
    """Where separated vocals, emissions and lab runs for a cache entry are kept. Outside
    the overlay's cache so the overlay never has to know about them."""
    return data_dir() / "align-lab" / entry.name


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


def original_lines(entry: Path) -> list[dict]:
    """The entry's lines with their LRC timing, before any alignment touched them: from
    the backup the first alignment made, as long as it still has the same lines."""
    current = json.loads((entry / "lyrics.lrc").read_text(encoding="utf-8")).get("synced_lyrics") or []
    texts = ["".join(s["text"] for s in l["segments"]) if l.get("segments") else l["text"] for l in current]
    backup = entry / "lyrics.lrc.bak"
    if backup.exists():
        try:
            old = json.loads(backup.read_text(encoding="utf-8")).get("synced_lyrics") or []
            if [l["text"] for l in old] == texts:
                return [{"time_ms": int(l["time_ms"]), "text": l["text"]} for l in old]
        except (OSError, KeyError, json.JSONDecodeError):
            pass
    return [{"time_ms": int(l["time_ms"]), "text": t} for l, t in zip(current, texts)]


OVERLAY_INFO = "overlay.json"


def write_cache_lines(entry: Path, lines: list[dict], shift_ms: int = 0, source: str = "") -> Path:
    """Writes aligned lines into the entry's lyrics.lrc, keeping the original as .bak.

    Next to the lab's files it notes how the timing was made (`source`) and the audio
    shift the alignment applied, so the lab can score that timing against tapped
    references, which are kept relative to the recording."""
    lyrics_file = entry / "lyrics.lrc"
    song = json.loads(lyrics_file.read_text(encoding="utf-8"))
    backup = entry / "lyrics.lrc.bak"
    if not backup.exists():
        shutil.copyfile(lyrics_file, backup)
    song["synced_lyrics"] = to_cache_lines(lines)
    lyrics_file.write_text(json.dumps(song, indent=2, ensure_ascii=False), encoding="utf-8")
    # Whatever wrote this, a lab run applied earlier is no longer what the overlay shows
    # (the lab writes applied.json again right after, when it's the one applying).
    (lab_dir(entry) / "applied.json").unlink(missing_ok=True)
    info = lab_dir(entry) / OVERLAY_INFO
    if any(l.get("segments") for l in lines):
        info.parent.mkdir(parents=True, exist_ok=True)
        info.write_text(json.dumps({"shift_applied_ms": int(shift_ms or 0), "source": source}), encoding="utf-8")
    else:
        info.unlink(missing_ok=True)
    return lyrics_file


def overlay_info(entry: Path) -> dict | None:
    """What write_cache_lines noted about the entry's current timing; None if unknown."""
    try:
        return json.loads((lab_dir(entry) / OVERLAY_INFO).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def process_entry(entry: Path, audio: Path, pool: ModelPool, cfg: RunConfig, args) -> int:
    """Aligns one cache entry's lyrics to `audio`. Returns the number of lines aligned."""
    lines = original_lines(entry)
    if not lines:
        print(f"{entry.name}: no line-synced lyrics to align, skipping.")
        return 0
    print(f"Aligning {entry.name}")
    result = run_alignment(lines, audio, recording_offset_ms(audio), cfg, pool,
                           cache_dir=lab_dir(entry))
    aligned = result["stats"]["aligned"]
    lrc = to_enhanced_lrc(result["lines"])
    if args.lrc_out:
        args.lrc_out.write_text(lrc, encoding="utf-8")
        print(f"Wrote {args.lrc_out}")
    if args.dry_run:
        print(lrc)
        return aligned
    if aligned:
        lyrics_file = write_cache_lines(entry, result["lines"], result["stats"].get("shift_applied_ms") or 0,
                                        "align.py")
        print(f"Updated {lyrics_file} (original kept as lyrics.lrc.bak). "
              "It shows up the next time the song starts.")
    return aligned


AUDIBLE_LEVEL = 32  # int16, about -60 dBFS; matches src/recorder.rs
MIN_AUDIBLE_SHARE = 0.3


def audible_share(path: Path) -> float:
    """Share of frames with any sound. Recordings the overlay made while Spotify played on
    another device (Spotify Connect) are near-silent."""
    import soundfile as sf

    audible = total = 0
    # Read as float: libsndfile reads float WAVs as all zeros when asked for int16.
    for block in sf.blocks(str(path), blocksize=1 << 16, dtype="float32", always_2d=True):
        audible += int((np.abs(block).max(axis=1) > AUDIBLE_LEVEL / 32768).sum())
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


def config_from_args(args) -> RunConfig:
    """The lab's saved defaults (if any), overridden by whatever was given on the command line."""
    cfg = load_defaults(data_dir()).to_dict()
    overrides = {
        "separator": "none" if args.no_separation else (args.separator or args.demucs_model),
        "aligner": args.aligner,
        "mode": args.mode,
        "unit": "word" if args.words else None,
        "lang": args.lang,
        "min_confidence": args.min_confidence,
        "offset_ms": args.offset,
    }
    cfg.update({k: v for k, v in overrides.items() if v is not None})
    return RunConfig.from_dict(cfg)


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
    g = p.add_argument_group("alignment settings (default: what the lab saved with 'Use as default', "
                             "otherwise the built-in defaults)")
    g.add_argument("--separator", choices=sorted(SEPARATORS), help="Vocal separation model")
    g.add_argument("--demucs-model", help=argparse.SUPPRESS)
    g.add_argument("--no-separation", action="store_true", help="Same as --separator none")
    g.add_argument("--aligner", choices=sorted(ALIGNERS), help="Alignment model")
    g.add_argument("--mode", choices=["line", "song"], help="Align line by line, or the whole song at once")
    g.add_argument("--lang", help="Lyrics language (ISO code, e.g. en, nl, ja) for syllable splitting; detected if omitted")
    g.add_argument("--words", action="store_true", help="Time whole words instead of syllables")
    g.add_argument("--min-confidence", type=float, help="Drop timing for lines aligned less confidently than this")
    g.add_argument("--offset", type=int, help="Shift all syllable times by this many ms (positive = later)")
    p.add_argument("--device", default=None, help="cpu or cuda (default: cuda if available)")
    p.add_argument("--dry-run", action="store_true", help="Print the result as Enhanced LRC instead of writing the cache")
    p.add_argument("--lrc-out", type=Path, help="Also write the result as an Enhanced LRC file")
    args = p.parse_args()
    cfg = config_from_args(args)
    print(f"Settings: {cfg.summary()}")

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
        pool = ModelPool(args.device)
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
                aligned = process_entry(entry, recording, pool, cfg, args)
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
    process_entry(entry, audio, ModelPool(args.device), cfg, args)


if __name__ == "__main__":
    main()
