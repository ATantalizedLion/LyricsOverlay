"""The configurable alignment pipeline shared by align.py (batch) and lab.py (tuning UI).

    audio -> vocal separation -> CTC emissions -> forced alignment -> syllable segments

Every step is chosen by a `RunConfig`, so different separators, aligners and alignment
strategies can be tried on the same song and compared. The two slow steps, separation and
emissions, are cached per song when a cache folder is given, so changing only the
alignment parameters re-runs in about a second.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import time
from dataclasses import asdict, dataclass, fields
from pathlib import Path

import numpy as np

from ctc import forced_align_detailed
from text import MMS_LABELS, Syllable, syllabify_line, tokens_for

ALIGN_SR = 16_000
# wav2vec2-style models emit one frame per 320 samples, 20 ms at 16 kHz.
FRAME_SAMPLES = 320
FRAME_MS = 20

SEPARATORS = {
    "htdemucs": "Demucs v4 (default)",
    "htdemucs_ft": "Demucs v4 fine-tuned: 4x slower, a bit cleaner",
    "mdx_extra": "Demucs v3 (MDX extra)",
    "uvr:model_bs_roformer_ep_317_sdr_12.9755.ckpt": "BS-Roformer (UVR): cleanest vocals, slow on CPU",
    "uvr:UVR-MDX-NET-Voc_FT.onnx": "MDX-Net Voc FT (UVR)",
    "none": "None: align on the full mix",
}

ALIGNERS = {
    "mms": "MMS forced aligner, multilingual (default)",
    "w2v2-base": "wav2vec2 base 960h, English only",
    "w2v2-large": "wav2vec2 large 960h, English only, slower",
    "even": "Baseline: spread each line evenly over its syllables",
}


@dataclass
class RunConfig:
    separator: str = "htdemucs"
    aligner: str = "mms"
    mode: str = "line"
    """"line": align each line within its own time window from the LRC. "song": align the
    whole song's lyrics in one pass, which copes with LRC line times being off."""
    unit: str = "syllable"
    """"syllable" or "word"."""
    lang: str = ""
    """Lyrics language for syllable splitting; empty to detect it."""
    pad_before_ms: int = 400
    """Line mode: how far before the line's LRC time its window starts."""
    pad_after_ms: int = 300
    """Line mode: how far past the next line's LRC time its window ends."""
    max_window_ms: int = 30_000
    anchor_slack_ms: int = 2_000
    """Song mode: how far a line may drift from its LRC window before it's penalised.
    0 lets lines go anywhere."""
    star: bool = True
    """Pad each window with a wildcard token that soaks up unrelated sound (the previous
    line's tail, ad-libs, backing vocals)."""
    min_confidence: float = -4.0
    """Lines whose mean log-probability is below this keep line timing only."""
    offset_ms: int = 0
    """Shifts all syllable times; positive makes them later."""
    retime_lines: bool = False
    """Move each aligned line's start to its first syllable, for LRCs with sloppy times."""

    @classmethod
    def from_dict(cls, d: dict | None) -> "RunConfig":
        cfg = cls()
        for f in fields(cls):
            if d and f.name in d and d[f.name] is not None:
                default = getattr(cfg, f.name)
                value = d[f.name]
                if isinstance(default, bool):
                    value = value if isinstance(value, bool) else str(value).lower() in ("1", "true", "yes", "on")
                else:
                    value = type(default)(value)
                setattr(cfg, f.name, value)
        if cfg.separator not in SEPARATORS and not cfg.separator.startswith("uvr:"):
            raise ValueError(f"unknown separator {cfg.separator!r}")
        if cfg.aligner not in ALIGNERS:
            raise ValueError(f"unknown aligner {cfg.aligner!r}")
        if cfg.mode not in ("line", "song"):
            raise ValueError(f"unknown mode {cfg.mode!r}")
        if cfg.unit not in ("syllable", "word"):
            raise ValueError(f"unknown unit {cfg.unit!r}")
        return cfg

    def to_dict(self) -> dict:
        return asdict(self)

    def summary(self) -> str:
        parts = [self.aligner if self.aligner == "even" else f"{self.separator} + {self.aligner}"]
        if self.aligner != "even":
            parts.append(self.mode)
        parts.append(self.unit)
        if self.offset_ms:
            parts.append(f"{self.offset_ms:+d}ms")
        return ", ".join(parts)


