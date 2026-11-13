//! Tool manager for yt-dlp and ffmpeg: install, verify, update and roll back.
//!
//! Layout under the tools dir:
//! `yt-dlp/` (current), `yt-dlp.prev/` (rollback), `ffmpeg/`, `ffmpeg.prev/`, `state.toml`.
use crate::bail;
use crate::error::{Context, Result};
use crate::net;
use crate::util::{command, unix_now};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug, Default, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    #[default]
    Stable,
    Nightly,
    Master,
}

impl Channel {
    pub const ALL: [Channel; 3] = [Channel::Stable, Channel::Nightly, Channel::Master];

    pub fn repo(self) -> &'static str {
        match self {
            Channel::Stable => "yt-dlp/yt-dlp",
            Channel::Nightly => "yt-dlp/yt-dlp-nightly-builds",
            Channel::Master => "yt-dlp/yt-dlp-master-builds",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Channel::Stable => "Stable",
            Channel::Nightly => "Nightly",
            Channel::Master => "Master",
        }
    }

    fn download_base(self) -> String {
        format!(
            "https://github.com/{}/releases/latest/download",
            self.repo()
        )
    }
}

/// A yt-dlp release asset. Archives are PyInstaller "onedir" builds, which start far faster than
/// the single-file builds because nothing is unpacked on each launch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Asset {
    pub name: &'static str,
    pub archive: bool,
}

/// Assets in order of preference for this platform.
pub fn ytdlp_assets() -> &'static [Asset] {
    macro_rules! a {
        ($name:literal, $archive:literal) => {
            Asset {
                name: $name,
                archive: $archive,
            }
        };
    }
    if cfg!(windows) {
        if cfg!(target_arch = "aarch64") {
            &[
                a!("yt-dlp_win_arm64.zip", true),
                a!("yt-dlp_arm64.exe", false),
            ]
        } else if cfg!(target_arch = "x86") {
            &[a!("yt-dlp_win_x86.zip", true), a!("yt-dlp_x86.exe", false)]
        } else {
            &[a!("yt-dlp_win.zip", true), a!("yt-dlp.exe", false)]
        }
    } else if cfg!(target_os = "macos") {
        &[a!("yt-dlp_macos.zip", true), a!("yt-dlp_macos", false)]
    } else if cfg!(target_arch = "aarch64") {
        &[
            a!("yt-dlp_linux_aarch64.zip", true),
            a!("yt-dlp_linux_aarch64", false),
        ]
    } else {
        &[a!("yt-dlp_linux.zip", true), a!("yt-dlp_linux", false)]
    }
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Installed {
    pub version: String,
    pub channel: Channel,
    pub asset: String,
    pub sha256: String,
    pub installed_at: i64,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ToolState {
    pub ytdlp: Option<Installed>,
    pub ytdlp_prev: Option<Installed>,
    pub ffmpeg: Option<Installed>,
    pub ffmpeg_prev: Option<Installed>,
    /// Downloaded libmpv (Windows). `version` is the release tag, `sha256` the archive's.
    pub libmpv: Option<Installed>,
    pub last_check: i64,
    /// Checksum of a yt-dlp build the user rolled back from.
    pub rejected_sha: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpdateInfo {
    pub latest: String,
    pub available: bool,
}

/// Paths to the tools the rest of the app should run, after applying settings overrides.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolPaths {
    pub ytdlp: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
}

impl ToolPaths {
    /// Directory to pass to yt-dlp's `--ffmpeg-location`.
    pub fn ffmpeg_dir(&self) -> Option<PathBuf> {
        self.ffmpeg
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
    }
}

pub fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_owned()
    }
}

/// Searches `PATH` for an executable.
pub fn which(base: &str) -> Option<PathBuf> {
    let name = exe_name(base);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&name))
        .find(|p| p.is_file())
}

