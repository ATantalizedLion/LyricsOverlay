"""Import a hand-timed test set into the sync lab.

JamendoLyrics (https://huggingface.co/datasets/morgangautho/jamendolyrics) has 80 songs
with Creative Commons audio and every word's start time annotated by hand. Each song
becomes a lab song: its line times become the LRC, the audio is registered as its source,
and the hand-timed word starts become its reference, so every run and batch on it gets
an error score against the truth.

The songs are added to the overlay's cache folder as "JamendoLyrics-<song>" entries. The
overlay only looks entries up by the track playing, so they never show up in it.

Usage: uv run testset.py <jamendolyrics folder> [--lang en,de] [--remove]
"""

from __future__ import annotations

import argparse
import csv
import difflib
import json
import math
import shutil
import sys
import time
from pathlib import Path

from align import cache_root, lab_dir
from text import split_units

PREFIX = "JamendoLyrics-"
LANGS = {"English": "en", "Spanish": "es", "German": "de", "French": "fr"}


def line_units(text: str) -> list[dict]:
    """The words of a line as the lab shows them (offset into the line, text)."""
    units, pos = [], 0
    for u in split_units(text):
        if u.strip():
            units.append({"offset": pos, "text": u})
        pos += len(u)
    return units


def _norm(word: str) -> str:
    return "".join(ch for ch in word.casefold() if ch.isalnum())


def _rows(path: Path) -> list[list[str]]:
    """CSV rows without a header row (the dataset's files may or may not have one)."""
    rows = [r for r in csv.reader(path.read_text(encoding="utf-8").splitlines()) if r]
    if rows:
        try:
            float(rows[0][0])
        except ValueError:
            rows = rows[1:]
    return rows


def word_starts(lines: list[str], words: list[str], starts_ms: list[int | None]) -> dict[str, list[int | None]]:
    """Each line's word start times, matching the lab's words to the annotated ones. They
    can split differently (hyphens, apostrophes), so this matches the two word sequences
    and leaves words it can't pair untimed."""
    units = [(li, k, _norm(u["text"])) for li, line in enumerate(lines) for k, u in enumerate(line_units(line))]
    out: dict[str, list[int | None]] = {str(li): [None] * len(line_units(line)) for li, line in enumerate(lines)}
    sm = difflib.SequenceMatcher(a=[u[2] for u in units], b=[_norm(w) for w in words], autojunk=False)
    for block in sm.get_matching_blocks():
        for i in range(block.size):
            li, k, _ = units[block.a + i]
            if starts_ms[block.b + i] is not None:
                out[str(li)][k] = starts_ms[block.b + i]
    return {li: taps for li, taps in out.items() if any(t is not None for t in taps)}


def find_audio(root: Path, filepath: str) -> Path:
    """The song's audio. In the dataset, mp3/ holds symlinks into subsets/<lang>/mp3; a
    checkout without symlink support (Windows) turns them into small text files holding
    the link target."""
    audio = root / "mp3" / filepath
    if audio.is_file() and audio.stat().st_size < 1024:
        try:
            target = (audio.parent / audio.read_text(encoding="utf-8").strip()).resolve()
            if target.is_file():
                return target
        except (OSError, UnicodeDecodeError):
            pass
    if audio.is_file() and audio.stat().st_size >= 1024:
        return audio
    found = sorted(root.glob(f"subsets/*/mp3/{filepath}"))
    return found[0] if found else audio


def import_song(root: Path, row: dict) -> str | None:
    name = Path(row["Filepath"]).stem
    audio = find_audio(root, row["Filepath"])
    lines_csv = root / "annotations" / "lines" / f"{name}.csv"
    words_csv = root / "annotations" / "words" / f"{name}.csv"
    words_txt = root / "lyrics" / f"{name}.words.txt"
    if not (audio.exists() and lines_csv.exists() and words_csv.exists() and words_txt.exists()):
        print(f"  {name}: files missing, skipped")
        return None

    lrc = [{"time_ms": round(float(r[0]) * 1000), "text": r[2].strip()} for r in _rows(lines_csv) if len(r) >= 3]
    words = [w.strip() for w in words_txt.read_text(encoding="utf-8").splitlines()]
    while words and not words[-1]:
        words.pop()
    starts = [None if math.isnan(float(r[0])) else round(float(r[0]) * 1000) for r in _rows(words_csv)]
    if len(words) != len(starts):
        print(f"  {name}: {len(words)} words but {len(starts)} timestamps, skipped")
        return None

    try:
        import soundfile as sf

        duration = sf.info(str(audio)).duration
    except Exception:  # noqa: BLE001 - only used for display
        duration = (lrc[-1]["time_ms"] / 1000 + 5) if lrc else 0

    entry = cache_root() / f"{PREFIX}{name}"
    entry.mkdir(parents=True, exist_ok=True)
    meta = {"track_name": row.get("Title") or name, "artist_name": row.get("Artist") or "JamendoLyrics",
            "duration_sec": round(duration, 3), "test_set": "JamendoLyrics",
            "language": LANGS.get(row.get("Language", ""), ""), "license": row.get("LicenseType", "")}
    (entry / ".meta").write_text(json.dumps(meta, ensure_ascii=False), encoding="utf-8")
    (entry / "lyrics.lrc").write_text(json.dumps({"synced_lyrics": lrc, "plain_lyrics": []}, ensure_ascii=False),
                                      encoding="utf-8")
    lab = lab_dir(entry)
    lab.mkdir(parents=True, exist_ok=True)
    (lab / "source.json").write_text(json.dumps({"audio": str(audio.resolve())}), encoding="utf-8")
    texts = [l["text"] for l in lrc]
    taps = word_starts(texts, words, starts)
    (lab / "reference.json").write_text(json.dumps({
        "words": taps, "texts": {li: texts[int(li)] for li in taps}, "frame": "recording",
        "source": "JamendoLyrics hand annotation", "locked": True, "updated": time.time()}, ensure_ascii=False), encoding="utf-8")
    timed = sum(t is not None for v in taps.values() for t in v)
    total = sum(len(line_units(t)) for t in texts)
    print(f"  {name}: {len(lrc)} lines, {timed}/{total} words timed")
    return entry.name


def remove_all() -> int:
    """Removes every imported test song and its lab files (runs included)."""
    n = 0
    for entry in cache_root().glob(f"{PREFIX}*"):
        shutil.rmtree(lab_dir(entry), ignore_errors=True)
        shutil.rmtree(entry, ignore_errors=True)
        n += 1
    return n


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("folder", type=Path, nargs="?", help="The downloaded jamendolyrics folder")
    p.add_argument("--lang", help="Only these languages, e.g. en or en,de (default: all)")
    p.add_argument("--remove", action="store_true", help="Remove all imported test songs instead")
    args = p.parse_args()
    if args.remove:
        print(f"Removed {remove_all()} test songs.")
        return
    if not args.folder or not (args.folder / "JamendoLyrics.csv").exists():
        sys.exit("Pass the jamendolyrics folder (the one with JamendoLyrics.csv in it).")
    langs = set(args.lang.split(",")) if args.lang else None
    rows = list(csv.DictReader((args.folder / "JamendoLyrics.csv").read_text(encoding="utf-8").splitlines()))
    done = 0
    for row in rows:
        if langs and LANGS.get(row.get("Language", ""), "") not in langs:
            continue
        done += import_song(args.folder, row) is not None
    print(f"Imported {done} songs into {cache_root()}. Open the lab (uv run lab.py) to run them.")


if __name__ == "__main__":
    main()
