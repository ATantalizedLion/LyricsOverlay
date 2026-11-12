"""Sync lab: a local web page for tuning syllable alignment and comparing models.

Pick a song the overlay recorded (or pass your own audio files), run the alignment with
different separators, aligners and parameters, then compare the runs side by side while
the song plays, score them against timing you tap in yourself, and apply the best one to
the overlay's cache.

Usage: uv run lab.py                       (songs the overlay recorded from Spotify)
       uv run lab.py "D:\\Music\\song.flac"  (also add your own audio files)
"""

from __future__ import annotations

import argparse
import json
import mimetypes
import queue
import re
import secrets
import sys
import threading
import time
import traceback
import urllib.parse
import webbrowser
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import numpy as np

from align import (ATTEMPTED_MARKER, RECORDING_FILE, cache_root, data_dir, find_cache_entry, lab_dir, original_lines,
                   overlay_info, read_tags, recording_offset_ms, recording_problem, write_cache_lines)
from pipeline import (ALIGNERS, SEPARATORS, ModelPool, RunConfig, defaults_file, load_defaults,
                      run_alignment, vocals_cache_path)
from testset import line_units

HERE = Path(__file__).resolve().parent
PAGE = HERE / "lab.html"


# ---------------------------------------------------------------------------
# Songs


def read_json(path: Path, default=None):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return default


def write_json(path: Path, data) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(data, indent=1, ensure_ascii=False), encoding="utf-8")
    tmp.replace(path)


def entry_path(name: str) -> Path:
    if not name or "/" in name or "\\" in name or name in (".", ".."):
        raise LookupError("bad song id")
    path = cache_root() / name
    if not (path / "lyrics.lrc").exists():
        raise LookupError(f"no cached lyrics for {name}")
    return path


def audio_source(entry: Path) -> tuple[Path | None, int]:
    """The audio to align for an entry, and the song time of its first sample."""
    registered = read_json(lab_dir(entry) / "source.json", {}).get("audio")
    if registered and Path(registered).exists():
        return Path(registered), 0
    recording = entry / RECORDING_FILE
    if recording.exists():
        return recording, recording_offset_ms(recording)
    return None, 0


def list_songs() -> list[dict]:
    songs = []
    for meta_file in cache_root().glob("*/.meta"):
        entry = meta_file.parent
        audio, _ = audio_source(entry)
        lab = lab_dir(entry)
        runs = run_files(lab)
        if audio is None and not runs and not (lab / "reference.json").exists() \
                and not (entry / "lyrics.lrc.bak").exists():
            # Never recorded and nothing to review. (The .bak marks lyrics that were
            # aligned once, which stay reviewable after their recording is deleted.)
            continue
        try:
            lines = original_lines(entry)
        except (OSError, json.JSONDecodeError, KeyError):
            continue
        if not lines:
            continue
        meta = read_json(meta_file, {})
        current = read_json(entry / "lyrics.lrc", {}).get("synced_lyrics") or []
        if audio is None and not runs and not any(l.get("segments") for l in current) \
                and not (lab / "reference.json").exists():
            continue
        songs.append({
            "id": entry.name,
            "title": meta.get("track_name", entry.name),
            "artist": meta.get("artist_name", ""),
            "duration_s": meta.get("duration_sec"),
            "runs": len(runs),
            "has_reference": (lab / "reference.json").exists(),
            "aligned_in_overlay": any(l.get("segments") for l in current),
            "test_set": meta.get("test_set"),
            "source": None if audio is None else "recording" if audio.name == RECORDING_FILE else "file",
        })
    songs.sort(key=lambda s: (s["artist"].casefold(), s["title"].casefold()))
    return songs


def run_summary(run: dict) -> dict:
    return {k: v for k, v in run.items() if k != "lines"}


_summary_cache: dict[str, tuple[tuple, dict]] = {}


def run_files(lab: Path) -> list[Path]:
    runs = lab / "runs"
    return sorted(runs.glob("*.json")) if runs.exists() else []


def cached_run_summary(f: Path) -> dict | None:
    """The run without its lines, parsed once per file version: the library lists every
    run of every song, and run files are large."""
    try:
        st = f.stat()
    except OSError:
        return None
    stamp = (st.st_mtime_ns, st.st_size)
    hit = _summary_cache.get(str(f))
    if hit and hit[0] == stamp:
        return hit[1]
    run = read_json(f)
    summary = run_summary(run) if run else None
    if summary is not None:
        remember(_summary_cache, str(f), (stamp, summary), 5000)
    return summary


