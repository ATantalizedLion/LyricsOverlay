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
                   read_tags, recording_offset_ms, recording_problem, write_cache_lines)
from pipeline import (ALIGNERS, SEPARATORS, ModelPool, RunConfig, defaults_file, load_defaults,
                      run_alignment, vocals_cache_path)
from text import split_units

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
        if audio is None:
            continue
        try:
            lines = original_lines(entry)
        except (OSError, json.JSONDecodeError, KeyError):
            continue
        if not lines:
            continue
        meta = read_json(meta_file, {})
        lab = lab_dir(entry)
        runs = list((lab / "runs").glob("*.json")) if (lab / "runs").exists() else []
        current = read_json(entry / "lyrics.lrc", {}).get("synced_lyrics") or []
        songs.append({
            "id": entry.name,
            "title": meta.get("track_name", entry.name),
            "artist": meta.get("artist_name", ""),
            "duration_s": meta.get("duration_sec"),
            "runs": len(runs),
            "has_reference": (lab / "reference.json").exists(),
            "aligned_in_overlay": any(l.get("segments") for l in current),
            "source": "recording" if audio.name == RECORDING_FILE else "file",
        })
    songs.sort(key=lambda s: (s["artist"].casefold(), s["title"].casefold()))
    return songs


def run_summary(run: dict) -> dict:
    return {k: v for k, v in run.items() if k != "lines"}


def song_detail(name: str) -> dict:
    entry = entry_path(name)
    lab = lab_dir(entry)
    meta = read_json(entry / ".meta", {})
    audio, offset = audio_source(entry)
    lines = original_lines(entry)
    for line in lines:
        units, pos = [], 0
        for u in split_units(line["text"]):
            if u.strip():
                units.append({"offset": pos, "text": u})
            pos += len(u)
        line["units"] = units
    runs = []
    for f in sorted((lab / "runs").glob("*.json")) if (lab / "runs").exists() else []:
        run = read_json(f)
        if run:
            runs.append(run)
    runs.sort(key=lambda r: r.get("created", 0))
    problem = None
    if audio is not None and audio.name == RECORDING_FILE:
        try:
            problem = recording_problem(entry)
        except Exception as e:  # noqa: BLE001
            problem = f"couldn't check the recording: {e}"
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
        "reference": read_json(lab / "reference.json"),
        "applied": read_json(lab / "applied.json"),
        "vocals": vocals,
    }


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
        runs = []
        for f in sorted((lab / "runs").glob("*.json")) if (lab / "runs").exists() else []:
            run = read_json(f)
            if run:
                runs.append(run_summary(run))
        song["run_list"] = sorted(runs, key=lambda r: r.get("created", 0))
        song["applied"] = read_json(lab / "applied.json")
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


def apply_batch(batch: str) -> int:
    applied = 0
    for entry, _, run in batch_runs(batch):
        write_cache_lines(entry, run["lines"])
        write_json(lab_dir(entry) / "applied.json",
                   {"run_id": run["id"], "label": run.get("label"), "at": time.time()})
        applied += 1
    return applied


# ---------------------------------------------------------------------------
# Background jobs (one at a time, so models are loaded once and shared)


class Jobs:
    def __init__(self, device: str | None):
        self.device = device
        self.pool: ModelPool | None = None
        self.queue: queue.Queue = queue.Queue()
        self.jobs: dict[str, dict] = {}
        self.lock = threading.Lock()
        threading.Thread(target=self._work, daemon=True).start()

    def submit(self, song: str, cfg: RunConfig, label: str, batch: str | None = None) -> dict:
        job = {"id": secrets.token_hex(4), "song": song, "config": cfg.to_dict(), "label": label,
               "batch": batch, "status": "queued", "log": [], "run_id": None, "error": None, "created": time.time()}
        with self.lock:
            self.jobs[job["id"]] = job
        self.queue.put(job["id"])
        return job

    def list(self, song: str | None = None) -> list[dict]:
        with self.lock:
            jobs = [dict(j, log=j["log"][-200:]) for j in self.jobs.values() if song is None or j["song"] == song]
        return sorted(jobs, key=lambda j: j["created"])

    def _work(self) -> None:
        while True:
            job = self.jobs[self.queue.get()]
            job["status"] = "running"

            def log(msg: str, job=job) -> None:
                print(msg, flush=True)
                job["log"].append(msg)

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

    def _run(self, job: dict, log) -> str:
        entry = entry_path(job["song"])
        audio, offset = audio_source(entry)
        cfg = RunConfig.from_dict(job["config"])
        meta = read_json(entry / ".meta", {})
        result = run_alignment(original_lines(entry), audio, offset, cfg, self.pool,
                               cache_dir=lab_dir(entry),
                               duration_ms=int(float(meta.get("duration_sec", 0)) * 1000) or None,
                               log=log)
        run_id = time.strftime("%Y%m%d-%H%M%S-") + secrets.token_hex(2)
        result.update(id=run_id, label=job["label"] or cfg.summary(), created=time.time(),
                      batch=job.get("batch"))
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
        _peaks_cache[key] = {"bin_ms": bin_ms, "peaks": scaled.astype(int).tolist()}
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

    def _guard(self, fn):
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
        if parts == ["api", "recordings", "delete"]:
            self._json(delete_recordings(bool(body.get("clear_timing"))))
            return
        if parts == ["api", "batch"]:
            cfg = RunConfig.from_dict(body.get("config") or {})
            batch = time.strftime("%Y%m%d-%H%M%S-") + secrets.token_hex(2)
            label = str(body.get("label") or cfg.summary())[:120]
            songs = [s["id"] for s in list_songs()]
            jobs = [self.jobs.submit(song, cfg, label, batch) for song in songs]
            self._json({"batch": batch, "jobs": len(jobs)})
            return
        if parts == ["api", "batch", "apply"]:
            self._json({"applied": apply_batch(str(body.get("batch")))})
            return
        if parts == ["api", "batch", "delete"]:
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
            configs = body.get("configs") or [body.get("config") or {}]
            labels = body.get("labels") or []
            jobs = []
            for i, c in enumerate(configs):
                cfg = RunConfig.from_dict(c)
                jobs.append(self.jobs.submit(song, cfg, labels[i] if i < len(labels) else ""))
            self._json({"jobs": jobs})
        elif action == "reference":
            write_json(lab / "reference.json", {"words": body.get("words", {}), "updated": time.time()})
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
                write_cache_lines(entry, run["lines"])
                write_json(lab / "applied.json", {"run_id": run_id, "label": run.get("label"), "at": time.time()})
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
