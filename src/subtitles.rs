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

/// Removes `<...>` tags (styling, voice, inline word timestamps) and decodes entities.
pub fn clean_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Turns raw (start, end, lines) blocks into cues. YouTube's automatic captions repeat the
/// previous line at the top of each cue (rolling captions) and add 10 ms transition cues;
/// both are folded so each spoken line is indexed once, at the time it was first shown.
fn finish(raw: Vec<(f64, f64, Vec<String>)>) -> Vec<Cue> {
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
        if end - start < 0.05 {
            previous = lines;
            continue;
        }
        let new: Vec<&String> = lines.iter().filter(|l| !previous.contains(l)).collect();
        if !new.is_empty() {
            let text = new.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
            cues.push(Cue { start, end, text });
        }
        previous = lines;
    }
    cues
}

pub fn parse_vtt(text: &str) -> Vec<Cue> {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut raw = Vec::new();
    for block in text.split("\n\n") {
        let mut lines = block.lines().filter(|l| !l.trim().is_empty());
        let Some(mut first) = lines.next() else {
            continue;
        };
        if first.starts_with("WEBVTT")
            || first.starts_with("NOTE")
            || first.starts_with("STYLE")
            || first.starts_with("REGION")
        {
            continue;
        }
        if !first.contains("-->") {
            // Optional cue identifier.
            match lines.next() {
                Some(l) => first = l,
                None => continue,
            }
        }
        let Some((start, end)) = parse_timing(first) else {
            continue;
        };
        raw.push((start, end, lines.map(str::to_owned).collect()));
    }
    finish(raw)
}

pub fn parse_srt(text: &str) -> Vec<Cue> {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut raw = Vec::new();
    for block in text.split("\n\n") {
        let mut lines = block.lines().filter(|l| !l.trim().is_empty());
        let Some(mut first) = lines.next() else {
            continue;
        };
        if !first.contains("-->") {
            match lines.next() {
                Some(l) => first = l,
                None => continue,
            }
        }
        let Some((start, end)) = parse_timing(first) else {
            continue;
        };
        raw.push((start, end, lines.map(str::to_owned).collect()));
    }
    finish(raw)
}

/// Parses a subtitle file by its extension (vtt or srt). Other formats are skipped.
pub fn parse_file(path: &std::path::Path) -> Option<Vec<Cue>> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    match ext.as_str() {
        "vtt" => Some(parse_vtt(&text)),
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
        let c = parse_vtt(v);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "Hello & welcome");
        assert_eq!(c[0].start, 1.0);
        assert_eq!(c[1].text, "second line wraps here");
        assert_eq!(c[1].end, 5.5);
    }

    #[test]
    fn vtt_youtube_rolling_captions() {
        let v = "WEBVTT\n\n00:00:00.160 --> 00:00:02.150 align:start position:0%\n \nall<00:00:00.480><c> right</c><00:00:00.640><c> so</c>\n\n00:00:02.150 --> 00:00:02.160 align:start position:0%\nall right so\n \n\n00:00:02.160 --> 00:00:04.700 align:start position:0%\nall right so\nhere<00:00:02.400><c> we</c><c> are</c>\n\n00:00:04.700 --> 00:00:04.710 align:start position:0%\nhere we are\n \n\n00:00:04.710 --> 00:00:06.000 align:start position:0%\nhere we are\nat the zoo\n";
        let c = parse_vtt(v);
        let texts: Vec<&str> = c.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["all right so", "here we are", "at the zoo"]);
        assert_eq!(c[1].start, 2.16);
    }

    #[test]
    fn srt_basic() {
        let s = "\u{feff}1\r\n00:00:01,000 --> 00:00:02,500\r\nFirst <i>line</i>\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\nSecond\r\nline\r\n\r\n3\r\nbroken timing\r\ntext\r\n";
        let c = parse_srt(s);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "First line");
        assert_eq!(c[1].text, "Second line");
        assert_eq!(c[1].start, 3.0);
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
}