/// First line of `<exe> <arg>` output, if it runs.
/// Gives up after 30 s so a hung or wrong binary cannot block the caller.
pub fn exe_version(exe: &Path, arg: &str) -> Option<String> {
    use std::io::Read;
    let mut child = command(exe)
        .arg(arg)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        if let Some(st) = child.try_wait().ok()? {
            break st;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut out = Vec::new();
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    if !status.success() {
        return None;
    }
    String::from_utf8_lossy(&out)
        .lines()
        .next()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
}

/// Shortens `ffmpeg version N-1234-gabc Copyright ...` to `N-1234-gabc`.
pub fn short_ffmpeg_version(line: &str) -> String {
    line.strip_prefix("ffmpeg version ")
        .unwrap_or(line)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_owned()
}

/// Extracts the tag from a `.../releases/tag/<tag>` URL.
pub fn tag_from_url(url: &str) -> Option<String> {
    let (_, tag) = url.rsplit_once("/tag/")?;
    let tag = tag.trim_end_matches('/');
    (!tag.is_empty()).then(|| tag.to_owned())
}

pub struct Tools {
    pub dir: PathBuf,
}

pub type Progress<'a> = &'a dyn Fn(&str, u64, Option<u64>);

impl Tools {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("state.toml")
    }

    pub fn load_state(&self) -> ToolState {
        std::fs::read_to_string(self.state_path())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_state(&self, st: &ToolState) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.state_path().with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(st)?)?;
        std::fs::rename(tmp, self.state_path())?;
        Ok(())
    }

    pub fn ytdlp_dir(&self) -> PathBuf {
        self.dir.join("yt-dlp")
    }

    pub fn ffmpeg_dir(&self) -> PathBuf {
        self.dir.join("ffmpeg")
    }

    pub fn has_prev(&self, tool: &str) -> bool {
        self.dir.join(format!("{tool}.prev")).is_dir()
    }

    /// The managed yt-dlp executable, if installed.
    pub fn managed_ytdlp(&self) -> Option<PathBuf> {
        find_ytdlp_exe(&self.ytdlp_dir())
    }

    pub fn managed_ffmpeg(&self) -> Option<PathBuf> {
        let p = self.ffmpeg_dir().join(exe_name("ffmpeg"));
        p.is_file().then_some(p)
    }

    /// Where the downloaded libmpv goes; the player also looks here.
    pub fn libmpv_dir(&self) -> PathBuf {
        self.dir.join("mpv")
    }

    pub fn managed_libmpv(&self) -> Option<PathBuf> {
        let p = self.libmpv_dir().join(crate::libmpv::DLL);
        p.is_file().then_some(p)
    }

    /// Resolves which binaries to run: settings override, then managed copy, then `PATH`.
    pub fn resolve(&self, ytdlp_override: &str, ffmpeg_override: &str) -> ToolPaths {
        let ov = |s: &str| {
            let s = s.trim();
            (!s.is_empty()).then(|| PathBuf::from(s))
        };
        let ytdlp = ov(ytdlp_override)
            .or_else(|| self.managed_ytdlp())
            .or_else(|| which("yt-dlp"));
        let ffmpeg = ov(ffmpeg_override)
            .or_else(|| self.managed_ffmpeg())
            .or_else(|| which("ffmpeg"));
        let ffprobe = ffmpeg.as_ref().and_then(|f| {
            let p = f.with_file_name(exe_name("ffprobe"));
            p.is_file().then_some(p).or_else(|| which("ffprobe"))
        });
        ToolPaths {
            ytdlp,
            ffmpeg,
            ffprobe,
        }
    }

    /// Resolves the latest release tag once, so checksums and assets come from the same release
    /// even on channels that publish often.
    fn latest_release(&self, channel: Channel) -> Result<(String, String, String)> {
        let tag = net::resolve_redirect(&format!(
            "https://github.com/{}/releases/latest",
            channel.repo()
        ))
        .ok()
        .and_then(|u| tag_from_url(&u));
        let base = match &tag {
            Some(t) => format!(
                "https://github.com/{}/releases/download/{t}",
                channel.repo()
            ),
            None => channel.download_base(),
        };
        let sums = net::fetch_text(&format!("{base}/SHA2-256SUMS"))?;
        Ok((tag.unwrap_or_default(), base, sums))
    }

    /// Asks GitHub for the latest release on `channel` and compares its checksum with the installed one.
    pub fn check_ytdlp(&self, channel: Channel) -> Result<UpdateInfo> {
        let (latest, _, sums) = self.latest_release(channel)?;
        let (_, expected) = pick_asset(&sums)?;
        let st = self.load_state();
        let rejected = !st.rejected_sha.is_empty() && st.rejected_sha == expected;
        let available = self.managed_ytdlp().is_none()
            || match &st.ytdlp {
                Some(i) => i.channel != channel || (i.sha256 != expected && !rejected),
                None => true,
            };
        Ok(UpdateInfo {
            latest: if latest.is_empty() {
                "a new build".into()
            } else {
                latest
            },
            available,
        })
    }

    /// Downloads the latest yt-dlp for `channel`, verifies it, and swaps it in. The previous
    /// install is kept in `yt-dlp.prev` for rollback. Falls back to the single-file build when
    /// the archive cannot be extracted on this system.
    pub fn install_ytdlp(&self, channel: Channel, progress: Progress) -> Result<Installed> {
        progress("Fetching checksums", 0, None);
        let (_, base, sums) = self.latest_release(channel)?;
        let candidates: Vec<(Asset, String)> = ytdlp_assets()
            .iter()
            .filter_map(|a| net::find_checksum(&sums, a.name).map(|h| (*a, h)))
            .collect();
        if candidates.is_empty() {
            bail!("no yt-dlp build for this platform in the release");
        }
        let mut last_err = None;
        for (asset, expected) in candidates {
            match self.install_asset(channel, &base, asset, &expected, progress) {
                Ok(i) => return Ok(i),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| crate::error::Error::msg("install failed")))
    }

    fn install_asset(
        &self,
        channel: Channel,
        base: &str,
        asset: Asset,
        expected: &str,
        progress: Progress,
    ) -> Result<Installed> {
        let dl = self.dir.join("download");
        let file = dl.join(asset.name);
        net::download(&format!("{base}/{}", asset.name), &file, &|d, t| {
            progress("Downloading yt-dlp", d, t)
        })?;
        verify(&file, expected)?;
        progress("Installing yt-dlp", 0, None);
        let staging = self.dir.join("yt-dlp.new");
        let _ = std::fs::remove_dir_all(&staging);
        let unpacked = if asset.archive {
            net::extract(&file, &staging)
        } else {
            std::fs::create_dir_all(&staging)
                .and_then(|_| std::fs::rename(&file, staging.join(exe_name("yt-dlp"))))
                .map_err(Into::into)
        };
        let _ = std::fs::remove_dir_all(&dl);
        if let Err(e) = unpacked {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
        let exe = find_ytdlp_exe(&staging).context("yt-dlp executable missing from download")?;
        make_executable(&exe)?;
        let Some(version) = exe_version(&exe, "--version") else {
            let _ = std::fs::remove_dir_all(&staging);
            bail!("the downloaded yt-dlp does not run on this system");
        };
        let installed = Installed {
            version,
            channel,
            asset: asset.name.to_owned(),
            sha256: expected.to_owned(),
            installed_at: unix_now(),
        };
        rotate(&staging, &self.ytdlp_dir(), &self.dir.join("yt-dlp.prev"))?;
        let mut st = self.load_state();
        st.ytdlp_prev = st.ytdlp.take();
        st.ytdlp = Some(installed.clone());
        st.rejected_sha.clear();
        st.last_check = unix_now();
        self.save_state(&st)?;
        Ok(installed)
    }

    /// Restores the previous yt-dlp. The build rolled back from is remembered so automatic
    /// updates do not reinstall it; a newer release clears this.
    pub fn rollback_ytdlp(&self) -> Result<()> {
        swap_prev(&self.ytdlp_dir(), &self.dir.join("yt-dlp.prev"))?;
        let mut st = self.load_state();
        std::mem::swap(&mut st.ytdlp, &mut st.ytdlp_prev);
        st.rejected_sha = st
            .ytdlp_prev
            .as_ref()
            .map(|i| i.sha256.clone())
            .unwrap_or_default();
        self.save_state(&st)
    }

    pub fn check_ffmpeg(&self) -> Result<UpdateInfo> {
        let src = ffmpeg_source()?;
        let first = src.files.first().context("no ffmpeg asset")?;
        let (_, sha) = resolve_ffmpeg_file(first)?;
        let st = self.load_state();
        let available = st.ffmpeg.as_ref().is_none_or(|i| i.sha256 != sha);
        Ok(UpdateInfo {
            latest: src.label.to_owned(),
            available,
        })
    }

    /// Downloads a static ffmpeg + ffprobe build into the tools dir.
    pub fn install_ffmpeg(&self, progress: Progress) -> Result<Installed> {
        let src = ffmpeg_source()?;
        let dl = self.dir.join("download");
        let unpack = self.dir.join("ffmpeg.unpack");
        let staging = self.dir.join("ffmpeg.new");
        let _ = std::fs::remove_dir_all(&unpack);
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        let mut first_sha = String::new();
        for f in src.files {
            progress("Fetching checksums", 0, None);
            let (url, sha) = resolve_ffmpeg_file(f)?;
            if first_sha.is_empty() {
                first_sha = sha.clone();
            }
            let file = dl.join(f.file_name);
            net::download(&url, &file, &|d, t| progress("Downloading ffmpeg", d, t))?;
            verify(&file, &sha)?;
            progress("Extracting ffmpeg", 0, None);
            net::extract(&file, &unpack)?;
            let _ = std::fs::remove_file(&file);
        }
        for bin in ["ffmpeg", "ffprobe"] {
            let name = exe_name(bin);
            let found = find_file(&unpack, &name, 4).context(&format!("{name} not in archive"))?;
            std::fs::rename(&found, staging.join(&name))?;
            make_executable(&staging.join(&name))?;
        }
        let _ = std::fs::remove_dir_all(&unpack);
        let _ = std::fs::remove_dir_all(&dl);
        let line = exe_version(&staging.join(exe_name("ffmpeg")), "-version")
            .context("the downloaded ffmpeg does not run on this system")?;
        let installed = Installed {
            version: short_ffmpeg_version(&line),
            channel: Channel::Stable,
            asset: src.label.to_owned(),
            sha256: first_sha,
            installed_at: unix_now(),
        };
        rotate(&staging, &self.ffmpeg_dir(), &self.dir.join("ffmpeg.prev"))?;
        let mut st = self.load_state();
        st.ffmpeg_prev = st.ffmpeg.take();
        st.ffmpeg = Some(installed.clone());
        self.save_state(&st)?;
        Ok(installed)
    }

    pub fn rollback_ffmpeg(&self) -> Result<()> {
        swap_prev(&self.ffmpeg_dir(), &self.dir.join("ffmpeg.prev"))?;
        let mut st = self.load_state();
        std::mem::swap(&mut st.ffmpeg, &mut st.ffmpeg_prev);
        self.save_state(&st)
    }
}

