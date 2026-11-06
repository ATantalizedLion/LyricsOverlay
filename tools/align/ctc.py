"""CTC forced alignment (Viterbi) over a log-probability matrix.

Written out in numpy rather than using torchaudio.functional.forced_align, which is
deprecated in recent torchaudio releases."""

from __future__ import annotations

import numpy as np


def forced_align(log_probs: np.ndarray, targets: list[int], blank: int = 0
                 ) -> tuple[list[tuple[int, int]], float] | None:
    """Align `targets` to `log_probs` (frames x labels).

    Returns, per target token, the (first_frame, last_frame) it occupies, plus the mean
    log-probability of the chosen path's non-blank frames as a confidence score. None if
    there are too few frames for the targets."""
    T = log_probs.shape[0]
    N = len(targets)
    if N == 0:
        return [], 0.0
    # Extended sequence: blank t0 blank t1 ... tN-1 blank
    ext = [blank]
    for t in targets:
        ext += [t, blank]
    S = len(ext)
    # Each repeated token needs a blank between, so count the minimum frames needed.
    repeats = sum(1 for a, b in zip(targets, targets[1:]) if a == b)
    if T < N + repeats:
        return None

    neg = -1e30
    ext_arr = np.array(ext)
    emit = log_probs[:, ext_arr]  # T x S
    # Transitions into state s may come from s, s-1, and s-2 (if skip allowed).
    can_skip = np.zeros(S, dtype=bool)
    for s in range(2, S):
        can_skip[s] = ext[s] != blank and ext[s] != ext[s - 2]

    score = np.full(S, neg)
    score[0] = emit[0, 0]
    if S > 1:
        score[1] = emit[0, 1]
    back = np.zeros((T, S), dtype=np.int8)  # 0 stay, 1 from s-1, 2 from s-2
    for t in range(1, T):
        stay = score
        step = np.concatenate(([neg], score[:-1]))
        skip = np.where(can_skip, np.concatenate(([neg, neg], score[:-2])), neg)
        stacked = np.stack([stay, step, skip])
        choice = stacked.argmax(axis=0)
        score = stacked[choice, np.arange(S)] + emit[t]
        back[t] = choice

    # End in the final blank or final token.
    end = S - 1 if S == 1 or score[S - 1] >= score[S - 2] else S - 2
    if score[end] <= neg / 2:
        return None

    path = np.empty(T, dtype=np.int64)
    s = end
    for t in range(T - 1, -1, -1):
        path[t] = s
        s -= back[t, s]

    spans: list[list[int]] = [[-1, -1] for _ in range(N)]
    token_lp = []
    for t, s in enumerate(path):
        if s % 2 == 1:
            i = s // 2
            if spans[i][0] < 0:
                spans[i][0] = t
            spans[i][1] = t
            token_lp.append(emit[t, s])
    confidence = float(np.mean(token_lp)) if token_lp else float("-inf")
    return [tuple(sp) for sp in spans], confidence
