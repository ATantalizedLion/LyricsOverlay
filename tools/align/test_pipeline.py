"""Pipeline and lab tests with stand-in models: run with `uv run pytest`."""

import json
import threading
import time
import urllib.request

import numpy as np
import pytest
import soundfile as sf

from ctc import forced_align_detailed
from fakes import FakePool, encode, letter_start_ms
from pipeline import RunConfig, run_alignment, to_cache_lines
from text import MMS_LABELS

EVENTS = [(1000, "hello world"), (4000, "never again"), (8000, "ok")]
LINES = [{"time_ms": t, "text": text} for t, text in EVENTS]
DURATION = 11_000


def run(cfg: RunConfig, lines=LINES, events=EVENTS, tmp_path=None, pool=None):
    audio = encode(events, DURATION)
    pool = pool or FakePool({"song.wav": audio})
    if tmp_path is not None:
        path = tmp_path / "song.wav"
        if not path.exists():
            sf.write(path, audio, 16_000, subtype="FLOAT")
        pool.audio_by_path[str(path)] = audio
    else:
        path = None
    from pathlib import Path

    return run_alignment(lines, path or Path("song.wav"), 0, cfg, pool,
                         cache_dir=tmp_path, log=lambda *_: None), pool


@pytest.fixture(autouse=True)
def no_stat(monkeypatch):
    """The fake audio for most tests isn't a real file."""
    import pipeline

    real = pipeline._source_stamp
    monkeypatch.setattr(pipeline, "_source_stamp",
                        lambda a, o: real(a, o) if a.exists() else {"source": str(a), "offset_ms": o})


def first_starts(result):
    return [l["segments"][0]["start_ms"] if l["segments"] else None for l in result["lines"]]


def test_line_mode_puts_words_where_they_are_sung():
    result, _ = run(RunConfig(unit="word"))
    assert [l["status"] for l in result["lines"]] == ["ok", "ok", "ok"]
    assert first_starts(result) == [1000, 4000, 8000]
    second_word = result["lines"][0]["segments"][1]
    assert second_word["text"] == "world"
    assert second_word["start_ms"] == letter_start_ms(EVENTS, 5)
    assert "".join(s["text"] for s in result["lines"][0]["segments"]) == "hello world"


def test_offset_and_confidence_cutoff():
    result, _ = run(RunConfig(unit="word", offset_ms=-120))
    assert first_starts(result) == [880, 3880, 7880]
    result, _ = run(RunConfig(unit="word", min_confidence=0.0))
    assert all(l["status"] == "low" and not l["segments"] for l in result["lines"])
    assert result["stats"]["aligned"] == 0 and result["stats"]["low"] == 3


def test_song_mode_copes_with_wrong_line_times():
    # The LRC says the second line starts 1.5 s after it's actually sung, so its
    # line-mode window misses the start of it.
    wrong = [dict(LINES[0]), {"time_ms": 5500, "text": "never again"}, dict(LINES[2])]
    line_mode, _ = run(RunConfig(unit="word"), lines=wrong)
    song_mode, _ = run(RunConfig(unit="word", mode="song", anchor_slack_ms=0), lines=wrong)
    assert first_starts(song_mode) == [1000, 4000, 8000]
    assert first_starts(line_mode)[1] != 4000
    retimed, _ = run(RunConfig(unit="word", mode="song", anchor_slack_ms=0, retime_lines=True), lines=wrong)
    assert [l["time_ms"] for l in retimed["lines"]] == [1000, 4000, 8000]
    assert [l["orig_time_ms"] for l in retimed["lines"]] == [1000, 5500, 8000]


def test_even_baseline_needs_no_audio():
    result = run_alignment(LINES, None, 0, RunConfig(aligner="even"), FakePool(), duration_ms=DURATION,
                           log=lambda *_: None)
    segs = result["lines"][0]["segments"]
    assert segs[0]["start_ms"] == 1000
    assert all(a["end_ms"] <= b["start_ms"] + 1 for a, b in zip(segs, segs[1:]))
    assert result["stats"]["aligned"] == 3
    with pytest.raises(ValueError):
        run_alignment(LINES, None, 0, RunConfig(), FakePool(), log=lambda *_: None)