/// Picks the most preferred asset listed in a checksum file.
fn pick_asset(sums: &str) -> Result<(Asset, String)> {
    for a in ytdlp_assets() {
        if let Some(h) = net::find_checksum(sums, a.name) {
            return Ok((*a, h));
        }
    }
    bail!("no yt-dlp build for this platform in the release")
}

fn verify(file: &Path, expected: &str) -> Result<()> {
    let got = net::sha256_file(file)?;
    if !got.eq_ignore_ascii_case(expected) {
        let _ = std::fs::remove_file(file);
        bail!(
            "checksum mismatch for {}: expected {expected}, got {got}",
            file.display()
        );
    }
    Ok(())
}

/// The yt-dlp executable in an install dir: a file whose name starts with `yt-dlp`.
pub fn find_ytdlp_exe(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.starts_with("yt-dlp")
                && !n.ends_with(".zip")
                && (!cfg!(windows) || n.ends_with(".exe"))
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut dirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            dirs.push(p);
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    dirs.into_iter()
        .find_map(|d| find_file(&d, name, depth - 1))
}

fn make_executable(_p: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(_p, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Renames with a few retries: antivirus scanners briefly lock freshly written files on Windows.
pub(crate) fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last = None;
    for _ in 0..10 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("rename failed")))
}