def defaults_file(data_dir: Path) -> Path:
    return data_dir / "align-lab" / "defaults.json"


def load_defaults(data_dir: Path) -> RunConfig:
    """The settings saved from the lab with "Use as default", or the built-in ones."""
    path = defaults_file(data_dir)
    try:
        return RunConfig.from_dict(json.loads(path.read_text(encoding="utf-8")))
    except (OSError, ValueError, json.JSONDecodeError):
        return RunConfig()


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


def shift_to_song_time(wav, sr: int, offset_ms: int):
    """Pads (offset > 0) or trims (offset < 0) the start so sample 0 is song time 0."""
    import torch

    n = abs(offset_ms) * sr // 1000
    if offset_ms > 0:
        return torch.cat([torch.zeros(wav.shape[0], n, dtype=wav.dtype), wav], dim=1)
    return wav[:, n:]


def separate_with_demucs(wav, sr: int, device: str, model):
    """Returns mono vocals at 16 kHz as a (1, samples) tensor."""
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
# Aligners


class CtcAligner:
    """A wav2vec2-style CTC model plus how to turn romanised text into its labels."""

    def __init__(self, name: str, device: str):
        import torch
        import torchaudio

        self.name = name
        self.torch = torch
        self.device = device
        if name == "mms":
            bundle = torchaudio.pipelines.MMS_FA
            labels = bundle.get_labels(star="*")
            assert tuple(labels) == MMS_LABELS, labels
            self.model = bundle.get_model(with_star=True).to(device).eval()
            self.labels = list(labels)
            self.star = len(labels) - 1
            self.word_sep = None
            self.native_star = True
        else:
            bundle = {
                "w2v2-base": torchaudio.pipelines.WAV2VEC2_ASR_BASE_960H,
                "w2v2-large": torchaudio.pipelines.WAV2VEC2_ASR_LARGE_LV60K_960H,
            }[name]
            self.model = bundle.get_model().to(device).eval()
            self.labels = list(bundle.get_labels()) + ["*"]
            self.star = len(self.labels) - 1
            self.word_sep = self.labels.index("|")
            self.native_star = False
        self.blank = 0
        self._index = {c: i for i, c in enumerate(self.labels)}

    def tokenize(self, romanised: str) -> list[int]:
        if self.name == "mms":
            return tokens_for(romanised)
        out = []
        for ch in romanised.upper():
            if ch in ("’", "`"):
                ch = "'"
            idx = self._index.get(ch)
            if idx is not None and idx not in (self.blank, self.star, self.word_sep):
                out.append(idx)
        return out

    def _emit(self, wav: np.ndarray) -> np.ndarray:
        torch = self.torch
        if len(wav) < 400:
            wav = np.pad(wav, (0, 400 - len(wav)))
        with torch.inference_mode():
            x = torch.from_numpy(np.ascontiguousarray(wav, dtype=np.float32))[None].to(self.device)
            emission, _ = self.model(x)
            lp = torch.log_softmax(emission[0].float(), dim=-1).cpu().numpy()
        if not self.native_star:
            # No trained wildcard: score it as "some non-blank sound", log(1 - p(blank)).
            star = np.log1p(-np.exp(np.minimum(lp[:, self.blank], -1e-6)))
            lp = np.concatenate([lp, star[:, None]], axis=1)
        return lp

    def emissions(self, vocals: np.ndarray, chunk_s: int = 30, context_s: int = 2) -> np.ndarray:
        """Log-probabilities (frames x labels) for the whole song on a fixed 20 ms grid.
        Runs the model on overlapping chunks so memory stays bounded on long songs."""
        n = len(vocals)
        step = chunk_s * ALIGN_SR
        ctx = context_s * ALIGN_SR
        parts = []
        for a in range(0, n, step):
            b = min(n, a + step)
            s0, s1 = max(0, a - ctx), min(n, b + ctx)
            em = self._emit(vocals[s0:s1])
            first = (a - s0) // FRAME_SAMPLES
            count = -(-(b - a) // FRAME_SAMPLES)
            part = em[first:first + count]
            if len(part) < count:
                part = np.concatenate([part, np.repeat(part[-1:], count - len(part), axis=0)])
            parts.append(part)
        return np.concatenate(parts).astype(np.float32)


# ---------------------------------------------------------------------------
# Model and intermediate-result caching


class ModelPool:
    """Loads models lazily and keeps them for the next song or run."""

    def __init__(self, device: str | None = None):
        import torch

        self.device = device or ("cuda" if torch.cuda.is_available() else "cpu")
        self._romanise = None
        self._aligners: dict[str, CtcAligner] = {}
        self._demucs: dict[str, object] = {}
        self._uvr = None

    def romanise(self, text: str) -> str:
        if self._romanise is None:
            from uroman import Uroman

            self._romanise = Uroman().romanize_string
        return self._romanise(text)

    def aligner(self, name: str) -> CtcAligner:
        if name not in self._aligners:
            self._aligners[name] = CtcAligner(name, self.device)
        return self._aligners[name]

    def separate(self, audio: Path, offset_ms: int, separator: str, log=print) -> np.ndarray:
        """Mono 16 kHz vocals in song time (sample 0 = song start)."""
        if separator == "none":
            wav = shift_to_song_time(load_audio(audio, ALIGN_SR), ALIGN_SR, offset_ms)
            return wav.mean(0).numpy()
        if separator.startswith("uvr:"):
            return self._separate_uvr(audio, offset_ms, separator[4:], log)
        if separator not in self._demucs:
            from demucs.pretrained import get_model

            model = get_model(separator)
            model.eval()
            self._demucs[separator] = model
        model = self._demucs[separator]
        wav = shift_to_song_time(load_audio(audio, model.samplerate), model.samplerate, offset_ms)
        return separate_with_demucs(wav, model.samplerate, self.device, model)[0].numpy()

    def _separate_uvr(self, audio: Path, offset_ms: int, model_file: str, log) -> np.ndarray:
        try:
            from audio_separator.separator import Separator
        except ImportError as e:  # optional dependency group
            raise RuntimeError("UVR models need the optional 'uvr' packages: start the lab with "
                               "`uv run --group uvr lab.py` (or add --group uvr to align.py)") from e
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            models = Path.home() / ".cache" / "lyricsoverlay-align" / "uvr-models"
            sep = Separator(output_dir=tmp, output_single_stem="Vocals", output_format="WAV",
                            model_file_dir=str(models))
            sep.load_model(model_filename=model_file)
            outputs = sep.separate(str(audio))
            if not outputs:
                raise RuntimeError(f"{model_file} produced no vocals")
            out = Path(outputs[0])
            if not out.is_absolute():
                out = Path(tmp) / out
            wav = load_audio(out, ALIGN_SR)
        return shift_to_song_time(wav, ALIGN_SR, offset_ms).mean(0).numpy()


def _safe(name: str) -> str:
    return "".join(c if c.isalnum() or c in "-_." else "_" for c in name)


def vocals_cache_path(cache_dir: Path, separator: str) -> Path:
    return cache_dir / "vocals" / f"{_safe(separator)}.wav"


def get_vocals(pool: ModelPool, audio: Path, offset_ms: int, separator: str,
               cache_dir: Path | None, log=print) -> np.ndarray:
    import soundfile as sf

    path = vocals_cache_path(cache_dir, separator) if cache_dir else None
    stamp = _source_stamp(audio, offset_ms)
    if path and path.exists() and _stamp_ok(path, stamp):
        log(f"Using cached vocals ({separator})")
        data, _ = sf.read(str(path), dtype="float32")
        return data
    log("Loading the full mix" if separator == "none" else f"Separating vocals with {separator}...")
    vocals = pool.separate(audio, offset_ms, separator, log).astype(np.float32)
    if path:
        path.parent.mkdir(parents=True, exist_ok=True)
        sf.write(str(path), vocals, ALIGN_SR, subtype="FLOAT")
        _write_stamp(path, stamp)
    return vocals


def get_emissions(pool: ModelPool, vocals: np.ndarray, aligner: str, separator: str,
                  cache_dir: Path | None, stamp: dict, log=print) -> np.ndarray:
    path = cache_dir / "emissions" / f"{_safe(separator)}__{aligner}.npy" if cache_dir else None
    if path and path.exists() and _stamp_ok(path, stamp):
        log(f"Using cached {aligner} emissions")
        return np.load(path)
    log(f"Running {aligner} on the vocals...")
    em = pool.aligner(aligner).emissions(vocals)
    if path:
        path.parent.mkdir(parents=True, exist_ok=True)
        np.save(path, em)
        _write_stamp(path, stamp)
    return em


def _source_stamp(audio: Path, offset_ms: int) -> dict:
    st = audio.stat()
    return {"source": str(audio), "size": st.st_size, "mtime": int(st.st_mtime), "offset_ms": offset_ms}


def _stamp_ok(path: Path, stamp: dict) -> bool:
    try:
        return json.loads(path.with_suffix(path.suffix + ".json").read_text(encoding="utf-8")) == stamp
    except (OSError, json.JSONDecodeError):
        return False


def _write_stamp(path: Path, stamp: dict) -> None:
    path.with_suffix(path.suffix + ".json").write_text(json.dumps(stamp), encoding="utf-8")


# ---------------------------------------------------------------------------
# Alignment


def detect_language(lines: list[str]) -> str | None:
    try:
        from langdetect import DetectorFactory, detect

        DetectorFactory.seed = 0
        return detect("\n".join(lines))
    except Exception:  # noqa: BLE001
        return None


def _targets(syllables: list[Syllable], aligner: CtcAligner, owner_base=()):
    """Token ids for a line and, per token, the syllable index owning it (None for
    word separators)."""
    targets, owner = [], []
    for i, syl in enumerate(syllables):
        if (aligner.word_sep is not None and targets and i > 0
                and syl.word_index != syllables[i - 1].word_index and syl.tokens):
            targets.append(aligner.word_sep)
            owner.append(None)
        targets += syl.tokens
        owner += [(*owner_base, i)] * len(syl.tokens)
    return targets, owner


def _times_from_spans(spans, owner, n_syllables, frame0: int, key=lambda o: o):
    times: list[tuple[int, int] | None] = [None] * n_syllables
    for (f0, f1), o in zip(spans, owner):
        if o is None:
            continue
        i = key(o)
        t0, t1 = (frame0 + f0) * FRAME_MS, (frame0 + f1 + 1) * FRAME_MS
        times[i] = (t0, t1) if times[i] is None else (times[i][0], t1)
    return times


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


def line_windows(lines: list[dict], song_end_ms: int, cfg: RunConfig) -> list[tuple[int, int]]:
    out = []
    for i, line in enumerate(lines):
        next_ms = lines[i + 1]["time_ms"] if i + 1 < len(lines) else song_end_ms
        start = max(0, line["time_ms"] - cfg.pad_before_ms)
        end = min(next_ms + cfg.pad_after_ms, start + cfg.max_window_ms, song_end_ms)
        out.append((start, end))
    return out


def align_lines(em: np.ndarray, aligner: CtcAligner, lines, sylls, cfg: RunConfig,
                song_end_ms: int):
    """Line mode: each line force-aligned within its own window. Returns per line
    (times, confidence) or None."""
    results = []
    for line, syllables, (start, end) in zip(lines, sylls, line_windows(lines, song_end_ms, cfg)):
        f0, f1 = start // FRAME_MS, min(len(em), end // FRAME_MS)
        targets, owner = _targets(syllables, aligner)
        if not targets or f1 - f0 < 5:
            results.append(None)
            continue
        if cfg.star:
            targets, owner = [aligner.star, *targets, aligner.star], [None, *owner, None]
        res = forced_align_detailed(em[f0:f1], targets, blank=aligner.blank)
        if res is None:
            results.append(None)
            continue
        spans, lps = res
        flat = [v for vals, o in zip(lps, owner) if o is not None for v in vals]
        conf = float(np.mean(flat)) if flat else float("-inf")
        results.append((_times_from_spans(spans, owner, len(syllables), f0, key=lambda o: o[0]), conf))
    return results


def align_song(em: np.ndarray, aligner: CtcAligner, lines, sylls, cfg: RunConfig,
               song_end_ms: int):
    """Song mode: all lyrics aligned in one pass, optionally anchored to the LRC windows."""
    targets, owner, ranges = [], [], []
    T = len(em)
    windows = line_windows(lines, song_end_ms, RunConfig(pad_before_ms=0, pad_after_ms=0,
                                                        max_window_ms=cfg.max_window_ms))
    if cfg.star:
        targets.append(aligner.star)
        owner.append(None)
        ranges.append((0, T))
    for li, (syllables, (ws, we)) in enumerate(zip(sylls, windows)):
        t, o = _targets(syllables, aligner, owner_base=(li,))
        if not t:
            continue
        if cfg.anchor_slack_ms > 0:
            r = (max(0, (ws - cfg.anchor_slack_ms) // FRAME_MS), min(T, (we + cfg.anchor_slack_ms) // FRAME_MS))
        else:
            r = (0, T)
        if owner and owner[-1] is not None and targets[-1] != aligner.star and aligner.word_sep is not None:
            targets.append(aligner.word_sep)
            owner.append(None)
            ranges.append((0, T))
        targets += t
        owner += o
        ranges += [r if oo is not None else (0, T) for oo in o]
        if cfg.star:
            targets.append(aligner.star)
            owner.append(None)
            ranges.append((0, T))
    results: list = [None] * len(lines)
    if not any(o is not None for o in owner):
        return results
    res = forced_align_detailed(em, targets, blank=aligner.blank,
                                frame_ranges=ranges if cfg.anchor_slack_ms > 0 else None)
    if res is None:
        return results
    spans, lps = res
    per_line: dict[int, list[float]] = {}
    for lp, o in zip(lps, owner):
        if o is not None:
            per_line.setdefault(o[0], []).extend(lp)
    for li, syllables in enumerate(sylls):
        if li not in per_line:
            continue
        sel = [(sp, o) for sp, o in zip(spans, owner) if o is not None and o[0] == li]
        times = _times_from_spans([s for s, _ in sel], [o for _, o in sel], len(syllables), 0,
                                  key=lambda o: o[1])
        lp = per_line[li]
        results[li] = (times, float(np.mean(lp)) if lp else float("-inf"))
    return results


def align_even(lines, sylls, song_end_ms: int):
    """Baseline without audio: each line's syllables spread over its LRC window, weighted
    by how many sounds they have."""
    results = []
    for i, (line, syllables) in enumerate(zip(lines, sylls)):
        if not syllables:
            results.append(None)
            continue
        start = line["time_ms"]
        next_ms = lines[i + 1]["time_ms"] if i + 1 < len(lines) else song_end_ms
        end = min(next_ms, start + max(1500, 450 * len(syllables)))
        weights = [max(1, len(s.tokens)) for s in syllables]
        total = sum(weights)
        times, t = [], float(start)
        for w in weights:
            d = (end - start) * w / total
            times.append((int(t), int(t + d)))
            t += d
        results.append((times, None))
    return results


def run_alignment(lines: list[dict], audio: Path | None, audio_offset_ms: int, cfg: RunConfig,
                  pool: ModelPool, cache_dir: Path | None = None, duration_ms: int | None = None,
                  log=print) -> dict:
    """Aligns `lines` ({time_ms, text} from the LRC) to `audio` with `cfg`.

    Returns {"config", "lang", "lines": [{time_ms, orig_time_ms, text, segments,
    confidence, status}], "stats"}. `status` is "ok", "low" (below min_confidence; no
    segments), "failed" or "empty"."""
    t_start = time.time()
    timings = {}
    lang = None if cfg.unit == "word" else (cfg.lang or detect_language([l["text"] for l in lines]))
    log(f"{len(lines)} lines, {cfg.summary()}, syllable language {lang or 'none (whole words)'}, "
        f"device {pool.device}")

    if cfg.aligner == "even":
        aligner = None
        tokenize = tokens_for
        song_end_ms = duration_ms or (lines[-1]["time_ms"] + 5000 if lines else 0)
    else:
        if audio is None:
            raise ValueError("this aligner needs the song's audio")
        t0 = time.time()
        vocals = get_vocals(pool, audio, audio_offset_ms, cfg.separator, cache_dir, log)
        timings["separation_s"] = round(time.time() - t0, 1)
        t0 = time.time()
        stamp = _source_stamp(audio, audio_offset_ms)
        em = get_emissions(pool, vocals, cfg.aligner, cfg.separator, cache_dir, stamp, log)
        timings["emissions_s"] = round(time.time() - t0, 1)
        aligner = pool.aligner(cfg.aligner)
        tokenize = aligner.tokenize
        song_end_ms = len(em) * FRAME_MS

    sylls = [syllabify_line(l["text"], lang, pool.romanise, tokenize, words=cfg.unit == "word")
             if l["text"].strip() else [] for l in lines]

    t0 = time.time()
    if aligner is None:
        results = align_even(lines, sylls, song_end_ms)
    elif cfg.mode == "song":
        log("Aligning the whole song...")
        results = align_song(em, aligner, lines, sylls, cfg, song_end_ms)
    else:
        log("Aligning lines...")
        results = align_lines(em, aligner, lines, sylls, cfg, song_end_ms)
    timings["alignment_s"] = round(time.time() - t0, 1)

    out_lines = []
    prev_time = -1
    for line, syllables, res in zip(lines, sylls, results):
        entry = {"time_ms": line["time_ms"], "orig_time_ms": line["time_ms"], "text": line["text"],
                 "segments": [], "confidence": None, "status": "empty"}
        if not line["text"].strip():
            pass
        elif res is None:
            entry["status"] = "failed"
        else:
            times, conf = res
            entry["confidence"] = None if conf is None else round(conf, 3)
            if conf is not None and conf < cfg.min_confidence:
                entry["status"] = "low"
            else:
                segs = build_segments(syllables, times, line["time_ms"])
                for s in segs:
                    s["start_ms"] = max(0, s["start_ms"] + cfg.offset_ms)
                    s["end_ms"] = max(s["start_ms"], s["end_ms"] + cfg.offset_ms)
                entry["segments"] = segs
                entry["text"] = "".join(s["text"] for s in segs)
                entry["status"] = "ok"
                if cfg.retime_lines and segs and segs[0]["start_ms"] > prev_time:
                    entry["time_ms"] = segs[0]["start_ms"]
        prev_time = entry["time_ms"]
        out_lines.append(entry)
        if entry["status"] != "empty":
            score = "failed" if entry["confidence"] is None and entry["status"] == "failed" \
                else ("-" if entry["confidence"] is None else f"{entry['confidence']:.2f}")
            label = {"ok": "ok  ", "low": "skip", "failed": "skip"}[entry["status"]]
            shown = "|".join(s["text"].strip() for s in entry["segments"]) or line["text"]
            log(f"  {label}  [{fmt_ms(line['time_ms'])}] ({score}) {shown}")

    # Lines must stay in order for the overlay.
    for a, b in zip(out_lines, out_lines[1:]):
        if b["time_ms"] < a["time_ms"]:
            b["time_ms"] = b["orig_time_ms"] if b["orig_time_ms"] >= a["time_ms"] else a["time_ms"]

    confs = [l["confidence"] for l in out_lines if l["confidence"] is not None and l["status"] == "ok"]
    sung = [l for l in out_lines if l["status"] != "empty"]
    shift, spread = lrc_shift([l for l in sung if l["status"] == "ok"])
    stats = {
        "lines": len(sung),
        "aligned": sum(l["status"] == "ok" for l in sung),
        "low": sum(l["status"] == "low" for l in sung),
        "failed": sum(l["status"] == "failed" for l in sung),
        "mean_confidence": round(float(np.mean(confs)), 3) if confs else None,
        "lrc_shift_ms": shift,
        "lrc_shift_spread_ms": spread,
        **timings,
        "total_s": round(time.time() - t_start, 1),
    }
    log(f"Aligned {stats['aligned']} of {stats['lines']} lines in {stats['total_s']}s.")
    return {"config": cfg.to_dict(), "lang": lang, "lines": out_lines, "stats": stats}


def lrc_shift(lines: list[dict]) -> tuple[int | None, int | None]:
    """How far the first sung syllable of each line sits from the LRC line time: the median
    (positive = singing starts after the LRC time) and the median distance from that.

    LRC times usually sit on or just before the start of singing, so a large, consistent
    shift (small spread) means the whole song is off, typically because the recording's
    start position was misjudged; a large spread means the lines disagree."""
    d = [l["segments"][0]["start_ms"] - l["orig_time_ms"] for l in lines if l.get("segments")]
    if len(d) < 3:
        return None, None
    med = float(np.median(d))
    return int(round(med)), int(round(float(np.median(np.abs(np.array(d) - med)))))


def fmt_ms(ms: int) -> str:
    return f"{ms // 60000:02}:{(ms % 60000) / 1000:05.2f}"


def to_cache_lines(result_lines: list[dict]) -> list[dict]:
    """The overlay's cache format for a run's lines."""
    out = []
    for l in result_lines:
        line = {"time_ms": int(l["time_ms"]), "text": l["text"]}
        if l.get("segments"):
            line["segments"] = [{"start_ms": int(s["start_ms"]), "end_ms": int(s["end_ms"]),
                                 "text": s["text"]} for s in l["segments"]]
        out.append(line)
    return out