def test_vocals_and_emissions_are_cached(tmp_path):
    pool = FakePool()
    run(RunConfig(), tmp_path=tmp_path, pool=pool)
    run(RunConfig(offset_ms=50, mode="song"), tmp_path=tmp_path, pool=pool)
    assert pool.separations == 1
    run(RunConfig(separator="none"), tmp_path=tmp_path, pool=pool)
    assert pool.separations == 2


def test_run_config_validation_and_coercion():
    cfg = RunConfig.from_dict({"offset_ms": "40", "star": "false", "min_confidence": "-3", "unknown": 1})
    assert cfg.offset_ms == 40 and cfg.star is False and cfg.min_confidence == -3.0
    with pytest.raises(ValueError):
        RunConfig.from_dict({"aligner": "nope"})
    assert RunConfig.from_dict(cfg.to_dict()) == cfg


def test_frame_ranges_anchor_tokens():
    a = MMS_LABELS.index("a")
    lp = np.full((20, len(MMS_LABELS)), -5.0)
    lp[:, 0] = -0.1
    lp[3, a] = lp[15, a] = -0.01  # two equally good places for one "a"
    spans, _ = forced_align_detailed(lp, [a], frame_ranges=[(10, 20)])
    assert spans == [(15, 15)]
    spans, _ = forced_align_detailed(lp, [a], frame_ranges=[(0, 8)])
    assert spans == [(3, 3)]


def test_cache_lines_format():
    result, _ = run(RunConfig(unit="word"))
    lines = to_cache_lines(result["lines"])
    assert set(lines[0]) == {"time_ms", "text", "segments"}
    assert set(lines[0]["segments"][0]) == {"start_ms", "end_ms", "text"}


# ---------------------------------------------------------------------------
# Lab server