/// Moves `new` into `current`, keeping the old `current` as `prev`. The old `prev` is only
/// deleted once the swap has succeeded, so a failure leaves the previous state intact.
pub fn rotate(new: &Path, current: &Path, prev: &Path) -> Result<()> {
    let old_prev = prev.with_extension("old");
    let _ = std::fs::remove_dir_all(&old_prev);
    let had_prev = prev.exists();
    if had_prev {
        rename_retry(prev, &old_prev).context("could not move the old backup aside")?;
    }
    let restore_prev = || {
        if had_prev {
            let _ = rename_retry(&old_prev, prev);
        }
    };
    let had_current = current.exists();
    if had_current && let Err(e) = rename_retry(current, prev) {
        restore_prev();
        bail!("could not move the current version aside (is it running?): {e}");
    }
    if let Err(e) = rename_retry(new, current) {
        if had_current {
            let _ = rename_retry(prev, current);
        }
        restore_prev();
        bail!("could not install the new version: {e}");
    }
    if had_prev {
        if had_current {
            let _ = std::fs::remove_dir_all(&old_prev);
        } else {
            restore_prev();
        }
    }
    Ok(())
}

/// Swaps `current` and `prev`.
pub fn swap_prev(current: &Path, prev: &Path) -> Result<()> {
    if !prev.is_dir() {
        bail!("no previous version to roll back to");
    }
    let tmp = current.with_extension("swap");
    let _ = std::fs::remove_dir_all(&tmp);
    if current.exists() {
        rename_retry(current, &tmp)
            .context("could not move the current version (is it running?)")?;
    }
    if let Err(e) = rename_retry(prev, current) {
        let _ = rename_retry(&tmp, current);
        bail!("rollback failed: {e}");
    }
    if tmp.exists() {
        rename_retry(&tmp, prev)?;
    }
    Ok(())
}

