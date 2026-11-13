//! libmpv for the embedded player. On Windows it is downloaded from the `mpv-dev` archives of
//! shinchiro's mpv-winbuild-cmake releases, because the normal mpv download has no DLL. macOS
//! and Linux get it from the package manager, so nothing is downloaded there.
use crate::bail;
use crate::error::{Context, Result};
use crate::net;
use crate::tools::{Channel, Installed, Progress, Tools};
use crate::util::unix_now;
use serde::Deserialize;
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub const DLL: &str = "libmpv-2.dll";
pub const RELEASES: &str = "https://github.com/shinchiro/mpv-winbuild-cmake/releases";
const LATEST_API: &str =
    "https://api.github.com/repos/shinchiro/mpv-winbuild-cmake/releases/latest";

/// The architecture name used in the archive names, where a download exists.
fn arch() -> Option<&'static str> {
    cfg!(all(windows, target_arch = "x86_64")).then_some("x86_64")
}

/// Whether BearDL can download libmpv itself on this system.
pub fn downloadable() -> bool {
    arch().is_some()
}

#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(default)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
    /// GitHub's checksum of the file, `sha256:<hex>`.
    pub digest: Option<String>,
}

impl ReleaseAsset {
    pub fn sha256(&self) -> Option<String> {
        let hex = self.digest.as_deref()?.strip_prefix("sha256:")?;
        (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hex.to_ascii_lowercase())
    }
}

