use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct LyricLine {
    pub time_ms: usize,
    pub text: String,
    /// Word or syllable timing within the line, for karaoke-style highlighting. Empty when
    /// the source only has line-level timing. When present, the segments' texts joined
    /// together make up `text`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<LyricSegment>,
}

/// One timed piece of a line: a word, a syllable, or a single CJK character, including
/// any whitespace that follows it.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct LyricSegment {
    pub start_ms: usize,
    pub end_ms: usize,
    pub text: String,
}

impl LyricLine {
    pub fn new(time_ms: usize, text: String) -> Self {
        Self {
            time_ms,
            text,
            segments: vec![],
        }
    }

    /// How far through this line's segments we are at `elapsed_ms`, as a fractional
    /// count of characters sung. `None` if the line has no segment timing.
    #[allow(clippy::cast_precision_loss)]
    pub fn sung_chars(&self, elapsed_ms: usize) -> Option<f32> {
        if self.segments.is_empty() {
            return None;
        }
        let mut sung = 0.0;
        for seg in &self.segments {
            let len = seg.text.chars().count() as f32;
            if elapsed_ms >= seg.end_ms {
                sung += len;
                continue;
            }
            if elapsed_ms > seg.start_ms {
                let t = (elapsed_ms - seg.start_ms) as f32
                    / (seg.end_ms.saturating_sub(seg.start_ms)).max(1) as f32;
                sung += len * t.clamp(0.0, 1.0);
            }
            break;
        }
        Some(sung)
    }
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum LyricPosition {
    BeforeStart,
    Line(usize),
    AfterEnd(usize),
}

#[derive(Deserialize, Serialize, Debug)]
pub struct SongLyrics {
    pub synced_lyrics: Vec<LyricLine>,
    /// Untimed lyric text, used when a source has no line-level timing at all.
    /// Only meaningful when `synced_lyrics` is empty.
    #[serde(default)]
    pub plain_lyrics: Vec<String>,
}

impl SongLyrics {
    pub fn display_text_as_lyrics(text: String, duration_ms: usize) -> Self {
        Self {
            synced_lyrics: vec![
                LyricLine::new(0, text),
                LyricLine::new(duration_ms, " ".to_string()),
            ],
            plain_lyrics: vec![],
        }
    }

    pub fn plain(lines: Vec<String>) -> Self {
        Self {
            synced_lyrics: vec![],
            plain_lyrics: lines,
        }
    }
    pub fn find_current_index(&self, elapsed_ms: usize) -> LyricPosition {
        let mut lyric_pos = LyricPosition::BeforeStart;

        if self.synced_lyrics.is_empty() {
            return lyric_pos;
        }

        for (i, line) in self.synced_lyrics.iter().enumerate() {
            if line.time_ms <= elapsed_ms {
                lyric_pos = LyricPosition::Line(i);
            } else {
                return lyric_pos;
            }
        }

        LyricPosition::AfterEnd(self.synced_lyrics.len())
    }
}

pub fn parse_lrc(content: &str, strip_empty_lines: bool) -> SongLyrics {
    let mut lines: Vec<LyricLine> = Vec::new();

    for raw in content.lines() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }

        // Match [mm:ss.xx] or [mm:ss:xx] timestamps
        let mut rest = raw;
        while rest.starts_with('[') {
            if let Some(close) = rest.find(']') {
                let tag = &rest[1..close];
                rest = rest[close + 1..].trim();

                if let Some(ms) = parse_time_tag_to_ms(tag) {
                    let (text, segments) = parse_inline_segments(rest, ms);
                    if strip_empty_lines && text.is_empty() {
                        break;
                    }
                    lines.push(LyricLine {
                        time_ms: ms,
                        text,
                        segments,
                    });
                    break;
                }
                // Otherwise it's a metadata tag, skip
            } else {
                break;
            }
        }
    }

    fill_open_segment_ends(&mut lines);

    SongLyrics {
        synced_lyrics: lines,
        plain_lyrics: vec![],
    }
}

