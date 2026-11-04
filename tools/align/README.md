# Syllable alignment tool

Adds syllable (karaoke-style) timing to lyrics the overlay has already cached, by aligning
them to the song's audio. The overlay then fills in the current line syllable by syllable
(Settings → Highlight sung words).

This is an optional extra. It works on songs you have as audio files, or on songs the
overlay recorded while you played them in Spotify (see below). It needs Python and a few GB
of machine-learning libraries and models, so it is kept separate from the exe.

## How it works

1. Finds the song's entry in the overlay's lyrics cache (`%APPDATA%\LyricsOverlay\cache`)
   by the audio file's title/artist tags and duration.
2. Separates the vocals from the music with [Demucs](https://github.com/facebookresearch/demucs).
3. Splits each lyric line into syllables (hyphenation patterns via `pyphen`, one syllable
   per character for Chinese/Japanese/Korean).
4. For each line, force-aligns its syllables to the vocals within that line's time window,
   using the multilingual [MMS forced-alignment model](https://docs.pytorch.org/audio/stable/tutorials/forced_alignment_for_multilingual_data_tutorial.html)
   (text is romanised with `uroman` first, so most languages work).
5. Writes the timing back into the cache entry (keeping `lyrics.lrc.bak`). Lines it isn't
   confident about are left without syllable timing and display as before.

## Usage

Install [uv](https://docs.astral.sh/uv/), then from this folder:

```
uv run align.py "D:\Music\Rick Astley - Never Gonna Give You Up.flac"
```

Play the song once in the overlay first (with caching on) so its lyrics are in the cache.
The first run downloads the Python packages and models (a few GB). Expect roughly a minute
per song on CPU, a few seconds with an NVIDIA GPU.

### Songs recorded from Spotify

Turn on **Settings → Record Spotify for syllable timing** in the overlay (needs lyrics
caching on). While it's on, every song you play in the Spotify desktop app is recorded
into its cache entry, as long as you play it from the start to the end without pausing or
seeking (anything else is thrown away, because the audio would no longer line up with
the lyrics). Only Spotify's own audio is captured, not other apps or notification sounds.
Songs without line-synced lyrics, or that already have syllable timing, aren't recorded.

Then process everything that was recorded:

```
uv run align.py --pending
```

Recordings are kept after processing, so you can re-align them later, for example after
tweaking `--min-confidence` or trying `--demucs-model htdemucs_ft`:

```
uv run align.py --pending --redo                    # re-align every recorded song
uv run align.py --entry "<cache entry folder>"      # re-align one recorded song
```

They take about 10 MB per minute of audio. Add `--delete-audio` to `--pending` to remove
each recording once it's processed, or delete `recording.wav` from a song's cache folder
by hand. They're for this analysis only; the tool and the overlay never play them back.

Useful options (`uv run align.py --help` for all):

| Option | |
|---|---|
| `--pending` | Process the songs the overlay recorded from Spotify that aren't aligned yet |
| `--redo` | With `--pending`, re-align every recorded song |
| `--delete-audio` | With `--pending`, delete recordings after processing |
| `--entry <folder>` | Cache entry to update, if the file's tags don't match what the overlay saw |
| `--lang en` | Lyrics language for syllable splitting (detected automatically otherwise) |
| `--words` | Time whole words instead of syllables |
| `--dry-run` | Print the result as Enhanced LRC instead of writing the cache |
| `--lrc-out file.lrc` | Also save the result as an Enhanced LRC file |
| `--demucs-model htdemucs_ft` | Slower, slightly cleaner vocal separation |
| `--min-confidence -4.0` | Lower to keep more lines, raise to drop doubtful ones |

**GPU:** the default `torch` from PyPI is CPU-only on Windows. For an NVIDIA GPU, install
the CUDA build into the tool's environment:
`uv pip install torch torchaudio --index-url https://download.pytorch.org/whl/cu128 --reinstall`.

## Accuracy

Expect syllables within about a tenth of a second on clear pop vocals. Heavy vocal effects,
stacked harmonies, fast rap, long melismas and lyrics that don't match what's sung will be
worse. Check the per-line `ok`/`skip` output and confidence scores.

## Tests

```
uv run pytest
```
