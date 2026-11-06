"""Stand-in models for tests and for trying the lab without downloading anything.

The fake "audio" encodes which letter is sung in each 20 ms frame as the sample value, and
the fake aligner reads it back as near-certain emissions. So alignments come out exactly
where the letters were put, which makes the pipeline's bookkeeping testable."""

from __future__ import annotations

import numpy as np

from pipeline import ALIGN_SR, FRAME_MS, FRAME_SAMPLES
from text import BLANK, MMS_LABELS, STAR, tokens_for


def encode(events: list[tuple[int, str]], duration_ms: int, frames_per_char: int = 2) -> np.ndarray:
    """Audio where each (start_ms, text) event's letters are "sung" from start_ms on, each
    for `frames_per_char` frames followed by one silent frame."""
    n_frames = duration_ms // FRAME_MS
    labels = np.zeros(n_frames, dtype=np.int64)
    for start, text in events:
        f = start // FRAME_MS
        for tok in tokens_for(text):
            labels[f:f + frames_per_char] = tok
            f += frames_per_char + 1
    return np.repeat((labels / 64.0).astype(np.float32), FRAME_SAMPLES)


def letter_start_ms(events, index: int, frames_per_char: int = 2) -> int:
    """Where the `index`-th letter of the events (counting across all of them) starts."""
    for start, text in events:
        n = len(tokens_for(text))
        if index < n:
            return start + index * (frames_per_char + 1) * FRAME_MS
        index -= n
    raise IndexError(index)


class FakeAligner:
    blank = BLANK
    star = STAR
    word_sep = None

    def __init__(self, name: str = "mms"):
        self.name = name

    def tokenize(self, romanised: str) -> list[int]:
        return tokens_for(romanised)

    def emissions(self, vocals: np.ndarray) -> np.ndarray:
        n = len(vocals) // FRAME_SAMPLES
        frames = vocals[: n * FRAME_SAMPLES].reshape(n, FRAME_SAMPLES)[:, 0]
        labels = np.clip(np.round(frames * 64).astype(np.int64), 0, len(MMS_LABELS) - 1)
        lp = np.full((n, len(MMS_LABELS)), -12.0, dtype=np.float32)
        lp[np.arange(n), labels] = -0.05
        lp[:, STAR] = -6.0
        return lp


class FakePool:
    """Drop-in for pipeline.ModelPool."""

    device = "cpu"

    def __init__(self, audio_by_path: dict | None = None):
        self.audio_by_path = audio_by_path or {}
        self.separations = 0

    def romanise(self, text: str) -> str:
        return text

    def aligner(self, name: str) -> FakeAligner:
        return FakeAligner(name)

    def separate(self, audio, offset_ms, separator, log=print) -> np.ndarray:
        self.separations += 1
        if str(audio) in self.audio_by_path:
            return self.audio_by_path[str(audio)]
        import soundfile as sf

        data, sr = sf.read(str(audio), dtype="float32", always_2d=True)
        assert sr == ALIGN_SR
        return data[:, 0]
