//! Optional startup timing, enabled with `BEARDL_STARTUP_LOG=<file>`.
//! Each line is `<unix ms> <ms since main> <label>` so an external script can
//! measure from process launch.
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static START: OnceLock<Instant> = OnceLock::new();

pub fn begin() {
    START.get_or_init(Instant::now);
}

pub fn mark(label: &str) {
    let Some(path) = std::env::var_os("BEARDL_STARTUP_LOG") else {
        return;
    };
    let since = START
        .get()
        .map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0);
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{unix} {since:.1} {label}");
    }
}