pub struct FfmpegFile {
    pub file_name: &'static str,
    /// Direct URL, or a redirect that resolves to the versioned file.
    pub url: &'static str,
    /// Checksum list URL (`sha256sum` format). `None` means `<resolved url>.sha256`.
    pub sums_url: Option<&'static str>,
}

pub struct FfmpegSource {
    pub label: &'static str,
    pub files: &'static [FfmpegFile],
}

fn ffmpeg_source() -> Result<FfmpegSource> {
    const YT: &str =
        "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/checksums.sha256";
    macro_rules! yt {
        ($name:literal) => {
            FfmpegFile {
                file_name: $name,
                url: concat!(
                    "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/",
                    $name
                ),
                sums_url: Some(YT),
            }
        };
    }
    let src = if cfg!(all(windows, target_arch = "x86_64")) {
        FfmpegSource {
            label: "yt-dlp FFmpeg-Builds (win64)",
            files: &[yt!("ffmpeg-master-latest-win64-gpl.zip")],
        }
    } else if cfg!(all(windows, target_arch = "aarch64")) {
        FfmpegSource {
            label: "yt-dlp FFmpeg-Builds (winarm64)",
            files: &[yt!("ffmpeg-master-latest-winarm64-gpl.zip")],
        }
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        FfmpegSource {
            label: "yt-dlp FFmpeg-Builds (linux64)",
            files: &[yt!("ffmpeg-master-latest-linux64-gpl.tar.xz")],
        }
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        FfmpegSource {
            label: "yt-dlp FFmpeg-Builds (linuxarm64)",
            files: &[yt!("ffmpeg-master-latest-linuxarm64-gpl.tar.xz")],
        }
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        FfmpegSource {
            label: "martin-riedl.de (macOS arm64)",
            files: &[
                FfmpegFile {
                    file_name: "ffmpeg.zip",
                    url: "https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffmpeg.zip",
                    sums_url: None,
                },
                FfmpegFile {
                    file_name: "ffprobe.zip",
                    url: "https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffprobe.zip",
                    sums_url: None,
                },
            ],
        }
    } else if cfg!(target_os = "macos") {
        FfmpegSource {
            label: "martin-riedl.de (macOS x86_64)",
            files: &[
                FfmpegFile {
                    file_name: "ffmpeg.zip",
                    url: "https://ffmpeg.martin-riedl.de/redirect/latest/macos/amd64/release/ffmpeg.zip",
                    sums_url: None,
                },
                FfmpegFile {
                    file_name: "ffprobe.zip",
                    url: "https://ffmpeg.martin-riedl.de/redirect/latest/macos/amd64/release/ffprobe.zip",
                    sums_url: None,
                },
            ],
        }
    } else {
        bail!(
            "no ffmpeg build available for this platform; install ffmpeg with your package manager"
        )
    };
    Ok(src)
}

