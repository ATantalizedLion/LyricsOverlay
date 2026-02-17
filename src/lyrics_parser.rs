#[derive(Debug, Clone)]
pub struct LyricLine {
    time_ms: u64,
    text: String,
}

fn parse_lrc(content: &str, strip_empty_lines: bool) -> Vec<LyricLine> {
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

                if let Some(ms) = parse_time_tag(tag) {
                    let text = rest.to_string();
                    if strip_empty_lines && text.is_empty() {
                        break;
                    }
                    lines.push(LyricLine { time_ms: ms, text });
                    break;
                }
                // Otherwise it's a metadata tag, skip
            } else {
                break;
            }
        }
    }

    lines.sort_by_key(|l| l.time_ms);
    lines
}

fn parse_time_tag(tag: &str) -> Option<u64> {
    // mm:ss.xx  or  mm:ss:xx  or  mm:ss
    let parts: Vec<&str> = tag.splitn(2, ':').collect();
    if parts.len() != 2 {
        return None;
    }
    let minutes: u64 = parts[0].trim().parse().ok()?;

    let sec_part = parts[1];
    let (secs_str, centis_str) = if let Some(dot) = sec_part.find('.') {
        (&sec_part[..dot], &sec_part[dot + 1..])
    } else if let Some(colon) = sec_part.find(':') {
        // mm:ss:xx style
        (&sec_part[..colon], &sec_part[colon + 1..])
    } else {
        (sec_part, "0")
    };

    let secs: u64 = secs_str.trim().parse().ok()?;
    let centis: u64 = centis_str.trim().parse().unwrap_or(0);

    Some(minutes * 60_000 + secs * 1_000 + centis * 10)
}

#[derive(PartialEq, Eq, Debug, Clone)]
pub enum LyricPosition {
    BeforeStart,
    Line(usize),
    AfterEnd,
}
pub fn find_current_index(lyrics: &[LyricLine], elapsed_ms: u64) -> LyricPosition {
    let mut lyric_pos = LyricPosition::BeforeStart;

    if lyrics.is_empty() {
        return lyric_pos;
    }

    for (i, line) in lyrics.iter().enumerate() {
        if line.time_ms <= elapsed_ms {
            lyric_pos = LyricPosition::Line(i);
        } else {
            return lyric_pos;
        }
    }

    LyricPosition::AfterEnd
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(rick_parsed.len(), 59);

        let rick_parsed_strip = parse_lrc(&rick, true);
        assert_eq!(rick_parsed_strip.len(), 58);

        assert_eq!(
            find_current_index(&rick_parsed, 19111),
            LyricPosition::Line(0)
        );

        assert_eq!(
            find_current_index(&rick_parsed, 1),
            LyricPosition::BeforeStart
        );

        assert_eq!(
            find_current_index(&rick_parsed, 1_111_111_111),
            LyricPosition::AfterEnd
        );
    }
}