def remember(cache: dict, key, value, limit: int):
    """Stores `value`, dropping the oldest entries past `limit`."""
    cache[key] = value
    while len(cache) > limit:
        del cache[next(iter(cache))]
    return value


_problem_cache: dict[tuple, str | None] = {}


def cached_recording_problem(entry: Path) -> str | None:
    """recording_problem reads the whole recording, so remember it per file version."""
    rec = entry / RECORDING_FILE
    try:
        st = rec.stat()
    except OSError:
        return None
    key = (str(rec), st.st_mtime, st.st_size)
    if key not in _problem_cache:
        try:
            problem = recording_problem(entry)
        except Exception as e:  # noqa: BLE001
            problem = f"couldn't check the recording: {e}"
        remember(_problem_cache, key, problem, 2000)
    return _problem_cache[key]


def overlay_run(lines: list[dict], current: list[dict], info: dict | None = None) -> dict:
    """The syllable timing the overlay uses now (e.g. from align.py --pending), shown as a
    read-only run so new runs can be compared with it."""
    out = []
    for orig, cur in zip(lines, current):
        segs = cur.get("segments") or []
        out.append({"time_ms": cur["time_ms"], "orig_time_ms": orig["time_ms"], "text": cur["text"],
                    "segments": segs, "confidence": None,
                    "status": "ok" if segs else ("empty" if not cur["text"].strip() else "low")})
    sung = [l for l in out if l["status"] != "empty"]
    return {"id": "overlay", "label": "In overlay now", "readonly": True, "created": 0, "config": {},
            "lines": out, "stats": {"lines": len(sung), "aligned": sum(l["status"] == "ok" for l in sung),
                                    "mean_confidence": None, "total_s": None,
                                    # None: written before the shift was recorded, so unknown
                                    "shift_applied_ms": None if info is None else info.get("shift_applied_ms", 0)}}


def song_detail(name: str) -> dict:
    entry = entry_path(name)
    lab = lab_dir(entry)
    meta = read_json(entry / ".meta", {})
    audio, offset = audio_source(entry)
    lines = original_lines(entry)
    for line in lines:
        line["units"] = line_units(line["text"])
    runs = []
    for f in run_files(lab):
        run = read_json(f)
        if run:
            runs.append(run)
    runs.sort(key=lambda r: r.get("created", 0))
    applied = applied_run(lab)
    current = read_json(entry / "lyrics.lrc", {}).get("synced_lyrics") or []
    if any(l.get("segments") for l in current) and not (applied and any(r["id"] == applied.get("run_id") for r in runs)):
        runs.insert(0, overlay_run(lines, current, overlay_info(entry)))
    problem = cached_recording_problem(entry) if audio is not None and audio.name == RECORDING_FILE else None
    vocals = [s for s in SEPARATORS if s != "none" and vocals_cache_path(lab, s).exists()]
    return {
        "id": name,
        "title": meta.get("track_name", name),
        "artist": meta.get("artist_name", ""),
        "duration_ms": int(float(meta.get("duration_sec", 0)) * 1000),
        "audio": None if audio is None else {"name": audio.name, "offset_ms": offset,
                                              "kind": "recording" if audio.name == RECORDING_FILE else "file"},
        "problem": problem,
        "lines": lines,
        "runs": runs,
        "reference": remap_reference(read_json(lab / "reference.json"), lines),
        "applied": applied,
        "vocals": vocals,
    }


def unit_starts(units: list[dict], run_line: dict | None) -> list[int | None]:
    """Start time of each word of a line in a run (from the syllable that word starts in),
    or None where the run has no timing. Same as unitStarts in lab.html."""
    segs = (run_line or {}).get("segments") or []
    if not segs:
        return [None] * len(units)
    starts, pos = [], 0
    for s in segs:
        starts.append((pos, s["start_ms"]))
        pos += len(s["text"])
    out = []
    for u in units:
        best = None
        for p, ms in starts:
            if p <= u["offset"]:
                best = ms
            else:
                break
        out.append(best)
    return out


