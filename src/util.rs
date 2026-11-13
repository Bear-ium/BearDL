use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A `Command` that never flashes a console window on Windows.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Async variant of [`command`]. Children are killed when their handle drops.
pub fn async_command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(program);
    c.kill_on_drop(true);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    #[cfg(unix)]
    c.process_group(0);
    c
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// UTC `YYYY-MM-DD HH:MM`.
pub fn format_timestamp(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    let (y, m, d) = civil_from_days(ts.div_euclid(86_400));
    let s = ts.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", s / 3600, s % 3600 / 60)
}

/// yt-dlp's `YYYYMMDD` to `YYYY-MM-DD`.
pub fn format_upload_date(d: &str) -> String {
    if d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..])
    } else {
        d.to_owned()
    }
}

pub fn format_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// `H:MM:SS` or `M:SS`.
pub fn format_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return String::new();
    }
    let s = secs as u64;
    let (h, m, s) = (s / 3600, s % 3600 / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Parses `90`, `1:30`, `1:02:03` and `1:02:03.5` into seconds.
pub fn parse_duration(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total = 0.0;
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    for p in &parts {
        let v: f64 = p.trim().parse().ok()?;
        if v < 0.0 {
            return None;
        }
        total = total * 60.0 + v;
    }
    Some(total)
}

/// Parses a rate such as `500K`, `2.5M` or `1G` (bytes per second, 1024-based) as yt-dlp does.
pub fn parse_rate(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, mult) = match s.chars().last()?.to_ascii_uppercase() {
        'K' => (&s[..s.len() - 1], 1024.0),
        'M' => (&s[..s.len() - 1], 1024.0 * 1024.0),
        'G' => (&s[..s.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (s, 1.0),
    };
    let v: f64 = num.trim().parse().ok()?;
    (v > 0.0).then_some((v * mult) as u64)
}

/// Splits a command line into arguments, honouring single and double quotes and backslash escapes
/// (outside single quotes). Returns `None` on an unterminated quote.
pub fn split_args(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => cur.push(ch),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\')) => cur.push(chars.next()?),
                        ch => cur.push(ch),
                    }
                }
            }
            '\\' if matches!(chars.peek(), Some(' ' | '"' | '\'' | '\\')) => {
                in_word = true;
                cur.push(chars.next()?);
            }
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    Some(out)
}

/// Opens a file or folder with the system handler.
pub fn open_path(p: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    let mut c = command("explorer");
    #[cfg(target_os = "macos")]
    let mut c = command("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut c = command("xdg-open");
    c.arg(p).spawn().map(|_| ())
}

/// Opens the containing folder with the file selected where the platform supports it.
pub fn reveal_path(p: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut arg = std::ffi::OsString::from("/select,\"");
        arg.push(p.as_os_str());
        arg.push("\"");
        command("explorer").raw_arg(arg).spawn().map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        command("open").arg("-R").arg(p).spawn().map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        open_path(p.parent().unwrap_or(p))
    }
}

/// Replaces characters that are invalid in file names on any supported platform.
pub fn sanitise_file_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let t = cleaned.trim().trim_end_matches('.');
    if t.is_empty() {
        "untitled".into()
    } else {
        t.chars().take(180).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for days in [-1000, 0, 1, 59, 60, 365, 11_016, 20_000, 30_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(format_timestamp(1_700_000_000), "2023-11-14 22:13");
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(59.9), "0:59");
        assert_eq!(format_duration(3723.0), "1:02:03");
        assert_eq!(parse_duration("1:02:03"), Some(3723.0));
        assert_eq!(parse_duration("90"), Some(90.0));
        assert_eq!(parse_duration("1:30.5"), Some(90.5));
        assert_eq!(parse_duration("x"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn rates_and_bytes() {
        assert_eq!(parse_rate("500K"), Some(512_000));
        assert_eq!(parse_rate("2m"), Some(2 * 1024 * 1024));
        assert_eq!(parse_rate("100"), Some(100));
        assert_eq!(parse_rate("0"), None);
        assert_eq!(parse_rate("abc"), None);
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
    }

    #[test]
    fn splitting() {
        assert_eq!(
            split_args(r#"--a "b c" 'd e' f\ g "h\"i""#).unwrap(),
            vec!["--a", "b c", "d e", "f g", "h\"i"]
        );
        assert_eq!(split_args("  ").unwrap(), Vec::<String>::new());
        assert_eq!(split_args(r#"--x """#).unwrap(), vec!["--x", ""]);
        assert!(split_args("'open").is_none());
        assert_eq!(split_args(r"C:\path\x").unwrap(), vec![r"C:\path\x"]);
    }

    #[test]
    fn sanitising() {
        assert_eq!(sanitise_file_name("a/b:c?"), "a_b_c_");
        assert_eq!(sanitise_file_name(" ..."), "untitled");
        assert_eq!(format_upload_date("20240131"), "2024-01-31");
    }
}
