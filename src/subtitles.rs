//! WebVTT and SRT parsing into plain-text cues for the full-text index.
use crate::db::Cue;

/// Parses `[hh:]mm:ss.mmm` (or with a comma) into seconds.
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (h.parse::<f64>().ok()?, m.parse::<f64>().ok()?, s),
        [m, s] => (0.0, m.parse::<f64>().ok()?, s),
        _ => return None,
    };
    let sec: f64 = sec.parse().ok()?;
    (h >= 0.0 && m >= 0.0 && sec >= 0.0).then_some(h * 3600.0 + m * 60.0 + sec)
}

fn parse_timing(line: &str) -> Option<(f64, f64)> {
    let (a, b) = line.split_once("-->")?;
    let start = parse_time(a)?;
    // Cue settings may follow the end time (`align:start position:0%`).
    let end = parse_time(b.split_whitespace().next()?)?;
    Some((start, end))
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let name = &tail[1..end];
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "lrm" | "rlm" => Some('\u{200b}'),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some('\u{200b}') => {}
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Removes markup (`<i>`, `<c.colour>`, `<00:00:01.234>`, `{\an8}`) and decodes entities.
/// A `<` that does not start a tag (as in "a < b") is kept.
pub fn clean_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let tag_start = match c {
            '<' => next.is_some_and(|n| n.is_ascii_alphanumeric() || n == '/' || n == '.'),
            '{' => next == Some('\\'),
            _ => false,
        };
        if tag_start {
            let close = if c == '<' { '>' } else { '}' };
            if let Some(len) = chars[i..].iter().position(|&x| x == close) {
                i += len + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    decode_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Turns raw (start, end, lines) cues into clean cues. For automatic captions, YouTube
/// repeats the previous line at the top of each cue (rolling captions) and adds 10 ms
/// transition cues; with `rolling` both are folded so each spoken line is indexed once.
fn finish(raw: Vec<(f64, f64, Vec<String>)>, rolling: bool) -> Vec<Cue> {
    let mut cues = Vec::new();
    let mut previous: Vec<String> = Vec::new();
    for (start, end, lines) in raw {
        let lines: Vec<String> = lines
            .iter()
            .map(|l| clean_text(l))
            .filter(|l| !l.is_empty())
            .collect();
        if lines.is_empty() {
            continue;
        }
        if rolling && end - start < 0.05 {
            previous = lines;
            continue;
        }
        let new: Vec<&String> = if rolling {
            lines.iter().filter(|l| !previous.contains(l)).collect()
        } else {
            lines.iter().collect()
        };
        if !new.is_empty() {
            let text = new.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
            cues.push(Cue { start, end, text });
        }
        previous = lines;
    }
    cues
}

/// Line-based parser shared by WebVTT and SRT: a timing line starts a cue, a blank line ends
/// its text. Headers, NOTE/STYLE blocks and cue numbers outside cue text are ignored, and a
/// number directly before the next timing line (SRT without blank separators) is dropped.
fn parse_lines(text: &str) -> Vec<(f64, f64, Vec<String>)> {
    let text = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut raw: Vec<(f64, f64, Vec<String>)> = Vec::new();
    let mut in_text = false;
    for line in text.lines() {
        if line.contains("-->")
            && let Some((start, end)) = parse_timing(line)
        {
            if in_text
                && let Some(last) = raw.last_mut()
                && last.2.last().is_some_and(|l| {
                    !l.trim().is_empty() && l.trim().bytes().all(|b| b.is_ascii_digit())
                })
            {
                last.2.pop();
            }
            raw.push((start, end, Vec::new()));
            in_text = true;
            continue;
        }
        if line.trim().is_empty() {
            // YouTube cues open with a line of spaces; only a blank after text ends a cue.
            let has_text = raw
                .last()
                .is_some_and(|c| c.2.iter().any(|l| !l.trim().is_empty()));
            if has_text {
                in_text = false;
            }
            continue;
        }
        if in_text && let Some(last) = raw.last_mut() {
            last.2.push(line.to_owned());
        }
    }
    raw
}

pub fn parse_vtt(text: &str, rolling: bool) -> Vec<Cue> {
    finish(parse_lines(text), rolling)
}

pub fn parse_srt(text: &str) -> Vec<Cue> {
    finish(parse_lines(text), false)
}

/// Parses a subtitle file by its extension (vtt or srt). Other formats are skipped.
/// `auto` marks automatic captions, whose rolling lines are folded.
pub fn parse_file(path: &std::path::Path, auto: bool) -> Option<Vec<Cue>> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    match ext.as_str() {
        "vtt" => Some(parse_vtt(&text, auto)),
        "srt" => Some(parse_srt(&text)),
        _ => None,
    }
}

/// Builds an FTS5 query for spoken-word search. A quoted input is an exact phrase;
/// otherwise every word must appear (as a prefix).
pub fn cue_query(text: &str) -> Option<String> {
    let t = text.trim();
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| *c != '"' && !c.is_control())
            .collect::<String>()
    };
    if t.len() > 2 && t.starts_with('"') && t.ends_with('"') {
        let inner = clean(&t[1..t.len() - 1]);
        let inner = inner.trim();
        return (!inner.is_empty()).then(|| format!("\"{inner}\""));
    }
    crate::db::fts_query(t)
}

