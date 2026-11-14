"""CTC forced alignment (Viterbi) over a log-probability matrix.

Written out in numpy rather than using torchaudio.functional.forced_align, which is
deprecated in recent torchaudio releases."""

from __future__ import annotations

import numpy as np

NEG = -1e30


def forced_align(log_probs: np.ndarray, targets: list[int], blank: int = 0
                 ) -> tuple[list[tuple[int, int]], float] | None:
    """Align `targets` to `log_probs` (frames x labels).

    Returns, per target token, the (first_frame, last_frame) it occupies, plus the mean
    log-probability of the chosen path's non-blank frames as a confidence score. None if
    there are too few frames for the targets."""
    result = forced_align_detailed(log_probs, targets, blank)
    if result is None:
        return None
    spans, token_lps = result
    flat = [lp for lps in token_lps for lp in lps]
    confidence = float(np.mean(flat)) if flat else float("-inf")
    return spans, confidence


def forced_align_detailed(log_probs: np.ndarray, targets: list[int], blank: int = 0,
                          frame_ranges: list[tuple[int, int]] | None = None,
                          outside_penalty: float = 20.0,
                          ) -> tuple[list[tuple[int, int]], list[list[float]]] | None:
    """Like `forced_align`, but returns each target token's per-frame log-probabilities
    instead of one overall score, so callers can score parts of the target separately.

    `frame_ranges` optionally gives, per target token, the frames [lo, hi) it should be
    emitted in. Emitting it outside costs `outside_penalty` per frame, which anchors a
    long (whole-song) alignment to rough line times without forbidding it from moving.

    Works one frame at a time, so memory is frames x states bytes for the backpointers
    rather than a full float matrix; a whole song (~12k frames, ~6k states) stays small."""
    T = log_probs.shape[0]
    N = len(targets)
    if N == 0:
        return [], []
    ext = [blank]
    for t in targets:
        ext += [t, blank]
    S = len(ext)
    repeats = sum(1 for a, b in zip(targets, targets[1:]) if a == b)
    if T < N + repeats:
        return None

    ext_arr = np.array(ext)
    can_skip = np.zeros(S, dtype=bool)
    for s in range(2, S):
        can_skip[s] = ext[s] != blank and ext[s] != ext[s - 2]

    lo = hi = None
    if frame_ranges is not None:
        lo = np.zeros(S, dtype=np.int64)
        hi = np.full(S, T, dtype=np.int64)
        for i, (a, b) in enumerate(frame_ranges):
            lo[2 * i + 1], hi[2 * i + 1] = a, b

    def emit(t: int) -> np.ndarray:
        e = log_probs[t, ext_arr].astype(np.float64)
        if lo is not None:
            e = np.where((t >= lo) & (t < hi), e, e - outside_penalty)
        return e

    score = np.full(S, NEG)
    e0 = emit(0)
    score[0] = e0[0]
    if S > 1:
        score[1] = e0[1]
    back = np.zeros((T, S), dtype=np.int8)  # 0 stay, 1 from s-1, 2 from s-2
    neg1 = np.array([NEG])
    neg2 = np.array([NEG, NEG])
    for t in range(1, T):
        step = np.concatenate((neg1, score[:-1]))
        skip = np.where(can_skip, np.concatenate((neg2, score[:-2])), NEG)
        stacked = np.stack([score, step, skip])
        choice = stacked.argmax(axis=0)
        score = stacked[choice, np.arange(S)] + emit(t)
        back[t] = choice

    end = S - 1 if S == 1 or score[S - 1] >= score[S - 2] else S - 2
    if score[end] <= NEG / 2:
        return None

    path = np.empty(T, dtype=np.int64)
    s = end
    for t in range(T - 1, -1, -1):
        path[t] = s
        s -= int(back[t, s])

    spans: list[list[int]] = [[-1, -1] for _ in range(N)]
    token_lps: list[list[float]] = [[] for _ in range(N)]
    for t, s in enumerate(path):
        if s % 2 == 1:
            i = s // 2
            if spans[i][0] < 0:
                spans[i][0] = t
            spans[i][1] = t
            token_lps[i].append(float(log_probs[t, ext[s]]))
    return [tuple(sp) for sp in spans], token_lps
