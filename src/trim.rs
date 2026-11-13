//! Trimming with ffmpeg: cut a file by start and end time (or a chapter) into a new clip.
use crate::util::format_duration;
use crate::ytdlp::Chapter;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct TrimSpec {
    pub input: PathBuf,
    pub output: PathBuf,
    pub start: f64,
    pub end: f64,
    /// Re-encode for frame-exact cuts; otherwise copy streams and cut at keyframes (instant).
    pub accurate: bool,
}

/// ffmpeg arguments. Progress is written to stdout as `key=value` lines (`-progress pipe:1`).
pub fn args(spec: &TrimSpec) -> Vec<String> {
    let mut a: Vec<String> = [
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-nostats",
        "-progress",
        "pipe:1",
        "-y",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.push("-ss".into());
    a.push(format!("{:.3}", spec.start.max(0.0)));
    a.push("-to".into());
    a.push(format!("{:.3}", spec.end.max(spec.start)));
    a.push("-i".into());
    a.push(spec.input.to_string_lossy().into_owned());
    if spec.accurate {
        // Real video and audio only: cover art (an attached picture) is not re-encoded as video.
        for m in ["0:V?", "0:a?"] {
            a.push("-map".into());
            a.push(m.into());
        }
        a.push("-map_metadata".into());
        a.push("0".into());
        a.push("-map_chapters".into());
        a.push("-1".into());
    } else {
        // Data streams (QuickTime chapter text, timecode) make some muxers refuse a copy.
        a.extend(
            [
                "-map",
                "0",
                "-map",
                "-0:d",
                "-ignore_unknown",
                "-c",
                "copy",
                "-map_chapters",
                "-1",
                "-avoid_negative_ts",
                "make_zero",
            ]
            .map(String::from),
        );
    }
    a.push(spec.output.to_string_lossy().into_owned());
    a
}

/// Seconds of output written so far, from one `-progress` line.
pub fn progress_seconds(line: &str) -> Option<f64> {
    let v = line
        .strip_prefix("out_time_us=")
        .or_else(|| line.strip_prefix("out_time_ms="))?;
    v.trim().parse::<f64>().ok().map(|us| us / 1_000_000.0)
}

/// `1:02:03.5` style time for a file name (no characters invalid on any OS).
fn stamp(t: f64) -> String {
    format_duration(t).replace(':', ".")
}

/// A new file next to `input`: `<name> (clip 1.02–2.30).<ext>`, never overwriting.
/// WebM re-encodes are written as Matroska, which takes ffmpeg's default codecs.
pub fn clip_path(input: &Path, start: f64, end: f64, accurate: bool) -> PathBuf {
    clip_path_avoiding(input, start, end, accurate, &|_| false)
}

/// Like [`clip_path`], also skipping names `taken` reports (outputs of running jobs).
pub fn clip_path_avoiding(
    input: &Path,
    start: f64,
    end: f64,
    accurate: bool,
    taken: &dyn Fn(&Path) -> bool,
) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip".into());
    let mut ext = input
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mkv".into());
    if accurate && ext.eq_ignore_ascii_case("webm") {
        ext = "mkv".into();
    }
    let dir = input.parent().unwrap_or(Path::new("."));
    let base = format!("{stem} (clip {}\u{2013}{})", stamp(start), stamp(end));
    let mut out = dir.join(format!("{base}.{ext}"));
    let mut n = 2;
    while out.exists() || taken(&out) {
        out = dir.join(format!("{base} {n}.{ext}"));
        n += 1;
    }
    out
}

/// The time range of chapter `i`; the last chapter ends at `duration`.
pub fn chapter_range(chapters: &[Chapter], i: usize, duration: Option<f64>) -> Option<(f64, f64)> {
    let c = chapters.get(i)?;
    let end = if c.end.is_finite() && c.end > c.start {
        c.end
    } else {
        chapters
            .get(i + 1)
            .map(|n| n.start)
            .or(duration)
            .unwrap_or(c.start)
    };
    (end > c.start).then_some((c.start, end))
}

pub fn validate(start: f64, end: f64, duration: Option<f64>) -> Result<(), String> {
    if !(start.is_finite() && end.is_finite()) || start < 0.0 {
        return Err("start and end must be times like 1:23 or 83".into());
    }
    if end <= start {
        return Err("the end must be after the start".into());
    }
    if let Some(d) = duration.filter(|d| *d > 0.0)
        && start >= d
    {
        return Err("the start is past the end of the file".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_args() {
        let spec = TrimSpec {
            input: "/v/a.mp4".into(),
            output: "/v/out.mp4".into(),
            start: 1.5,
            end: 10.0,
            accurate: false,
        };
        let a = args(&spec);
        let pos = |s: &str| a.iter().position(|x| x == s).unwrap();
        assert!(pos("-ss") < pos("-i"), "fast seek before input");
        assert_eq!(a[pos("-ss") + 1], "1.500");
        assert_eq!(a[pos("-to") + 1], "10.000");
        assert_eq!(a[pos("-c") + 1], "copy");
        assert_eq!(a.last().unwrap(), "/v/out.mp4");
        let acc = args(&TrimSpec {
            accurate: true,
            ..spec
        });
        assert!(!acc.iter().any(|x| x == "copy"));
        assert!(acc.iter().any(|x| x == "0:V?"));
    }

    #[test]
    fn progress_and_ranges() {
        assert_eq!(progress_seconds("out_time_us=2500000"), Some(2.5));
        assert_eq!(progress_seconds("frame=10"), None);
        let ch = vec![
            Chapter {
                start: 0.0,
                end: f64::NAN,
                title: "A".into(),
            },
            Chapter {
                start: 30.0,
                end: 60.0,
                title: "B".into(),
            },
            Chapter {
                start: 60.0,
                end: f64::NAN,
                title: "C".into(),
            },
        ];
        assert_eq!(chapter_range(&ch, 0, None), Some((0.0, 30.0)));
        assert_eq!(chapter_range(&ch, 1, None), Some((30.0, 60.0)));
        assert_eq!(chapter_range(&ch, 2, Some(90.0)), Some((60.0, 90.0)));
        assert_eq!(chapter_range(&ch, 2, None), None);
        assert!(validate(5.0, 3.0, None).is_err());
        assert!(validate(5.0, 8.0, Some(4.0)).is_err());
        assert!(validate(1.0, 8.0, Some(10.0)).is_ok());
    }

    #[test]
    fn clip_names_are_unique() {
        let d = std::env::temp_dir().join(format!("beardl-trim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let input = d.join("Song [x].webm");
        let a = clip_path(&input, 62.0, 150.0, false);
        assert_eq!(
            a.file_name().unwrap().to_string_lossy(),
            "Song [x] (clip 1.02\u{2013}2.30).webm"
        );
        std::fs::write(&a, "").unwrap();
        let b = clip_path(&input, 62.0, 150.0, false);
        assert!(b.to_string_lossy().ends_with(" 2.webm"));
        assert!(
            clip_path(&input, 0.0, 5.0, true)
                .to_string_lossy()
                .ends_with(".mkv")
        );
        std::fs::remove_dir_all(d).unwrap();
    }
}