/// Sentinel for a segment whose end isn't known until the next line is parsed.
const OPEN_END: usize = usize::MAX;

/// Splits Enhanced LRC (A2 extension) inline word tags out of a line's text, e.g.
/// `<00:12.00>Ne<00:12.30>ver <00:12.60>gon<00:12.90>na<00:13.20>`. Each tag starts a
/// segment that runs until the next tag; a trailing tag with no text after it only marks
/// where the last segment ends. Text before the first tag starts at the line's own time.
/// Returns the plain line text, and no segments if there were no inline tags.
fn parse_inline_segments(rest: &str, line_ms: usize) -> (String, Vec<LyricSegment>) {
    let mut pieces: Vec<(usize, String)> = vec![(line_ms, String::new())];
    let mut found_tag = false;
    let mut remaining = rest;

    while let Some(open) = remaining.find('<') {
        let Some(close) = remaining[open..].find('>').map(|c| c + open) else {
            break;
        };
        let Some(ms) = parse_time_tag_to_ms(&remaining[open + 1..close]) else {
            // Not a time tag, keep the '<' as text and look further.
            pieces.last_mut().unwrap().1.push_str(&remaining[..=open]);
            remaining = &remaining[open + 1..];
            continue;
        };
        found_tag = true;
        pieces.last_mut().unwrap().1.push_str(&remaining[..open]);
        pieces.push((ms, String::new()));
        remaining = &remaining[close + 1..];
    }
    pieces.last_mut().unwrap().1.push_str(remaining);

    if !found_tag {
        return (rest.to_string(), vec![]);
    }

    let mut segments: Vec<LyricSegment> = Vec::new();
    for (i, (start_ms, text)) in pieces.iter().enumerate() {
        if text.is_empty() {
            continue;
        }
        let end_ms = pieces.get(i + 1).map_or(OPEN_END, |(next, _)| *next);
        segments.push(LyricSegment {
            start_ms: *start_ms,
            end_ms: end_ms.max(*start_ms),
            text: text.clone(),
        });
    }

    // Keep the plain text identical to what the overlay showed before: trimmed.
    if let Some(first) = segments.first_mut() {
        first.text = first.text.trim_start().to_string();
    }
    if let Some(last) = segments.last_mut() {
        last.text = last.text.trim_end().to_string();
    }
    segments.retain(|s| !s.text.is_empty());

    let text = segments.iter().map(|s| s.text.as_str()).collect();
    (text, segments)
}

/// Lines whose last word had no closing tag get it ended by the next line's start.
fn fill_open_segment_ends(lines: &mut [LyricLine]) {
    const LAST_LINE_FALLBACK_MS: usize = 1_000;
    let next_starts: Vec<Option<usize>> = (0..lines.len())
        .map(|i| lines.get(i + 1).map(|l| l.time_ms))
        .collect();
    for (line, next_start) in lines.iter_mut().zip(next_starts) {
        for seg in &mut line.segments {
            if seg.end_ms == OPEN_END {
                seg.end_ms = next_start
                    .filter(|&n| n > seg.start_ms)
                    .unwrap_or(seg.start_ms + LAST_LINE_FALLBACK_MS);
            }
        }
    }
}