def score_run(run: dict, lines: list[dict], ref_words: dict) -> dict | None:
    """A run's word start errors against a reference (see scoreRun in lab.html): median
    absolute error, mean signed error (bias), share within 100 ms and share timed."""
    shift = run.get("stats", {}).get("shift_applied_ms") or 0
    errs, n = [], 0
    for key, taps in ref_words.items():
        li = int(key)
        if li >= len(lines) or li >= len(run.get("lines", [])):
            continue
        got = unit_starts(lines[li]["units"], run["lines"][li])
        for k, r in enumerate(taps):
            if r is None or k >= len(got):
                continue
            n += 1
            if got[k] is not None:
                errs.append(got[k] - shift - r)
    if not n:
        return None
    ab = sorted(abs(e) for e in errs)
    return {"n": n, "median": ab[len(ab) // 2] if ab else None,
            "bias": round(sum(errs) / len(errs)) if errs else None,
            "within": sum(e <= 100 for e in ab) / n, "timed": len(errs) / n}


_score_cache: dict[tuple, dict | None] = {}


def remap_reference(ref: dict | None, lines: list[dict]) -> dict | None:
    """Tapped words are stored per line number, with each line's text. If the lyrics
    changed since (lines added or removed), move each line's taps to the line with the
    same text and word count nearest its old number, and drop taps for lines that are gone."""
    if not ref or not ref.get("texts"):
        return ref
    words, texts = ref.get("words") or {}, ref["texts"]
    out, used = {}, set()
    # Lines saved without their text stay where they are, so they go first.
    for key, taps in sorted(words.items(), key=lambda kv: kv[0] in texts):
        try:
            old = int(key)
        except ValueError:
            continue
        text = texts.get(key)
        if text is None:
            if old < len(lines) and old not in used:
                used.add(old)
                out[str(old)] = taps
            continue
        matches = [i for i, l in enumerate(lines)
                   if l["text"] == text and len(l.get("units", [])) == len(taps) and i not in used]
        if matches:
            new = min(matches, key=lambda i: abs(i - old))
            used.add(new)
            out[str(new)] = taps
    dropped = sum(1 for k, taps in words.items() if any(t is not None for t in taps)) \
        - sum(1 for taps in out.values() if any(t is not None for t in taps))
    return {**ref, "words": out, "texts": {k: lines[int(k)]["text"] for k in out}, "dropped_lines": dropped}


# ---------------------------------------------------------------------------
# Lab caches (separated vocals and model output)


def lab_caches() -> list[Path]:
    root = data_dir() / "align-lab"
    return [d for d in root.glob("*/*") if d.name in ("vocals", "emissions") and d.is_dir()] if root.exists() else []


def cache_overview() -> dict:
    size, songs = 0, set()
    for d in lab_caches():
        for f in d.rglob("*"):
            if f.is_file():
                size += f.stat().st_size
                songs.add(d.parent.name)
    return {"mb": round(size / 1e6), "songs": len(songs)}


def clear_caches() -> dict:
    """Deletes separated vocals and model output for every song. Runs, references and the
    overlay's timing stay; the next run on a song separates it again."""
    import shutil

    before = cache_overview()
    for d in lab_caches():
        shutil.rmtree(d, ignore_errors=True)
    return before


# ---------------------------------------------------------------------------
# Removing recordings

# In-progress recordings live in the cache root until the song ends; leave recent ones
# alone in case the overlay is recording right now.
PARTIAL_MIN_AGE_S = 15 * 60


def _recording_files() -> tuple[list[Path], list[Path]]:
    """(cache entries with a recording, leftover in-progress files)."""
    root = cache_root()
    entries = sorted(rec.parent for rec in root.glob(f"*/{RECORDING_FILE}"))
    now = time.time()
    partials = [p for p in root.glob(".recording*.wav.part")
                if now - p.stat().st_mtime > PARTIAL_MIN_AGE_S]
    return entries, partials


def recordings_overview() -> dict:
    entries, partials = _recording_files()
    size = sum((e / RECORDING_FILE).stat().st_size for e in entries) + sum(p.stat().st_size for p in partials)
    aligned = 0
    for e in entries:
        lines = read_json(e / "lyrics.lrc", {}).get("synced_lyrics") or []
        aligned += any(l.get("segments") for l in lines)
    return {"recordings": len(entries), "leftovers": len(partials), "mb": round(size / 1e6),
            "aligned": aligned, "markers": len(list(cache_root().glob(f"*/{ATTEMPTED_MARKER}")))}


def delete_recordings(clear_timing: bool) -> dict:
    """Deletes every recording the overlay made, the lab's vocals and model output made from
    them, and the "already tried" markers, so the overlay records those songs again.
    With `clear_timing`, also takes the syllable timing made from them out of the cache
    (otherwise the overlay won't record songs that already have it)."""
    import shutil

    entries, partials = _recording_files()
    cleared = 0
    for entry in entries:
        for name in (RECORDING_FILE, "recording.json"):
            (entry / name).unlink(missing_ok=True)
        lab = lab_dir(entry)
        if not (lab / "source.json").exists():  # vocals came from the recording
            for sub in ("vocals", "emissions"):
                shutil.rmtree(lab / sub, ignore_errors=True)
        if clear_timing:
            current = read_json(entry / "lyrics.lrc", {}).get("synced_lyrics") or []
            if any(l.get("segments") for l in current):
                write_cache_lines(entry, original_lines(entry))
                (lab / "applied.json").unlink(missing_ok=True)
                cleared += 1
    for p in partials:
        p.unlink(missing_ok=True)
    markers = list(cache_root().glob(f"*/{ATTEMPTED_MARKER}"))
    for m in markers:
        m.unlink(missing_ok=True)
    return {"recordings": len(entries), "leftovers": len(partials), "cleared": cleared, "markers": len(markers)}


# ---------------------------------------------------------------------------
# Library: the same settings on every song


def library() -> dict:
    """Every song with its runs (without per-line detail), for comparing settings batches
    across the whole library."""
    songs = []
    for song in list_songs():
        entry = cache_root() / song["id"]
        lab = lab_dir(entry)
        pairs = [(f, r) for f in run_files(lab) if (r := cached_run_summary(f))]
        runs = [r for _, r in pairs]
        ref_file = lab / "reference.json"
        if ref_file.exists() and pairs:
            # Score every run against the reference, once per run and reference version.
            lines = original_lines(entry)
            for line in lines:
                line["units"] = line_units(line["text"])
            ref = None
            rst = ref_file.stat()
            runs = []
            for f, summary in pairs:
                st = f.stat()
                key = (str(f), st.st_mtime_ns, st.st_size, rst.st_mtime_ns, rst.st_size)
                if key not in _score_cache:
                    if ref is None:
                        ref = (remap_reference(read_json(ref_file), lines) or {}).get("words") or {}
                    run = read_json(f)
                    remember(_score_cache, key, score_run(run, lines, ref) if run else None, 5000)
                runs.append(dict(summary, score=_score_cache[key]))
        song["run_list"] = sorted(runs, key=lambda r: r.get("created", 0))
        song["applied"] = applied_run(lab)
        songs.append(song)
    return {"songs": songs}


def batch_runs(batch: str) -> list[tuple[Path, Path, dict]]:
    """(entry, run file, run) for each song's run in `batch`."""
    out = []
    for f in (data_dir() / "align-lab").glob("*/runs/*.json"):
        run = read_json(f)
        if run and run.get("batch") == batch:
            entry = cache_root() / f.parent.parent.name
            if (entry / "lyrics.lrc").exists():
                out.append((entry, f, run))
    return out


def applied_run(lab: Path) -> dict | None:
    """Which lab run the overlay uses, if that run still exists. (If it was deleted, the
    overlay keeps its timing, which then shows as "In overlay now".)"""
    applied = read_json(lab / "applied.json")
    if applied and re.fullmatch(r"[\w-]+", applied.get("run_id") or "") \
            and (lab / "runs" / f"{applied['run_id']}.json").exists():
        return applied
    return None


def stale_run(entry: Path, run: dict) -> bool:
    """True if the song's lyrics changed since the run, so writing it would bring back
    the old lines."""
    return [l["text"] for l in run["lines"]] != [l["text"] for l in original_lines(entry)]


def apply_run(entry: Path, run: dict) -> None:
    if stale_run(entry, run):
        raise ValueError("The lyrics of this song changed since this run. Run it again first.")
    write_cache_lines(entry, run["lines"], run["stats"].get("shift_applied_ms") or 0, "lab")
    write_json(lab_dir(entry) / "applied.json", {"run_id": run["id"], "label": run.get("label"), "at": time.time()})


def apply_batch(batch: str) -> dict:
    """Puts each song's run from `batch` into the overlay, except runs that timed no
    lines (they would wipe out whatever timing the song has) or whose lyrics changed."""
    applied, skipped = 0, []
    for entry, _, run in batch_runs(batch):
        name = read_json(entry / ".meta", {}).get("track_name", entry.name)
        if not run["stats"].get("aligned"):
            skipped.append({"song": name, "why": "no lines timed"})
        elif stale_run(entry, run):
            skipped.append({"song": name, "why": "lyrics changed since the run"})
        else:
            apply_run(entry, run)
            applied += 1
    return {"applied": applied, "skipped": skipped}


# ---------------------------------------------------------------------------
# Background jobs (one at a time, so models are loaded once and shared)


MAX_LOG_LINES = 500
MAX_FINISHED_JOBS = 200


class Jobs:
    def __init__(self, device: str | None):
        self.device = device
        self.pool: ModelPool | None = None
        self.queue: queue.Queue = queue.Queue()
        self.jobs: dict[str, dict] = {}
        self.lock = threading.Lock()
        self.deleted_batches: set[str] = set()
        threading.Thread(target=self._work, daemon=True).start()

    def submit(self, song: str, cfg: RunConfig, label: str, batch: str | None = None) -> dict:
        job = {"id": secrets.token_hex(4), "song": song, "config": cfg.to_dict(), "label": label,
               "batch": batch, "status": "queued", "log": [], "run_id": None, "error": None, "created": time.time()}
        with self.lock:
            self.jobs[job["id"]] = job
            finished = [j for j in self.jobs.values() if j["status"] in ("done", "error", "cancelled")]
            for old in sorted(finished, key=lambda j: j["created"])[:max(0, len(finished) - MAX_FINISHED_JOBS)]:
                del self.jobs[old["id"]]
        self.queue.put(job["id"])
        return job

    def cancel_queued(self, batch: str | None = None) -> int:
        """Drops every job (of `batch`, if given) that hasn't started yet; a running one
        finishes."""
        n = 0
        with self.lock:
            for j in self.jobs.values():
                if j["status"] == "queued" and (batch is None or j.get("batch") == batch):
                    j["status"] = "cancelled"
                    n += 1
        return n

    def list(self, song: str | None = None) -> list[dict]:
        with self.lock:
            jobs = [dict(j, log=j["log"][-200:]) for j in self.jobs.values() if song is None or j["song"] == song]
        return sorted(jobs, key=lambda j: j["created"])

    def _work(self) -> None:
        while True:
            job = self.jobs.get(self.queue.get())
            if job is None:
                continue
            with self.lock:
                if job["status"] != "queued":
                    continue
                job["status"] = "running"

            def log(msg: str, job=job) -> None:
                print(msg, flush=True)
                job["log"].append(msg)
                if len(job["log"]) > MAX_LOG_LINES:
                    del job["log"][:len(job["log"]) - MAX_LOG_LINES]

            try:
                if self.pool is None:
                    log("Loading models...")
                    self.pool = ModelPool(self.device)
                job["run_id"] = self._run(job, log)
                job["status"] = "done"
            except Exception as e:  # noqa: BLE001 - report to the page and keep serving
                traceback.print_exc()
                job["error"] = str(e) or type(e).__name__
                job["status"] = "error"
                log(f"Failed: {job['error']}")

    def _run(self, job: dict, log) -> str | None:
        entry = entry_path(job["song"])
        audio, offset = audio_source(entry)
        if audio is None:
            raise ValueError("this song has no recording or audio file to align")
        cfg = RunConfig.from_dict(job["config"])
        meta = read_json(entry / ".meta", {})
        result = run_alignment(original_lines(entry), audio, offset, cfg, self.pool,
                               cache_dir=lab_dir(entry),
                               duration_ms=int(float(meta.get("duration_sec", 0)) * 1000) or None,
                               log=log)
        run_id = time.strftime("%Y%m%d-%H%M%S-") + secrets.token_hex(2)
        result.update(id=run_id, label=job["label"] or cfg.summary(), created=time.time(),
                      batch=job.get("batch"))
        if job.get("batch") in self.deleted_batches:
            log("Its batch was deleted meanwhile, so the run isn't kept.")
            return None
        write_json(lab_dir(entry) / "runs" / f"{run_id}.json", result)
        return run_id


# ---------------------------------------------------------------------------
# Waveforms


_peaks_cache: dict[tuple, dict] = {}


def peaks(path: Path, offset_ms: int, bin_ms: int = 10) -> dict:
    """Per-bin peak level (0-255) of an audio file, for drawing its waveform."""
    import soundfile as sf

    st = path.stat()
    key = (str(path), st.st_mtime, st.st_size, bin_ms)
    if key not in _peaks_cache:
        info = sf.info(str(path))
        per_bin = max(1, info.samplerate * bin_ms // 1000)
        out = []
        for block in sf.blocks(str(path), blocksize=per_bin * 1000, dtype="float32", always_2d=True):
            mono = np.abs(block).max(axis=1)
            n = len(mono) // per_bin * per_bin
            if n:
                out.append(mono[:n].reshape(-1, per_bin).max(axis=1))
            if len(mono) > n:
                out.append(np.array([mono[n:].max()]))
        values = np.concatenate(out) if out else np.zeros(0)
        top = float(np.percentile(values, 99.5)) if len(values) else 1.0
        scaled = np.clip(values / max(top, 1e-6), 0, 1) ** 0.6 * 255
        remember(_peaks_cache, key, {"bin_ms": bin_ms, "peaks": scaled.astype(int).tolist()}, 20)
    return {"offset_ms": offset_ms, **_peaks_cache[key]}


def resolve_audio(song: str, source: str) -> tuple[Path, int]:
    """`source` is "mix" (the recording or file) or "vocals:<separator>"."""
    entry = entry_path(song)
    if source == "mix":
        audio, offset = audio_source(entry)
        if audio is None:
            raise LookupError("no audio for this song")
        return audio, offset
    if source.startswith("vocals:"):
        path = vocals_cache_path(lab_dir(entry), source[7:])
        if path.exists():
            return path, 0
    raise LookupError(f"no audio {source!r}")


# ---------------------------------------------------------------------------
# HTTP


class Handler(BaseHTTPRequestHandler):
    jobs: Jobs

    def log_message(self, fmt, *args):  # quieter console
        if not self.path.startswith(("/api/jobs", "/audio")):
            super().log_message(fmt, *args)

    def _send(self, status: int, body: bytes, ctype: str, extra: dict | None = None) -> None:
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def _json(self, data, status: int = 200) -> None:
        self._send(status, json.dumps(data, ensure_ascii=False).encode("utf-8"), "application/json; charset=utf-8")

    def _body(self) -> dict:
        n = int(self.headers.get("Content-Length") or 0)
        return json.loads(self.rfile.read(n) or b"{}") if n else {}

    def _route(self):
        url = urllib.parse.urlsplit(self.path)
        parts = [urllib.parse.unquote(p) for p in url.path.strip("/").split("/") if p]
        return parts, urllib.parse.parse_qs(url.query)

    def _trusted(self) -> bool:
        """Only this page may use the API. Other websites the browser has open can send
        requests to 127.0.0.1 too: a wrong Host header means DNS rebinding, a foreign
        Origin a cross-site request. Changes must also be sent as JSON, which browsers
        only allow cross-site after a CORS preflight this server never answers."""
        port = self.server.server_address[1]
        allowed = {f"127.0.0.1:{port}", f"localhost:{port}"}
        if self.headers.get("Host") not in allowed:
            return False
        origin = self.headers.get("Origin")
        if origin and origin not in {f"http://{h}" for h in allowed}:
            return False
        if self.command in ("POST", "PUT", "DELETE"):
            ctype = (self.headers.get("Content-Type") or "").split(";")[0].strip().lower()
            if ctype != "application/json":
                return False
        return True

    def _guard(self, fn):
        if not self._trusted():
            self._json({"error": "forbidden"}, 403)
            return
        try:
            fn()
        except LookupError as e:
            self._json({"error": str(e)}, 404)
        except (ValueError, json.JSONDecodeError) as e:
            self._json({"error": str(e)}, 400)
        except (BrokenPipeError, ConnectionResetError):
            pass
        except Exception as e:  # noqa: BLE001
            traceback.print_exc()
            self._json({"error": str(e)}, 500)

    def do_GET(self):
        self._guard(self._get)

    do_HEAD = do_GET

    def do_POST(self):
        self._guard(self._post)

    def do_PUT(self):
        self._guard(self._post)

    def do_DELETE(self):
        self._guard(self._delete)

    def _get(self):
        parts, q = self._route()
        if not parts:
            self._send(200, PAGE.read_bytes(), "text/html; charset=utf-8")
        elif parts == ["api", "options"]:
            self._json({
                "separators": SEPARATORS, "aligners": ALIGNERS,
                "defaults": load_defaults(data_dir()).to_dict(), "builtin": RunConfig().to_dict(),
                "cache_root": str(cache_root()),
            })
        elif parts == ["api", "recordings"]:
            self._json(recordings_overview())
        elif parts == ["api", "cache"]:
            self._json(cache_overview())
        elif parts == ["api", "library"]:
            self._json(library())
        elif parts == ["api", "songs"]:
            self._json(list_songs())
        elif len(parts) == 3 and parts[:2] == ["api", "song"]:
            self._json(song_detail(parts[2]))
        elif len(parts) == 4 and parts[:2] == ["api", "song"] and parts[3] == "peaks":
            path, offset = resolve_audio(parts[2], q.get("source", ["mix"])[0])
            self._json(peaks(path, offset))
        elif parts[:2] == ["api", "jobs"]:
            self._json(self.jobs.list(q.get("song", [None])[0]))
        elif len(parts) == 3 and parts[0] == "audio":
            path, _ = resolve_audio(parts[1], parts[2])
            self._serve_file(path)
        else:
            raise LookupError("not found")

    def _post(self):
        parts, _ = self._route()
        body = self._body()
        if parts == ["api", "jobs", "cancel"]:
            self._json({"cancelled": self.jobs.cancel_queued()})
            return
        if parts == ["api", "cache", "clear"]:
            if any(j["status"] in ("queued", "running") for j in self.jobs.list()):
                raise ValueError("Wait for the queued runs to finish (or cancel them) first.")
            self._json(clear_caches())
            return
        if parts == ["api", "recordings", "delete"]:
            if any(j["status"] == "running" for j in self.jobs.list()):
                raise ValueError("A run is using the recordings right now. Cancel the queue and wait for it to finish first.")
            self.jobs.cancel_queued()
            self._json(delete_recordings(bool(body.get("clear_timing"))))
            return
        if parts == ["api", "batch"]:
            cfg = RunConfig.from_dict(body.get("config") or {})
            batch = time.strftime("%Y%m%d-%H%M%S-") + secrets.token_hex(2)
            label = str(body.get("label") or cfg.summary())[:120]
            songs, skipped = [], []
            for s in list_songs():
                if s["source"] is None:  # recording deleted; listed only for review
                    continue
                problem = cached_recording_problem(cache_root() / s["id"]) if s["source"] == "recording" else None
                if problem:
                    skipped.append({"song": f"{s['artist']} - {s['title']}", "problem": problem})
                else:
                    songs.append(s["id"])
            for song in songs:
                self.jobs.submit(song, cfg, label, batch)
            self._json({"batch": batch, "jobs": len(songs), "skipped": skipped})
            return
        if parts == ["api", "batch", "apply"]:
            self._json(apply_batch(str(body.get("batch"))))
            return
        if parts == ["api", "batch", "delete"]:
            self.jobs.cancel_queued(str(body.get("batch")))
            self.jobs.deleted_batches.add(str(body.get("batch")))
            runs = batch_runs(str(body.get("batch")))
            for _, f, _ in runs:
                f.unlink(missing_ok=True)
            self._json({"deleted": len(runs)})
            return
        if parts == ["api", "defaults"]:
            cfg = RunConfig.from_dict(body)
            write_json(defaults_file(data_dir()), cfg.to_dict())
            self._json({"ok": True, "defaults": cfg.to_dict()})
            return
        if len(parts) < 4 or parts[:2] != ["api", "song"]:
            raise LookupError("not found")
        song, action = parts[2], parts[3]
        entry = entry_path(song)
        lab = lab_dir(entry)
        if action == "runs":
            if audio_source(entry)[0] is None:
                raise ValueError("This song has no recording to align. Play it in Spotify with recording on first.")
            configs = body.get("configs") or [body.get("config") or {}]
            labels = body.get("labels") or []
            jobs = []
            for i, c in enumerate(configs):
                cfg = RunConfig.from_dict(c)
                jobs.append(self.jobs.submit(song, cfg, labels[i] if i < len(labels) else ""))
            self._json({"jobs": jobs})
        elif action == "reference":
            if (read_json(lab / "reference.json") or {}).get("locked"):
                raise ValueError("This song's reference is hand-annotated and can't be changed.")
            words = body.get("words", {})
            lines = original_lines(entry)
            texts = {k: lines[int(k)]["text"] for k in words if k.isdigit() and int(k) < len(lines)}
            write_json(lab / "reference.json", {"words": words, "texts": texts, "frame": "recording",
                                                "updated": time.time()})
            self._json({"ok": True})
        elif action == "label" and len(parts) == 5:
            path = lab / "runs" / f"{safe_id(parts[4])}.json"
            run = read_json(path)
            if not run:
                raise LookupError("no such run")
            run["label"] = str(body.get("label", ""))[:120]
            write_json(path, run)
            self._json({"ok": True})
        elif action == "apply":
            run_id = body.get("run_id")
            if run_id:
                run = read_json(lab / "runs" / f"{safe_id(run_id)}.json")
                if not run:
                    raise LookupError("no such run")
                apply_run(entry, run)
            else:
                # Back to plain line timing.
                write_cache_lines(entry, original_lines(entry))
                (lab / "applied.json").unlink(missing_ok=True)
            self._json({"ok": True})
        else:
            raise LookupError("not found")

    def _delete(self):
        parts, _ = self._route()
        if len(parts) == 5 and parts[:2] == ["api", "song"] and parts[3] == "runs":
            path = lab_dir(entry_path(parts[2])) / "runs" / f"{safe_id(parts[4])}.json"
            path.unlink(missing_ok=True)
            self._json({"ok": True})
        else:
            raise LookupError("not found")

    def _serve_file(self, path: Path) -> None:
        size = path.stat().st_size
        ctype = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
        if path.suffix.lower() == ".flac":
            ctype = "audio/flac"
        rng = self.headers.get("Range")
        start, end, status = 0, size - 1, HTTPStatus.OK
        m = re.match(r"bytes=(\d*)-(\d*)$", rng or "")
        if m and (m.group(1) or m.group(2)):
            if m.group(1):
                start = int(m.group(1))
                end = int(m.group(2)) if m.group(2) else size - 1
            else:
                start = max(0, size - int(m.group(2)))
            end = min(end, size - 1)
            if start > end:
                self._send(HTTPStatus.REQUESTED_RANGE_NOT_SATISFIABLE, b"", ctype,
                           {"Content-Range": f"bytes */{size}"})
                return
            status = HTTPStatus.PARTIAL_CONTENT
        length = end - start + 1
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(length))
        self.send_header("Cache-Control", "no-store")
        if status == HTTPStatus.PARTIAL_CONTENT:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.end_headers()
        if self.command == "HEAD":
            return
        with path.open("rb") as f:
            f.seek(start)
            remaining = length
            while remaining > 0:
                chunk = f.read(min(1 << 16, remaining))
                if not chunk:
                    break
                self.wfile.write(chunk)
                remaining -= len(chunk)


def safe_id(run_id: str) -> str:
    if not re.fullmatch(r"[\w-]+", run_id or ""):
        raise LookupError("bad run id")
    return run_id


def register_files(files: list[Path]) -> None:
    root = cache_root()
    for f in files:
        if not f.exists():
            raise SystemExit(f"{f} doesn't exist")
        entry = find_cache_entry(root, read_tags(f))
        write_json(lab_dir(entry) / "source.json", {"audio": str(f.resolve())})
        print(f"{f.name} -> {entry.name}")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("audio", type=Path, nargs="*",
                   help="Audio files to add to the lab (their songs must be in the overlay's cache)")
    p.add_argument("--port", type=int, default=8765)
    p.add_argument("--device", default=None, help="cpu or cuda (default: cuda if available)")
    p.add_argument("--no-browser", action="store_true", help="Don't open the page in a browser")
    p.add_argument("--fake-models", action="store_true", help=argparse.SUPPRESS)  # for developing the page
    args = p.parse_args()
    # Job logs are printed too; lyrics contain characters (♪, accents) that a Windows
    # console or redirected output may not be able to encode, which must not fail a run.
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(errors="replace")
        except (AttributeError, ValueError):
            pass
    if args.audio:
        register_files(args.audio)

    if args.fake_models:
        from fakes import FakePool

        global ModelPool
        ModelPool = lambda device=None: FakePool()  # noqa: E731
    Handler.jobs = Jobs(args.device)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    url = f"http://127.0.0.1:{args.port}/"
    print(f"Sync lab running at {url}  (lyrics cache: {cache_root()})\nPress Ctrl+C to stop.")
    if not args.no_browser:
        threading.Timer(0.5, webbrowser.open, (url,)).start()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("Stopped.")


if __name__ == "__main__":
    main()