@pytest.fixture
def lab_server(tmp_path, monkeypatch):
    monkeypatch.setenv("APPDATA", str(tmp_path))
    entry = tmp_path / "LyricsOverlay" / "cache" / "Test-Song-11.000"
    entry.mkdir(parents=True)
    (entry / ".meta").write_text(json.dumps({"track_name": "Song", "artist_name": "Test", "duration_sec": 11.0}))
    (entry / "lyrics.lrc").write_text(json.dumps({"synced_lyrics": LINES, "plain_lyrics": []}))
    sf.write(entry / "recording.wav", encode(EVENTS, DURATION), 16_000, subtype="FLOAT")

    import lab
    from http.server import ThreadingHTTPServer

    monkeypatch.setattr(lab, "ModelPool", lambda device=None: FakePool())
    lab.Handler.jobs = lab.Jobs(None)
    server = ThreadingHTTPServer(("127.0.0.1", 0), lab.Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{server.server_address[1]}", entry
    server.shutdown()


def call(url, method="GET", body=None, headers=None):
    req = urllib.request.Request(url, method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", **(headers or {})})
    with urllib.request.urlopen(req) as res:
        data = res.read()
        return res.status, dict(res.headers), data


def test_lab_end_to_end(lab_server):
    base, entry = lab_server
    _, _, body = call(base + "/api/songs")
    songs = json.loads(body)
    assert [s["id"] for s in songs] == [entry.name]
    sid = urllib.request.quote(entry.name)

    _, _, body = call(f"{base}/api/song/{sid}/runs", "POST",
                      {"configs": [{"unit": "word"}, {"unit": "word", "offset_ms": 100}], "labels": ["a", "b"]})
    assert len(json.loads(body)["jobs"]) == 2
    for _ in range(100):
        state = json.loads(call(f"{base}/api/jobs?song={sid}")[2])
        if all(j["status"] in ("done", "error") for j in state):
            break
        time.sleep(0.05)
    assert [j["status"] for j in state] == ["done", "done"], state
    song = json.loads(call(f"{base}/api/song/{sid}")[2])
    assert [r["label"] for r in song["runs"]] == ["a", "b"]
    assert song["vocals"] == ["htdemucs"]
    assert [u["text"] for u in song["lines"][0]["units"]] == ["hello ", "world"]

    run_id = song["runs"][0]["id"]
    call(f"{base}/api/song/{sid}/apply", "POST", {"run_id": run_id})
    cached = json.loads((entry / "lyrics.lrc").read_text())
    assert cached["synced_lyrics"][0]["segments"][0]["start_ms"] == 1000
    assert (entry / "lyrics.lrc.bak").exists()
    # The lab still works from the original line timing after applying.
    song = json.loads(call(f"{base}/api/song/{sid}")[2])
    assert song["applied"]["run_id"] == run_id
    assert "segments" not in song["lines"][0]
    call(f"{base}/api/song/{sid}/apply", "POST", {})
    assert "segments" not in json.loads((entry / "lyrics.lrc").read_text())["synced_lyrics"][0]

    call(f"{base}/api/song/{sid}/reference", "PUT", {"words": {"0": [1000, None]}})
    assert json.loads(call(f"{base}/api/song/{sid}")[2])["reference"]["words"] == {"0": [1000, None]}

    status, headers, data = call(f"{base}/audio/{sid}/mix", headers={"Range": "bytes=10-19"})
    assert status == 206 and len(data) == 10 and headers["Content-Range"].endswith(f"/{(entry / 'recording.wav').stat().st_size}")
    peaks = json.loads(call(f"{base}/api/song/{sid}/peaks?source=vocals:htdemucs")[2])
    assert len(peaks["peaks"]) == DURATION // 10

    call(f"{base}/api/song/{sid}/runs/{run_id}", "DELETE")
    assert len(json.loads(call(f"{base}/api/song/{sid}")[2])["runs"]) == 1

    call(f"{base}/api/defaults", "POST", {"offset_ms": -60})
    from align import data_dir
    from pipeline import load_defaults
    assert load_defaults(data_dir()).offset_ms == -60

    with pytest.raises(urllib.error.HTTPError) as e:
        call(f"{base}/api/song/..%2F..%2Fetc")
    assert e.value.code == 404


def test_chunked_emissions_line_up_with_the_audio():
    """CtcAligner stitches per-chunk model output onto one 20 ms grid; check with a model
    that just reads the label off each frame's first sample."""
    import torch

    from pipeline import CtcAligner

    class ReadLabels(torch.nn.Module):
        def forward(self, x):
            n = (x.shape[-1] - 400) // 320 + 1
            labels = torch.round(x[0, : n * 320 : 320] * 64).long().clamp(0, len(MMS_LABELS) - 1)
            return torch.nn.functional.one_hot(labels, len(MMS_LABELS)).float()[None] * 20, None

    al = CtcAligner.__new__(CtcAligner)
    al.torch, al.device, al.model, al.native_star, al.blank = torch, "cpu", ReadLabels(), True, 0
    rng = np.random.default_rng(1)
    labels = rng.integers(0, len(MMS_LABELS), size=95 * 50)  # 95 s: several chunks
    audio = np.repeat((labels / 64.0).astype(np.float32), 320)
    em = al.emissions(audio, chunk_s=30, context_s=2)
    assert em.shape == (len(labels), len(MMS_LABELS))
    assert (em.argmax(axis=1) == labels).mean() > 0.99

    al.native_star = False  # wav2vec2: star column appended as log(1 - p(blank))
    em = al.emissions(audio[: 320 * 100])
    assert em.shape[1] == len(MMS_LABELS) + 1
    blank_frames = labels[:100] == 0
    assert (em[blank_frames, -1] < -5).all() and (em[~blank_frames, -1] > -0.01).all()


def test_lab_deletes_all_recordings(lab_server):
    import os

    base, entry = lab_server
    root = entry.parent
    sid = urllib.request.quote(entry.name)
    call(f"{base}/api/song/{sid}/runs", "POST", {"configs": [{"unit": "word"}]})
    for _ in range(100):
        state = json.loads(call(f"{base}/api/jobs?song={sid}")[2])
        if state and all(j["status"] in ("done", "error") for j in state):
            break
        time.sleep(0.05)
    run_id = json.loads(call(f"{base}/api/song/{sid}")[2])["runs"][0]["id"]
    call(f"{base}/api/song/{sid}/apply", "POST", {"run_id": run_id})
    (entry / "recording.json").write_text('{"offset_ms": 0}')
    other = root / "Other-Song-100.000"
    other.mkdir()
    (other / ".align-attempted").touch()
    old_part, new_part = root / ".recording-1.wav.part", root / ".recording-2.wav.part"
    old_part.write_bytes(b"x")
    new_part.write_bytes(b"x")
    os.utime(old_part, (time.time() - 3600, time.time() - 3600))

    overview = json.loads(call(f"{base}/api/recordings")[2])
    assert {k: v for k, v in overview.items() if k != "mb"} == {"recordings": 1, "leftovers": 1, "aligned": 1, "markers": 1}

    result = json.loads(call(f"{base}/api/recordings/delete", "POST", {"clear_timing": True})[2])
    assert result == {"recordings": 1, "leftovers": 1, "cleared": 1, "markers": 1}
    assert not (entry / "recording.wav").exists() and not (entry / "recording.json").exists()
    assert not (other / ".align-attempted").exists()
    assert not old_part.exists() and new_part.exists()  # might still be recording
    assert "segments" not in json.loads((entry / "lyrics.lrc").read_text())["synced_lyrics"][0]
    from align import lab_dir
    assert not (lab_dir(entry) / "vocals").exists()
    assert (lab_dir(entry) / "runs" / f"{run_id}.json").exists()
    assert json.loads(call(f"{base}/api/songs")[2]) == []


def test_lab_batches(lab_server, tmp_path):
    base, entry = lab_server
    second = entry.parent / "Test-Other-11.000"
    second.mkdir()
    for name in (".meta", "lyrics.lrc", "recording.wav"):
        (second / name).write_bytes((entry / name).read_bytes())
    _, _, body = call(f"{base}/api/batch", "POST", {"config": {"unit": "word", "offset_ms": 400}, "label": "late"})
    batch = json.loads(body)["batch"]
    for _ in range(200):
        state = json.loads(call(f"{base}/api/jobs")[2])
        if state and all(j["status"] in ("done", "error") for j in state):
            break
        time.sleep(0.05)
    assert [j["status"] for j in state] == ["done", "done"], state
    lib = json.loads(call(f"{base}/api/library")[2])
    runs = [r for s in lib["songs"] for r in s["run_list"]]
    assert len(runs) == 2 and {r["batch"] for r in runs} == {batch}
    assert all(r["stats"]["lrc_shift_ms"] == 400 and r["stats"]["lrc_shift_spread_ms"] == 0 for r in runs)
    assert "lines" not in runs[0]

    assert json.loads(call(f"{base}/api/batch/apply", "POST", {"batch": batch})[2]) == {"applied": 2}
    for e in (entry, second):
        assert json.loads((e / "lyrics.lrc").read_text())["synced_lyrics"][0]["segments"][0]["start_ms"] == 1400
    assert json.loads(call(f"{base}/api/batch/delete", "POST", {"batch": batch})[2]) == {"deleted": 2}
    assert not any(s["run_list"] for s in json.loads(call(f"{base}/api/library")[2])["songs"])


def test_auto_shift_matches_singing_to_the_lrc():
    # Every line is sung 700 ms after its LRC time, as when a recording's start was
    # misjudged.
    texts = ["hello world", "never again", "dig down", "find a way", "ok then", "go home"]
    lrc = [{"time_ms": 1000 + 1500 * i, "text": t} for i, t in enumerate(texts)]
    sung = [(l["time_ms"] + 700, l["text"]) for l in lrc]
    shifted, _ = run(RunConfig(unit="word"), lines=lrc, events=sung)
    assert shifted["stats"]["shift_applied_ms"] == -700
    assert first_starts(shifted) == [l["time_ms"] for l in lrc]
    assert shifted["stats"]["lrc_shift_ms"] == 0
    kept, _ = run(RunConfig(unit="word", auto_shift=False), lines=lrc, events=sung)
    assert kept["stats"]["shift_applied_ms"] == 0
    assert kept["stats"]["lrc_shift_ms"] == 700


def test_auto_shift_leaves_small_or_inconsistent_shifts_alone():
    texts = ["hello world", "never again", "dig down", "find a way", "ok then", "go home"]
    lrc = [{"time_ms": 1000 + 1500 * i, "text": t} for i, t in enumerate(texts)]
    jitter = [80, -60, 900, -700, 40, 0]
    sung = [(l["time_ms"] + j, l["text"]) for l, j in zip(lrc, jitter)]
    result, _ = run(RunConfig(unit="word"), lines=lrc, events=sung)
    assert result["stats"]["shift_applied_ms"] == 0
