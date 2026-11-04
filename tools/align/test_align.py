"""Tests for the parts that don't need the models: run with `uv run pytest`."""

import numpy as np

from align import build_segments, find_cache_entry, norm, Tags, to_enhanced_lrc
from ctc import forced_align
from text import MMS_LABELS, split_units, syllabify_line, tokens_for


def identity(s):
    return s


def test_syllables_concatenate_back_to_line():
    for line, lang in [
        ("Never gonna give you up, never gonna let you down", "en"),
        ("Ik hou van jou (ooh)", "nl"),
        ("「君の名は」 Sparkle", "ja"),
        ("사랑해 baby", "ko"),
        ("  leading and trailing  ", "en"),
    ]:
        syls = syllabify_line(line, lang, identity)
        assert "".join(s.text for s in syls) == line


def test_english_hyphenation():
    syls = syllabify_line("understand feeling", "en", identity)
    assert [s.text for s in syls] == ["un", "der", "stand ", "feel", "ing"]
    assert [s.word_index for s in syls] == [0, 0, 0, 1, 1]


def test_cjk_units():
    assert split_units("君の名は") == ["君", "の", "名", "は"]
    assert split_units("ちょっと") == ["ちょっ", "と"]
    assert split_units("「君」 a") == ["「君」 ", "a"]
    assert split_units("사랑해") == ["사", "랑", "해"]


def test_unknown_language_keeps_words():
    syls = syllabify_line("hello world", None, identity)
    assert [s.text for s in syls] == ["hello ", "world"]


def test_tokens_skip_unknown_chars():
    assert [MMS_LABELS[t] for t in tokens_for("Don’t 2!")] == list("don't")


def fake_log_probs(frames):
    """frames: list of label chars, one per frame; '-' is blank."""
    lp = np.full((len(frames), len(MMS_LABELS)), -10.0)
    for t, c in enumerate(frames):
        lp[t, MMS_LABELS.index(c)] = -0.01
    return lp


def test_forced_align_finds_token_spans():
    lp = fake_log_probs(list("--aa--b--cc-"))
    spans, conf = forced_align(lp, [MMS_LABELS.index(c) for c in "abc"])
    assert spans == [(2, 3), (6, 6), (9, 10)]
    assert conf > -0.1


def test_forced_align_repeated_tokens_need_a_blank():
    lp = fake_log_probs(list("-l-l-"))
    spans, _ = forced_align(lp, [MMS_LABELS.index("l")] * 2)
    assert spans == [(1, 1), (3, 3)]
    assert forced_align(fake_log_probs(list("l")), [MMS_LABELS.index("l")] * 2) is None


def test_build_segments_fills_gaps_and_joins_word_syllables():
    syls = syllabify_line("nev er 2 go", None, identity)
    times = [(100, 200), (300, 400), None, (600, 700)]
    segs = build_segments(syls, times, 0)
    assert [(s["start_ms"], s["end_ms"]) for s in segs] == [(100, 200), (300, 400), (400, 600), (600, 700)]

    syls = syllabify_line("under", "en", identity)  # un-der, same word
    segs = build_segments(syls, [(100, 150), (300, 400)], 0)
    assert segs[0]["end_ms"] == 300


def test_enhanced_lrc_output():
    lines = [
        {"time_ms": 1000, "text": "ab cd", "segments": [
            {"start_ms": 1000, "end_ms": 1500, "text": "ab "},
            {"start_ms": 1500, "end_ms": 2000, "text": "cd"}]},
        {"time_ms": 3000, "text": "plain"},
    ]
    assert to_enhanced_lrc(lines) == "[00:01.00]<00:01.00>ab <00:01.50>cd<00:02.00>\n[00:03.00]plain\n"


def test_find_cache_entry(tmp_path):
    import json
    for name, meta in {
        "a": {"track_name": "Never Gonna Give You Up", "artist_name": "Rick Astley", "duration_sec": 213.5},
        "b": {"track_name": "Never Gonna Give You Up", "artist_name": "Rick Astley", "duration_sec": 250.0},
        "c": {"track_name": "Other", "artist_name": "Rick Astley", "duration_sec": 213.5},
    }.items():
        (tmp_path / name).mkdir()
        (tmp_path / name / ".meta").write_text(json.dumps(meta))
    found = find_cache_entry(tmp_path, Tags("Never Gonna Give You Up (2022 Remaster)", "Rick Astley", 213.0))
    assert found.name == "a"
    assert norm("Hello! - Remastered 2011") == norm("hello")


def test_vowel_group_fallback_for_short_words():
    syls = syllabify_line("gonna around make", "en", identity)
    assert [s.text for s in syls] == ["gon", "na ", "a", "round ", "make"]


def test_pending_entries(tmp_path):
    import json
    from align import pending_entries

    def entry(name, lines, recording=True):
        (tmp_path / name).mkdir()
        (tmp_path / name / "lyrics.lrc").write_text(json.dumps({"synced_lyrics": lines, "plain_lyrics": []}))
        if recording:
            (tmp_path / name / "recording.wav").write_bytes(b"")

    entry("todo", [{"time_ms": 0, "text": "a"}])
    entry("done", [{"time_ms": 0, "text": "a", "segments": [{"start_ms": 0, "end_ms": 1, "text": "a"}]}])
    entry("no-recording", [{"time_ms": 0, "text": "a"}], recording=False)
    entry("unsynced", [])
    entry("tried", [{"time_ms": 0, "text": "a"}])
    (tmp_path / "tried" / ".align-attempted").touch()
    assert [e.name for e in pending_entries(tmp_path)] == ["todo"]
    assert [e.name for e in pending_entries(tmp_path, redo=True)] == ["done", "todo", "tried", "unsynced"]


def test_latin_words_in_cjk_song_still_split():
    syls = syllabify_line("君の baby", "ja", identity)
    assert [s.text for s in syls] == ["君", "の ", "ba", "by"]


def test_recording_offset(tmp_path):
    import torch
    from align import recording_offset_ms, shift_to_song_time

    rec = tmp_path / "recording.wav"
    rec.write_bytes(b"")
    assert recording_offset_ms(rec) == 0
    (tmp_path / "recording.json").write_text('{"offset_ms": -1500}')
    assert recording_offset_ms(rec) == -1500
    assert recording_offset_ms(tmp_path / "song.flac") == 0

    wav = torch.ones(2, 1000)
    assert shift_to_song_time(wav, 1000, -250).shape == (2, 750)
    padded = shift_to_song_time(wav, 1000, 100)
    assert padded.shape == (2, 1100) and padded[:, :100].abs().sum() == 0