/// Picks `mpv-dev-<arch>-<date>-git-<hash>.7z`. This skips the player-only `mpv-<arch>-…`
/// archives and CPU-specific variants such as `mpv-dev-x86_64-v3-…`, which needs AVX2.
pub fn pick_asset<'a>(assets: &'a [ReleaseAsset], arch: &str) -> Option<&'a ReleaseAsset> {
    let prefix = format!("mpv-dev-{arch}-");
    assets
        .iter()
        .filter(|a| {
            a.name.ends_with(".7z")
                && a.name
                    .strip_prefix(&prefix)
                    .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        .max_by(|a, b| a.name.cmp(&b.name))
}

impl Tools {
    /// Downloads the latest `mpv-dev` build and installs its libmpv into [`Tools::libmpv_dir`].
    pub fn install_libmpv(&self, progress: Progress, cancel: &AtomicBool) -> Result<Installed> {
        let Some(arch) = arch() else {
            bail!("BearDL cannot download libmpv for this system")
        };
        progress("Finding the latest libmpv", 0, None);
        let release: Release = serde_json::from_str(&net::fetch_text(LATEST_API)?)
            .context("reading the mpv-winbuild-cmake release")?;
        let asset = pick_asset(&release.assets, arch).context(&format!(
            "the latest mpv-winbuild-cmake release ({}) has no mpv-dev-{arch} archive. \
             Download one from {RELEASES} and choose its {DLL} under Custom libmpv",
            release.tag_name
        ))?;
        if !asset
            .browser_download_url
            .starts_with("https://github.com/")
        {
            bail!("unexpected download address {}", asset.browser_download_url);
        }
        let dl = self.dir.join("mpv.download");
        let sha = self.fetch_libmpv(asset, &dl.join(&asset.name), progress, cancel);
        let _ = std::fs::remove_dir_all(&dl);
        let installed = Installed {
            version: release.tag_name.clone(),
            channel: Channel::Stable,
            asset: asset.name.clone(),
            sha256: sha?,
            installed_at: unix_now(),
        };
        let mut st = self.load_state();
        st.libmpv = Some(installed.clone());
        self.save_state(&st)?;
        Ok(installed)
    }

    /// Downloads and verifies the archive, extracts the DLL, and returns the archive's SHA-256.
    fn fetch_libmpv(
        &self,
        asset: &ReleaseAsset,
        file: &Path,
        progress: Progress,
        cancel: &AtomicBool,
    ) -> Result<String> {
        let size = (asset.size > 0).then_some(asset.size);
        net::download_cancellable(
            &asset.browser_download_url,
            file,
            &|d, t| progress("Downloading libmpv", d, t.or(size)),
            cancel,
        )?;
        let sha = net::sha256_file(file)?;
        if let Some(expected) = asset.sha256()
            && sha != expected
        {
            bail!(
                "checksum mismatch for {}: expected {expected}, got {sha}",
                asset.name
            );
        }
        progress("Extracting libmpv", 0, None);
        let dir = self.libmpv_dir();
        std::fs::create_dir_all(&dir)?;
        extract_dll(
            file,
            &dir.join(DLL),
            &|d, t| progress("Extracting libmpv", d, Some(t)),
            cancel,
        )?;
        Ok(sha)
    }
}

/// Writes the archive's `libmpv-2.dll`, and nothing else, to `dest`. It goes to a `.part` file
/// first and is renamed into place, so `dest` is never a partial file.
#[cfg(windows)]
pub fn extract_dll(
    archive: &Path,
    dest: &Path,
    progress: &dyn Fn(u64, u64),
    cancel: &AtomicBool,
) -> Result<()> {
    use crate::error::Error;
    use sevenz_rust2::{ArchiveReader, EntryDecision, Password};
    use std::io::Write;
    use std::sync::atomic::Ordering;
    let part = dest.with_extension("dll.part");
    let mut reader = ArchiveReader::open(archive, Password::empty())
        .map_err(|e| Error::msg(format!("could not open {}: {e}", archive.display())))?;
    let (mut found, mut cancelled, mut short) = (false, false, false);
    let res = reader.for_each_entries_with_decision(|entry, data| {
        let base = entry.name().rsplit(['/', '\\']).next().unwrap_or("");
        if entry.is_directory() || !base.eq_ignore_ascii_case(DLL) {
            return Ok(EntryDecision::Continue);
        }
        let total = entry.size();
        let mut out = std::fs::File::create(&part)?;
        let mut buf = vec![0u8; 1 << 20];
        let (mut done, mut reported) = (0u64, 0u64);
        loop {
            if cancel.load(Ordering::SeqCst) {
                cancelled = true;
                return Ok(EntryDecision::Stop);
            }
            // Reading to the end checks the entry's CRC.
            let n = data.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            done += n as u64;
            if done - reported >= 4 << 20 {
                reported = done;
                progress(done, total);
            }
        }
        out.sync_all()?;
        short = done != total;
        found = true;
        Ok(EntryDecision::Stop)
    });
    let failure = if cancelled {
        Some("download cancelled".to_owned())
    } else if let Err(e) = res {
        Some(format!("could not extract {DLL}: {e}"))
    } else if !found {
        Some(format!("{} contains no {DLL}", archive.display()))
    } else if short {
        Some(format!("{DLL} in {} is truncated", archive.display()))
    } else {
        None
    };
    if let Some(msg) = failure {
        let _ = std::fs::remove_file(&part);
        return Err(Error(msg));
    }
    replace_file(&part, dest)
}

#[cfg(not(windows))]
pub fn extract_dll(_: &Path, _: &Path, _: &dyn Fn(u64, u64), _: &AtomicBool) -> Result<()> {
    bail!("libmpv archives are only extracted on Windows")
}

/// Renames `part` over `dest`. Windows will not replace a DLL that is loaded, but will rename
/// it, so the old file is moved aside first.
#[cfg(windows)]
fn replace_file(part: &Path, dest: &Path) -> Result<()> {
    use crate::tools::rename_retry;
    let old = dest.with_extension("dll.old");
    let _ = std::fs::remove_file(&old);
    let had = dest.exists();
    if had {
        rename_retry(dest, &old).context("could not move the old libmpv aside")?;
    }
    if let Err(e) = rename_retry(part, dest) {
        if had {
            let _ = rename_retry(&old, dest);
        }
        let _ = std::fs::remove_file(part);
        bail!("could not install libmpv: {e}");
    }
    let _ = std::fs::remove_file(&old);
    Ok(())
}

/// How to get libmpv where BearDL does not download it: what to do, and a command to copy.
pub struct Instructions {
    pub text: &'static str,
    pub command: Option<&'static str>,
}

pub fn instructions() -> Instructions {
    if cfg!(target_os = "macos") {
        Instructions {
            text: "Install mpv with Homebrew. It includes libmpv.",
            command: Some("brew install mpv"),
        }
    } else if cfg!(windows) {
        Instructions {
            text: "Put libmpv-2.dll next to beardl.exe, or choose it under Custom libmpv in Settings.",
            command: None,
        }
    } else {
        static CMD: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
        let cmd = *CMD.get_or_init(|| {
            std::fs::read_to_string("/etc/os-release")
                .ok()
                .and_then(|s| linux_command(&s))
        });
        match cmd {
            Some(c) => Instructions {
                text: "Install libmpv with your package manager:",
                command: Some(c),
            },
            None => Instructions {
                text: "Install your distribution's libmpv package (usually libmpv2 or mpv-libs).",
                command: None,
            },
        }
    }
}

/// The libmpv install command for the distribution described by an `os-release` file.
pub fn linux_command(os_release: &str) -> Option<&'static str> {
    let field = |key: &str| {
        os_release.lines().find_map(|l| {
            let v = l.trim().strip_prefix(key)?.strip_prefix('=')?;
            Some(v.trim().trim_matches(['"', '\'']).to_ascii_lowercase())
        })
    };
    let ids = [field("ID"), field("ID_LIKE")];
    ids.iter()
        .flatten()
        .flat_map(|v| v.split_whitespace())
        .find_map(|id| match id {
            "debian" | "ubuntu" => Some("sudo apt install libmpv2"),
            "fedora" | "rhel" | "centos" => Some("sudo dnf install mpv-libs"),
            "arch" => Some("sudo pacman -S mpv"),
            "suse" | "opensuse" | "opensuse-tumbleweed" | "opensuse-leap" => {
                Some("sudo zypper install libmpv2")
            }
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.into(),
            browser_download_url: format!("https://github.com/x/y/releases/download/t/{name}"),
            size: 1,
            digest: None,
        }
    }

    #[test]
    fn picks_the_dev_archive_for_the_architecture() {
        let names = [
            "ffmpeg-x86_64-git-cd4e96eb3.7z",
            "mpv-x86_64-20261111-git-bcde6b93cc.7z",
            "mpv-dev-x86_64-v3-20261111-git-bcde6b93cc.7z",
            "mpv-dev-x86_64-20261111-git-bcde6b93cc.7z",
            "mpv-dev-aarch64-20261111-git-bcde6b93cc.7z",
            "mpv-dev-i686-20261111-git-bcde6b93cc.7z",
        ];
        let assets: Vec<_> = names.iter().map(|n| asset(n)).collect();
        assert_eq!(
            pick_asset(&assets, "x86_64").unwrap().name,
            "mpv-dev-x86_64-20261111-git-bcde6b93cc.7z"
        );
        assert_eq!(
            pick_asset(&assets, "aarch64").unwrap().name,
            "mpv-dev-aarch64-20261111-git-bcde6b93cc.7z"
        );
        let without: Vec<_> = assets
            .iter()
            .filter(|a| a.name != names[3])
            .cloned()
            .collect();
        assert!(
            pick_asset(&without, "x86_64").is_none(),
            "v3 is not a fallback"
        );
        assert!(pick_asset(&[asset("mpv-dev-x86_64-20261111.zip")], "x86_64").is_none());
        assert!(pick_asset(&[], "x86_64").is_none());
    }

    #[test]
    fn reads_the_github_release_json() {
        let json = r#"{"tag_name":"20261111","assets":[
            {"name":"mpv-dev-x86_64-20261111-git-bcde6b93cc.7z","size":31776374,
             "browser_download_url":"https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20261111/mpv-dev-x86_64-20261111-git-bcde6b93cc.7z",
             "digest":"sha256:273C079B11EA5575822F972756F4C9A71149A5C10130353ABEB8586738938452","uploader":{}},
            {"name":"old.7z","digest":null}]}"#;
        let r: Release = serde_json::from_str(json).unwrap();
        assert_eq!(r.tag_name, "20261111");
        let a = pick_asset(&r.assets, "x86_64").unwrap();
        assert_eq!(a.size, 31776374);
        assert_eq!(
            a.sha256().unwrap(),
            "273c079b11ea5575822f972756f4c9a71149a5c10130353abeb8586738938452"
        );
        assert_eq!(r.assets[1].sha256(), None);
        let md5 = ReleaseAsset {
            digest: Some("md5:abc".into()),
            ..Default::default()
        };
        assert_eq!(md5.sha256(), None);
    }

    #[test]
    fn linux_commands_follow_the_distribution() {
        let ubuntu = "NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n";
        assert_eq!(linux_command(ubuntu), Some("sudo apt install libmpv2"));
        let mint = "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n";
        assert_eq!(linux_command(mint), Some("sudo apt install libmpv2"));
        assert_eq!(
            linux_command("ID=fedora\n"),
            Some("sudo dnf install mpv-libs")
        );
        assert_eq!(
            linux_command("ID=manjaro\nID_LIKE=arch\n"),
            Some("sudo pacman -S mpv")
        );
        assert_eq!(
            linux_command("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n"),
            Some("sudo zypper install libmpv2")
        );
        assert_eq!(linux_command("ID=nixos\n"), None);
        assert_eq!(linux_command(""), None);
    }

    #[cfg(windows)]
    mod archive {
        use super::super::*;
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter};

        fn tmp(name: &str) -> std::path::PathBuf {
            let d =
                std::env::temp_dir().join(format!("beardl-libmpv-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        /// A 7z laid out like `mpv-dev`: headers in a folder, an import library and the DLL.
        fn make(path: &Path, files: &[(&str, &[u8])]) {
            let mut w = ArchiveWriter::create(path).unwrap();
            for (name, data) in files {
                w.push_archive_entry(ArchiveEntry::new_file(name), Some(*data))
                    .unwrap();
            }
            w.finish().unwrap();
        }

        fn dll_bytes() -> Vec<u8> {
            (0..6_000_000u32).map(|i| (i % 251) as u8).collect()
        }

        fn no_cancel() -> AtomicBool {
            AtomicBool::new(false)
        }

        #[test]
        fn extracts_only_the_dll() {
            let d = tmp("only");
            let dll = dll_bytes();
            let a = d.join("mpv-dev.7z");
            make(
                &a,
                &[
                    ("include/mpv/client.h", b"/* header */"),
                    ("libmpv.dll.a", b"import lib"),
                    (DLL, &dll),
                ],
            );
            let out = d.join("mpv");
            std::fs::create_dir_all(&out).unwrap();
            let calls = std::cell::Cell::new(0);
            extract_dll(
                &a,
                &out.join(DLL),
                &|_, _| calls.set(calls.get() + 1),
                &no_cancel(),
            )
            .unwrap();
            assert_eq!(std::fs::read(out.join(DLL)).unwrap(), dll);
            let names: Vec<_> = std::fs::read_dir(&out)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(names, [DLL], "nothing else is written");
            assert!(calls.get() > 0, "progress was reported");

            // Installing again replaces the file.
            make(&a, &[(DLL, b"v2")]);
            extract_dll(&a, &out.join(DLL), &|_, _| {}, &no_cancel()).unwrap();
            assert_eq!(std::fs::read(out.join(DLL)).unwrap(), b"v2");
            std::fs::remove_dir_all(d).unwrap();
        }

        #[test]
        fn failures_leave_no_partial_file() {
            let d = tmp("fail");
            let dest = d.join(DLL);
            std::fs::write(&dest, b"old").unwrap();

            let none = d.join("none.7z");
            make(
                &none,
                &[("libmpv.dll.a", b"x"), ("include/libmpv-2.dll.h", b"y")],
            );
            let e = extract_dll(&none, &dest, &|_, _| {}, &no_cancel()).unwrap_err();
            assert!(e.0.contains("contains no libmpv-2.dll"), "{e}");

            let bad = d.join("bad.7z");
            std::fs::write(&bad, b"not a 7z archive at all").unwrap();
            assert!(extract_dll(&bad, &dest, &|_, _| {}, &no_cancel()).is_err());

            let good = d.join("good.7z");
            make(&good, &[(DLL, &dll_bytes())]);
            let mut corrupt = std::fs::read(&good).unwrap();
            let mid = corrupt.len() / 2;
            corrupt[mid] ^= 0xff;
            let corrupt_path = d.join("corrupt.7z");
            std::fs::write(&corrupt_path, corrupt).unwrap();
            assert!(extract_dll(&corrupt_path, &dest, &|_, _| {}, &no_cancel()).is_err());

            let cancelled = AtomicBool::new(true);
            let e = extract_dll(&good, &dest, &|_, _| {}, &cancelled).unwrap_err();
            assert!(e.0.contains("cancelled"), "{e}");

            assert_eq!(std::fs::read(&dest).unwrap(), b"old", "old file kept");
            assert!(!dest.with_extension("dll.part").exists());
            std::fs::remove_dir_all(d).unwrap();
        }
    }
}