fn parse_time_tag_to_ms(tag: &str) -> Option<usize> {
    let parts: Vec<&str> = tag.splitn(2, ':').collect();
    if parts.len() != 2 {
        return None;
    }
    let minutes: usize = parts[0].trim().parse().ok()?;

    let sec_part = parts[1];
    let (secs_str, centis_str) = if let Some(dot) = sec_part.find('.') {
        // mm:ss.xx
        (&sec_part[..dot], &sec_part[dot + 1..])
    } else if let Some(colon) = sec_part.find(':') {
        // mm:ss:xx
        (&sec_part[..colon], &sec_part[colon + 1..])
    } else {
        // mm:ss
        (sec_part, "0")
    };

    let secs: usize = secs_str.trim().parse().ok()?;
    // The fraction is usually centiseconds, but some files use milliseconds (`.123`)
    // or tenths (`.1`), so scale by how many digits there are.
    let frac = centis_str.trim();
    let frac_ms: usize = match frac.len() {
        0 => 0,
        1 => frac.parse::<usize>().unwrap_or(0) * 100,
        2 => frac.parse::<usize>().unwrap_or(0) * 10,
        _ => frac[..3].parse().unwrap_or(0),
    };

    Some(minutes * 60_000 + secs * 1_000 + frac_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_cache_entries_without_plain_lyrics_still_deserialize() {
        let old_format = r#"{"synced_lyrics":[{"time_ms":18520,"text":"hello"}]}"#;
        let parsed: SongLyrics = serde_json::from_str(old_format).unwrap();
        assert_eq!(parsed.synced_lyrics.len(), 1);
        assert!(parsed.plain_lyrics.is_empty());
    }

    #[test]
    fn parses_enhanced_lrc_word_tags() {
        let lrc = "[00:12.00]<00:12.00>Ne<00:12.30>ver <00:12.60>gon<00:12.80>na<00:13.20>
[00:14.00]<00:14.00>give <00:14.50>you <00:15.00>up
[00:20.00]plain line";
        let parsed = parse_lrc(lrc, false);
        let lines = &parsed.synced_lyrics;
        assert_eq!(lines.len(), 3);

        assert_eq!(lines[0].text, "Never gonna");
        let starts: Vec<usize> = lines[0].segments.iter().map(|s| s.start_ms).collect();
        let ends: Vec<usize> = lines[0].segments.iter().map(|s| s.end_ms).collect();
        assert_eq!(starts, vec![12_000, 12_300, 12_600, 12_800]);
        assert_eq!(ends, vec![12_300, 12_600, 12_800, 13_200]);

        // No closing tag: the last word runs until the next line.
        assert_eq!(lines[1].text, "give you up");
        assert_eq!(lines[1].segments.last().unwrap().end_ms, 20_000);

        assert!(lines[2].segments.is_empty());
        assert_eq!(lines[2].text, "plain line");
    }

    #[test]
    fn non_time_angle_brackets_stay_text() {
        let parsed = parse_lrc("[00:01.00]a <3 b", false);
        assert_eq!(parsed.synced_lyrics[0].text, "a <3 b");
        assert!(parsed.synced_lyrics[0].segments.is_empty());
    }

    #[test]
    fn millisecond_fractions() {
        let parsed = parse_lrc("[00:01.234]x\n[00:02.5]y", false);
        assert_eq!(parsed.synced_lyrics[0].time_ms, 1_234);
        assert_eq!(parsed.synced_lyrics[1].time_ms, 2_500);
    }

    #[test]
    fn sung_chars_progress() {
        let parsed = parse_lrc("[00:00.00]<00:00.00>ab<00:01.00>cd<00:02.00>", false);
        let line = &parsed.synced_lyrics[0];
        assert_eq!(line.sung_chars(0), Some(0.0));
        assert_eq!(line.sung_chars(500), Some(1.0));
        assert_eq!(line.sung_chars(1_000), Some(2.0));
        assert_eq!(line.sung_chars(5_000), Some(4.0));
        assert_eq!(
            parse_lrc("[00:00.00]x", false).synced_lyrics[0].sung_chars(1),
            None
        );
    }

    #[test]
    fn segments_round_trip_through_cache_json() {
        let parsed = parse_lrc("[00:00.00]<00:00.00>ab<00:01.00>cd<00:02.00>", false);
        let json = serde_json::to_string(&parsed).unwrap();
        let back: SongLyrics = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.synced_lyrics[0].segments,
            parsed.synced_lyrics[0].segments
        );
        // Lines without timing don't grow a `segments` key in the cache.
        let plain = serde_json::to_string(&parse_lrc("[00:00.00]x", false)).unwrap();
        assert!(!plain.contains("segments"));
    }

    #[test]
    fn parse_rick() {
        let rick: String = "[00:18.92] We're no strangers to love
[00:22.59] You know the rules and so do I (do I)
[00:26.93] A full commitment's what I'm thinking of
[00:31.35] You wouldn't get this from any other guy
[00:35.14] I just wanna tell you how I'm feeling
[00:40.28] Gotta make you understand
[00:42.83] Never gonna give you up
[00:45.22] Never gonna let you down
[00:47.14] Never gonna run around and desert you
[00:51.40] Never gonna make you cry
[00:53.88] Never gonna say goodbye
[00:55.67] Never gonna tell a lie and hurt you
[01:00.52] We've known each other for so long
[01:05.04] Your heart's been aching, but you're too shy to say it (say it)
[01:09.42] Inside, we both know what's been going on (going on)
[01:13.11] We know the game and we're gonna play it
[01:17.29] And if you ask me how I'm feeling
[01:22.51] Don't tell me you're too blind to see
[01:25.33] Never gonna give you up
[01:27.47] Never gonna let you down
[01:29.65] Never gonna run around and desert you
[01:33.42] Never gonna make you cry
[01:35.82] Never gonna say goodbye
[01:37.78] Never gonna tell a lie and hurt you
[01:41.99] Never gonna give you up
[01:44.10] Never gonna let you down
[01:46.43] Never gonna run around and desert you
[01:50.26] Never gonna make you cry
[01:52.56] Never gonna say goodbye
[01:54.79] Never gonna tell a lie and hurt you
[01:59.22] (Ooh, give you up)
[02:02.98] (Ooh, give you up)
[02:07.08] (Ooh) Never gonna give, never gonna give (give you up)
[02:11.26] (Ooh) Never gonna give, never gonna give (give you up)
[02:16.13] We've known each other for so long
[02:20.53] Your heart's been aching, but you're too shy to say it (to say it)
[02:24.65] Inside, we both know what's been going on (going on)
[02:28.87] We know the game and we're gonna play it
[02:32.55] I just wanna tell you how I'm feeling
[02:37.79] Gotta make you understand
[02:40.88] Never gonna give you up
[02:42.94] Never gonna let you down
[02:45.16] Never gonna run around and desert you
[02:49.00] Never gonna make you cry
[02:51.17] Never gonna say goodbye
[02:53.78] Never gonna tell a lie and hurt you
[02:57.61] Never gonna give you up
[02:59.47] Never gonna let you down
[03:02.00] Never gonna run around and desert you
[03:05.95] Never gonna make you cry
[03:08.34] Never gonna say goodbye
[03:10.45] Never gonna tell a lie and hurt you
[03:14.37] Never gonna give you up
[03:16.37] Never gonna let you down
[03:18.84] Never gonna run around and desert you
[03:23.07] Never gonna make you cry
[03:25.17] Never gonna say goodbye
[03:27.38] Never gonna tell a lie and hurt you
[03:30.57]"
            .into();

        let rick_parsed = parse_lrc(&rick, false);
        assert_eq!(rick_parsed.synced_lyrics.len(), 59);

        let rick_parsed_strip = parse_lrc(&rick, true);
        assert_eq!(rick_parsed_strip.synced_lyrics.len(), 58);

        assert_eq!(
            rick_parsed.find_current_index(19111),
            LyricPosition::Line(0)
        );

        assert_eq!(
            rick_parsed.find_current_index(1),
            LyricPosition::BeforeStart
        );

        assert_eq!(
            rick_parsed.find_current_index(1_111_111_111),
            LyricPosition::AfterEnd(rick_parsed.synced_lyrics.len())
        );
    }
}
