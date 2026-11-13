//! Network access through the system `curl`, and archive extraction through `tar`/`unzip`.
//! Using the OS tools keeps a TLS stack out of the binary.
use crate::bail;
use crate::error::{Context, Result};
use crate::util::command;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

fn system32(exe: &str) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let root = std::env::var_os("SystemRoot")?;
    let p = Path::new(&root).join("System32").join(exe);
    p.is_file().then_some(p)
}

fn curl() -> PathBuf {
    system32("curl.exe").unwrap_or_else(|| PathBuf::from("curl"))
}

fn tar() -> PathBuf {
    system32("tar.exe").unwrap_or_else(|| PathBuf::from("tar"))
}

const UA: &str = concat!("BearDL/", env!("CARGO_PKG_VERSION"));

static CANCEL: AtomicBool = AtomicBool::new(false);

/// Aborts running downloads (used at shutdown).
pub fn cancel_all() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Allows downloads again (an engine starting after another shut down).
pub fn reset_cancel() {
    CANCEL.store(false, Ordering::SeqCst);
}

/// Aborts the transfer if it stalls below 1 KiB/s for a minute.
fn curl_base() -> std::process::Command {
    let mut c = command(curl());
    c.args([
        "-fsSL",
        "--retry",
        "2",
        "--connect-timeout",
        "20",
        "--speed-limit",
        "1024",
        "--speed-time",
        "60",
        "-A",
        UA,
    ]);
    c
}

/// For small requests: also cap the total time.
fn curl_quick() -> std::process::Command {
    let mut c = curl_base();
    c.args(["-m", "60"]);
    c
}

fn run_curl(mut c: std::process::Command, what: &str) -> Result<Vec<u8>> {
    let out = c
        .output()
        .context("could not run curl (is it installed?)")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{what} failed: {}", err.trim());
    }
    Ok(out.stdout)
}

pub fn fetch_text(url: &str) -> Result<String> {
    let mut c = curl_quick();
    c.arg(url);
    let body = run_curl(c, &format!("fetching {url}"))?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Follows redirects and returns the final URL.
pub fn resolve_redirect(url: &str) -> Result<String> {
    let mut c = curl_quick();
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    c.args(["-I", "-o", null, "-w", "%{url_effective}", url]);
    let body = run_curl(c, &format!("resolving {url}"))?;
    Ok(String::from_utf8_lossy(&body).trim().to_owned())
}

/// Size reported by the server, if any.
pub fn content_length(url: &str) -> Option<u64> {
    let mut c = curl_quick();
    c.args(["-I", url]);
    let out = run_curl(c, "HEAD").ok()?;
    String::from_utf8_lossy(&out).lines().rev().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| v.trim().parse().ok())?
    })
}

/// Downloads `url` to `dest`, reporting bytes written. Polls the file size while curl runs.
pub fn download(url: &str, dest: &Path, progress: &dyn Fn(u64, Option<u64>)) -> Result<()> {
    let total = content_length(url);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(dest);
    let mut c = curl_base();
    c.arg("-o").arg(dest).arg(url);
    c.stderr(std::process::Stdio::piped());
    let mut child = c.spawn().context("could not run curl (is it installed?)")?;
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(dest);
            bail!("download cancelled");
        }
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    use std::io::Read;
                    let _ = e.read_to_string(&mut err);
                }
                let _ = std::fs::remove_file(dest);
                bail!("download of {url} failed: {}", err.trim());
            }
            break;
        }
        let done = std::fs::metadata(dest).map_or(0, |m| m.len());
        progress(done, total);
        std::thread::sleep(Duration::from_millis(300));
    }
    progress(std::fs::metadata(dest).map_or(0, |m| m.len()), total);
    Ok(())
}

/// Extracts a .zip or .tar.* archive into `dest` (created if missing).
pub fn extract(archive: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let is_zip = archive
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    let mut attempts: Vec<std::process::Command> = Vec::new();
    // bsdtar (Windows, macOS) reads zip; GNU tar (Linux) does not, so try unzip and Python too.
    let mut t = command(tar());
    t.arg("-xf").arg(archive).arg("-C").arg(dest);
    attempts.push(t);
    if is_zip {
        let mut u = command("unzip");
        u.arg("-oq").arg(archive).arg("-d").arg(dest);
        attempts.push(u);
        let mut p = command("python3");
        p.args(["-m", "zipfile", "-e"]).arg(archive).arg(dest);
        attempts.push(p);
    }
    let mut last = String::new();
    for mut c in attempts {
        match c.output() {
            Ok(o) if o.status.success() => return Ok(()),
            Ok(o) => last = String::from_utf8_lossy(&o.stderr).trim().to_owned(),
            Err(e) => last = e.to_string(),
        }
    }
    bail!("could not extract {}: {last}", archive.display())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Finds the hash for `name` in a `sha256sum`-style listing (`<hash>  <name>` or `<hash> *<name>`).
pub fn find_checksum(listing: &str, name: &str) -> Option<String> {
    listing.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let hash = parts.next()?;
        let file = parts.next()?.trim_start_matches('*');
        (file == name && is_sha256(hash)).then(|| hash.to_ascii_lowercase())
    })
}

/// Returns the first SHA-256 hex token in `text` (for single-file `.sha256` files).
pub fn first_checksum(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|t| is_sha256(t))
        .map(str::to_ascii_lowercase)
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &str = "e51e69e89489f199530f13d32a4342da3ddb2cf34842f364603a066db05afbf3  yt-dlp\n\
F5389D3698ECDE4EF06F4E6B1CE0294FE9AC1396228C8990B8D3416D6D177A7D *yt-dlp.exe\n\
fd51b212cf8380c7a52ee08c0d6be7f3c59b2f2c832eaf7b45acfae6eab57419  yt-dlp_win.zip\n";

    #[test]
    fn checksum_lookup() {
        assert_eq!(
            find_checksum(SUMS, "yt-dlp_win.zip").as_deref(),
            Some("fd51b212cf8380c7a52ee08c0d6be7f3c59b2f2c832eaf7b45acfae6eab57419")
        );
        assert_eq!(
            find_checksum(SUMS, "yt-dlp.exe").unwrap(),
            "f5389d3698ecde4ef06f4e6b1ce0294fe9ac1396228c8990b8d3416d6d177a7d"
        );
        assert!(find_checksum(SUMS, "yt-dlp_macos").is_none());
        assert!(find_checksum("nothex  yt-dlp", "yt-dlp").is_none());
        assert_eq!(
            first_checksum(
                "SHA256 (ffmpeg.zip) = e51e69e89489f199530f13d32a4342da3ddb2cf34842f364603a066db05afbf3"
            )
            .unwrap()
            .len(),
            64
        );
    }

    #[test]
    fn hashes_files() {
        let dir = std::env::temp_dir().join(format!("beardl-net-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
