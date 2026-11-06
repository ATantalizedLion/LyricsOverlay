"""Splitting lyric lines into syllables, and turning syllables into aligner tokens.

Everything here is plain text processing, so it can be tested without any models.
"""

from __future__ import annotations

import re
import unicodedata
from dataclasses import dataclass
from functools import lru_cache

import pyphen

# The MMS forced-alignment model's alphabet (romanised text), see
# torchaudio.pipelines.MMS_FA.get_labels(). Index 0 is the CTC blank.
MMS_LABELS = ("-", "a", "i", "e", "n", "o", "u", "t", "s", "r", "m", "k", "l", "d", "g",
              "h", "y", "b", "p", "w", "c", "v", "j", "z", "f", "'", "q", "x", "*")
BLANK = 0
STAR = len(MMS_LABELS) - 1
_LABEL_INDEX = {c: i for i, c in enumerate(MMS_LABELS)}

# Small kana that belong to the previous mora (きゃ, っ is its own mora in timing terms
# but sounds as a pause, so keep it attached too).
_SMALL_KANA = set("ゃゅょぁぃぅぇぉゎャュョァィゥェォヮっッー")


@dataclass
class Syllable:
    text: str
    """Exactly as it appears in the line, including trailing whitespace/punctuation."""
    tokens: list[int]
    """MMS label indices for the sound of this syllable; may be empty (e.g. digits)."""
    word_index: int


def is_cjk_char(ch: str) -> bool:
    """Characters that are each roughly one syllable: Han, kana, Hangul."""
    name = unicodedata.name(ch, "")
    return name.startswith(("CJK UNIFIED", "CJK COMPATIBILITY IDEOGRAPH", "HIRAGANA",
                            "KATAKANA", "HANGUL SYLLABLE"))


@lru_cache(maxsize=None)
def _hyphenator(lang: str | None) -> pyphen.Pyphen | None:
    if not lang:
        return None
    fallback = pyphen.language_fallback(lang.replace("-", "_"))
    if fallback is None:
        return None
    return pyphen.Pyphen(lang=fallback)


def split_units(line: str) -> list[str]:
    """Split a line into word-ish units whose concatenation is the line.

    Latin/Cyrillic/etc. words keep their trailing whitespace; CJK characters become one
    unit each (with any trailing whitespace or punctuation)."""
    units: list[str] = []
    for chunk in re.findall(r"\S+\s*|\s+", line):
        if not chunk.strip():
            if units:
                units[-1] += chunk
            else:
                units.append(chunk)
            continue
        current = ""
        for ch in chunk:
            if ch in _SMALL_KANA:
                # Belongs to the previous mora.
                if current:
                    current += ch
                elif units:
                    units[-1] += ch
                else:
                    current = ch
            elif is_cjk_char(ch):
                if any(c.isalnum() for c in current):
                    units.append(current)
                    current = ch
                else:
                    # Leading punctuation like 「 sticks to the character after it.
                    current += ch
            elif current and any(is_cjk_char(c) for c in current) and ch.isalnum():
                # Latin text right after CJK starts a new unit; punctuation stays.
                units.append(current)
                current = ch
            else:
                current += ch
        if current:
            units.append(current)
    return units


def split_syllables(word: str, lang: str | None) -> list[str]:
    """Split one unit into syllables using hyphenation patterns. Leading/trailing
    punctuation and whitespace stay attached to the first/last piece."""
    if any(is_cjk_char(c) for c in word):
        return [word]
    m = re.match(r"^(\W*)(.*?)(\W*)$", word, re.S)
    assert m
    lead, core, trail = m.groups()
    if not lang or not core or "-" in core:
        return [word]
    hyph = _hyphenator(lang)
    if hyph is None:
        # No patterns for this language (e.g. English words in a Japanese song).
        positions = vowel_group_positions(core, "en") if core.isascii() else []
    else:
        # Hyphenation patterns deliberately avoid short splits ("gon-na", "a-round"),
        # which karaoke does want, so fall back to splitting on vowel groups.
        positions = hyph.positions(core) or vowel_group_positions(core, lang)
    if not positions:
        return [word]
    pieces, prev = [], 0
    for p in positions:
        pieces.append(core[prev:p])
        prev = p
    pieces.append(core[prev:])
    pieces[0] = lead + pieces[0]
    pieces[-1] = pieces[-1] + trail
    return pieces


_VOWELS = set("aeiouyàáâäãåèéêëìíîïòóôöõùúûüýÿæœø")


def vowel_group_positions(core: str, lang: str | None) -> list[int]:
    """Rough syllable boundaries for a single word: one syllable per vowel group, a lone
    consonant between vowels starts the next syllable (ma-ny) and a cluster is split
    after its first consonant (gon-na)."""
    lower = core.lower()
    if not lower.isalpha():
        return []
    is_vowel = [c in _VOWELS for c in lower]
    if lower[0] == "y":
        is_vowel[0] = False  # "you", "yeah"
    if lang and lang.startswith("en") and len(lower) > 3 and lower.endswith("e") and not is_vowel[-2]:
        is_vowel[-1] = False  # silent final e: "make", "time"
    groups = []  # (start, end) of each vowel run
    i = 0
    while i < len(lower):
        if is_vowel[i]:
            j = i
            while j + 1 < len(lower) and is_vowel[j + 1]:
                j += 1
            groups.append((i, j + 1))
            i = j + 1
        else:
            i += 1
    positions = []
    for (_, end), (start, _) in zip(groups, groups[1:]):
        consonants = start - end
        positions.append(end + (0 if consonants <= 1 else 1))
    return [p for p in positions if 0 < p < len(core)]


def tokens_for(romanised: str) -> list[int]:
    out = []
    for ch in romanised.lower():
        if ch in ("’", "`"):
            ch = "'"
        idx = _LABEL_INDEX.get(ch)
        if idx is not None and idx not in (BLANK, STAR):
            out.append(idx)
    return out


def syllabify_line(line: str, lang: str | None, romanise, tokenize=tokens_for,
                   words: bool = False) -> list[Syllable]:
    """Split `line` into syllables (texts concatenate back to `line`) with tokens.

    `romanise` turns a piece of text into Latin script (uroman in practice), `tokenize`
    turns that into the aligner's label indices (MMS labels by default). With `words`,
    each word-ish unit stays whole."""
    syllables: list[Syllable] = []
    for wi, unit in enumerate(split_units(line)):
        for piece in ([unit] if words else split_syllables(unit, lang)):
            core = piece.strip()
            roman = core if core.isascii() else romanise(core)
            syllables.append(Syllable(piece, tokenize(roman), wi))
    return syllables