/// Picks one track per base language (`en`, `en-US` and `en-orig` are all `en`): manual
/// subtitles first, then the original-language automatic track. `(lang, path, is_auto)`.
pub fn choose_tracks<P: Clone>(tracks: &[(String, P, bool)]) -> Vec<(String, P, bool)> {
    let base = |l: &str| l.split('-').next().unwrap_or(l).to_ascii_lowercase();
    let score = |l: &str, auto: bool| -> u8 {
        match (auto, l.ends_with("-orig")) {
            (false, _) => 0,
            (true, true) => 1,
            (true, false) => 2,
        }
    };
    let mut best: Vec<(String, P, bool)> = Vec::new();
    for t in tracks {
        match best.iter_mut().find(|b| base(&b.0) == base(&t.0)) {
            Some(b) if score(&t.0, t.2) < score(&b.0, b.2) => *b = t.clone(),
            Some(_) => {}
            None => best.push(t.clone()),
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("00:01:02.500"), Some(62.5));
        assert_eq!(parse_time("01:02.250"), Some(62.25));
        assert_eq!(parse_time("1:00:00,000"), Some(3600.0));
        assert_eq!(parse_time("x"), None);
        assert_eq!(
            parse_timing("00:00:01.000 --> 00:00:04.000 align:start position:0%"),
            Some((1.0, 4.0))
        );
    }

    #[test]
    fn vtt_basic_and_tags() {
        let v = "WEBVTT\nKind: captions\nLanguage: en\n\nNOTE a comment\n\n1\n00:00:01.000 --> 00:00:03.000\n<v Bob>Hello &amp; <b>welcome</b></v>\n\n00:00:03.000 --> 00:00:05.500\nsecond   line\nwraps here\n";
        let c = parse_vtt(v, false);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "Hello & welcome");
        assert_eq!(c[0].start, 1.0);
        assert_eq!(c[1].text, "second line wraps here");
        assert_eq!(c[1].end, 5.5);
    }

    #[test]
    fn vtt_youtube_rolling_captions() {
        let v = "WEBVTT\n\n00:00:00.160 --> 00:00:02.150 align:start position:0%\n \nall<00:00:00.480><c> right</c><00:00:00.640><c> so</c>\n\n00:00:02.150 --> 00:00:02.160 align:start position:0%\nall right so\n \n\n00:00:02.160 --> 00:00:04.700 align:start position:0%\nall right so\nhere<00:00:02.400><c> we</c><c> are</c>\n\n00:00:04.700 --> 00:00:04.710 align:start position:0%\nhere we are\n \n\n00:00:04.710 --> 00:00:06.000 align:start position:0%\nhere we are\nat the zoo\n";
        let c = parse_vtt(v, true);
        let texts: Vec<&str> = c.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["all right so", "here we are", "at the zoo"]);
        assert_eq!(c[1].start, 2.16);
    }

    #[test]
    fn manual_repeats_are_kept() {
        let v = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nYes.\n\n00:00:02.000 --> 00:00:03.000\nYes.\n";
        assert_eq!(parse_vtt(v, false).len(), 2);
        assert_eq!(parse_vtt(v, true).len(), 1);
    }

    #[test]
    fn srt_basic_and_messy() {
        let s = "\u{feff}1\r\n00:00:01,000 --> 00:00:02,500\r\nFirst <i>line</i>\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\nSecond\r\nline\r\n\r\n3\r\nbroken timing\r\ntext\r\n";
        let c = parse_srt(s);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "First line");
        assert_eq!(c[1].text, "Second line");
        assert_eq!(c[1].start, 3.0);
        // Whitespace-only separators, no separators at all, and lone CRs.
        let messy = "1\r00:00:01,000 --> 00:00:02,000\rOne\r   \r2\r00:00:02,000 --> 00:00:03,000\rTwo\r3\r00:00:03,000 --> 00:00:04,000\r{\\an8}Three &#8217;s &lt;x&gt; a < b\r";
        let c = parse_srt(messy);
        let texts: Vec<&str> = c.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["One", "Two", "Three \u{2019}s <x> a < b"]);
    }

    #[test]
    fn queries() {
        assert_eq!(
            cue_query("\"at the zoo\"").as_deref(),
            Some("\"at the zoo\"")
        );
        assert_eq!(cue_query("zoo ele").as_deref(), Some("\"zoo\"* \"ele\"*"));
        assert_eq!(cue_query("\"\""), None);
        assert_eq!(cue_query("  "), None);
    }

    #[test]
    fn track_choice() {
        let t = |l: &str, auto: bool| (l.to_owned(), l.to_owned(), auto);
        let chosen = choose_tracks(&[
            t("en", true),
            t("en-orig", true),
            t("en-US", false),
            t("de-orig", true),
            t("de", true),
        ]);
        let langs: Vec<&str> = chosen.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(langs, vec!["en-US", "de-orig"]);
    }
}