/// Returns the concrete download URL and its expected checksum.
fn resolve_ffmpeg_file(f: &FfmpegFile) -> Result<(String, String)> {
    match f.sums_url {
        Some(sums) => {
            let listing = net::fetch_text(sums)?;
            let sha = net::find_checksum(&listing, f.file_name)
                .context(&format!("no checksum for {}", f.file_name))?;
            Ok((f.url.to_owned(), sha))
        }
        None => {
            let url = net::resolve_redirect(f.url)?;
            let sha = net::first_checksum(&net::fetch_text(&format!("{url}.sha256"))?)
                .context(&format!("no checksum for {}", f.file_name))?;
            Ok((url, sha))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("beardl-tools-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn picks_preferred_asset() {
        let names: String = ytdlp_assets()
            .iter()
            .rev()
            .map(|a| format!("{}  {}\n", "a".repeat(64), a.name))
            .collect();
        let (a, _) = pick_asset(&names).unwrap();
        assert_eq!(a, ytdlp_assets()[0]);
        assert!(a.archive);
        let only_single = format!("{}  {}\n", "b".repeat(64), ytdlp_assets()[1].name);
        assert!(!pick_asset(&only_single).unwrap().0.archive);
        assert!(pick_asset("").is_err());
    }

    #[test]
    fn tags_and_versions() {
        assert_eq!(
            tag_from_url("https://github.com/yt-dlp/yt-dlp/releases/tag/2026.11.10").as_deref(),
            Some("2026.11.10")
        );
        assert_eq!(
            tag_from_url("https://github.com/yt-dlp/yt-dlp/releases"),
            None
        );
        assert_eq!(
            short_ffmpeg_version("ffmpeg version N-12345-gabc Copyright (c) 2000-2026"),
            "N-12345-gabc"
        );
    }

    #[test]
    fn rotate_and_rollback() {
        let d = tmp("rotate");
        let (cur, prev, new) = (d.join("cur"), d.join("prev"), d.join("new"));
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("v"), "1").unwrap();
        rotate(&new, &cur, &prev).unwrap();
        assert_eq!(std::fs::read_to_string(cur.join("v")).unwrap(), "1");
        assert!(!prev.exists());
        assert!(swap_prev(&cur, &prev).is_err());

        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("v"), "2").unwrap();
        rotate(&new, &cur, &prev).unwrap();
        assert_eq!(std::fs::read_to_string(cur.join("v")).unwrap(), "2");
        assert_eq!(std::fs::read_to_string(prev.join("v")).unwrap(), "1");

        swap_prev(&cur, &prev).unwrap();
        assert_eq!(std::fs::read_to_string(cur.join("v")).unwrap(), "1");
        assert_eq!(std::fs::read_to_string(prev.join("v")).unwrap(), "2");

        // A failed install (missing staging dir) keeps both current and backup.
        assert!(rotate(&new, &cur, &prev).is_err());
        assert_eq!(std::fs::read_to_string(cur.join("v")).unwrap(), "1");
        assert_eq!(std::fs::read_to_string(prev.join("v")).unwrap(), "2");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn finds_executables_and_state() {
        let d = tmp("find");
        let yd = d.join("yt-dlp");
        std::fs::create_dir_all(yd.join("_internal")).unwrap();
        std::fs::write(yd.join(exe_name("yt-dlp_x")), "").unwrap();
        std::fs::write(yd.join("yt-dlp.zip"), "").unwrap();
        assert!(find_ytdlp_exe(&yd).unwrap().ends_with(exe_name("yt-dlp_x")));
        std::fs::create_dir_all(d.join("a/b/bin")).unwrap();
        std::fs::write(d.join("a/b/bin/ffmpeg"), "").unwrap();
        assert!(find_file(&d.join("a"), "ffmpeg", 4).is_some());
        assert!(find_file(&d.join("a"), "ffmpeg", 1).is_none());

        let t = Tools::new(d.clone());
        assert_eq!(t.load_state(), ToolState::default());
        let st = ToolState {
            ytdlp: Some(Installed {
                version: "2026.11.10".into(),
                channel: Channel::Nightly,
                ..Default::default()
            }),
            last_check: 5,
            ..Default::default()
        };
        t.save_state(&st).unwrap();
        assert_eq!(t.load_state(), st);
        assert!(t.managed_ytdlp().is_some());
        let paths = t.resolve("", "/custom/ffmpeg");
        assert_eq!(paths.ffmpeg.as_deref(), Some(Path::new("/custom/ffmpeg")));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Network test: downloads real binaries. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_install_and_rollback() {
        let d = tmp("live");
        let t = Tools::new(d.clone());
        let p = |_: &str, _: u64, _: Option<u64>| {};
        let a = t.install_ytdlp(Channel::Stable, &p).unwrap();
        assert!(!a.version.is_empty());
        assert!(!t.check_ytdlp(Channel::Stable).unwrap().available);
        assert!(t.check_ytdlp(Channel::Nightly).unwrap().available);
        let b = t.install_ytdlp(Channel::Nightly, &p).unwrap();
        assert_eq!(t.load_state().ytdlp_prev, Some(a.clone()));
        t.rollback_ytdlp().unwrap();
        assert_eq!(t.load_state().rejected_sha, b.sha256);
        assert_eq!(t.load_state().ytdlp, Some(a));
        assert_eq!(t.load_state().ytdlp_prev, Some(b));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    #[ignore]
    fn live_ffmpeg_install() {
        let d = tmp("liveff");
        let t = Tools::new(d.clone());
        let i = t.install_ffmpeg(&|_, _, _| {}).unwrap();
        assert!(!i.version.is_empty());
        assert!(t.managed_ffmpeg().is_some());
        assert!(t.ffmpeg_dir().join(exe_name("ffprobe")).is_file());
        assert!(!t.check_ffmpeg().unwrap().available);
        std::fs::remove_dir_all(d).unwrap();
    }
}
