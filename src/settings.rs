use crate::error::Result;
use crate::tools::Channel;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub download_dir: PathBuf,
    /// File name template passed to yt-dlp `-o`.
    pub output_template: String,
    pub concurrency: u32,
    /// Global rate limit such as `5M`; empty for none.
    pub rate_limit: String,
    pub default_preset: String,
    pub subtitle_langs: String,

    pub ytdlp_channel: Channel,
    pub auto_update: bool,
    pub update_interval_hours: u32,
    /// Use this yt-dlp instead of the managed one (disables updates).
    pub ytdlp_path: String,
    /// Use this ffmpeg instead of the managed or system one.
    pub ffmpeg_path: String,
    /// Use this mpv executable for the external player fallback.
    pub mpv_path: String,
    /// Use this libmpv for the embedded player instead of searching for one.
    pub libmpv_path: String,

    pub theme: String,
    pub ui_scale: f32,

    pub start_with_os: bool,
    pub start_minimised: bool,
    pub close_to_tray: bool,
    pub notifications: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: crate::paths::default_download_dir(),
            output_template: "%(title)s [%(id)s].%(ext)s".into(),
            concurrency: 3,
            rate_limit: String::new(),
            default_preset: "Best video + audio".into(),
            subtitle_langs: crate::ytdlp::DEFAULT_SUB_LANGS.into(),
            ytdlp_channel: Channel::Stable,
            auto_update: true,
            update_interval_hours: 24,
            ytdlp_path: String::new(),
            ffmpeg_path: String::new(),
            mpv_path: String::new(),
            libmpv_path: String::new(),
            theme: "Dark".into(),
            ui_scale: 1.0,
            start_with_os: false,
            start_minimised: false,
            // Many Linux desktops show no tray icons, and a hidden window could not come back.
            close_to_tray: !cfg!(target_os = "linux"),
            notifications: true,
        }
    }
}

impl Settings {
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str::<Settings>(&s).ok())
            .unwrap_or_default()
            .sanitised()
    }

    pub fn load() -> Self {
        Self::load_from(&crate::paths::settings_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&crate::paths::settings_path())
    }

    pub fn sanitised(mut self) -> Self {
        // Windows "Copy as path" wraps paths in quotes.
        for p in [
            &mut self.ytdlp_path,
            &mut self.ffmpeg_path,
            &mut self.mpv_path,
            &mut self.libmpv_path,
        ] {
            *p = p.trim().trim_matches('"').trim().to_owned();
        }
        let dir = self.download_dir.to_string_lossy();
        let clean = dir.trim().trim_matches('"').trim().to_owned();
        if clean.is_empty() {
            self.download_dir = crate::paths::default_download_dir();
        } else if clean != dir {
            self.download_dir = clean.into();
        }
        self.concurrency = self.concurrency.clamp(1, 64);
        self.update_interval_hours = self.update_interval_hours.clamp(1, 24 * 30);
        if !(0.5..=3.0).contains(&self.ui_scale) {
            self.ui_scale = 1.0;
        }
        if self.output_template.trim().is_empty() {
            self.output_template = Self::default().output_template;
        }
        self
    }

    /// Per-process yt-dlp rate so that all concurrent downloads together stay under the global limit.
    pub fn per_process_rate(&self) -> Option<u64> {
        crate::util::parse_rate(&self.rate_limit)
            .map(|r| (r / self.concurrency.max(1) as u64).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("beardl-settings-{}", std::process::id()));
        let p = dir.join("settings.toml");
        let s = Settings {
            concurrency: 5,
            rate_limit: "10M".into(),
            ..Default::default()
        };
        s.save_to(&p).unwrap();
        assert_eq!(Settings::load_from(&p), s);
        std::fs::write(&p, "concurrency = 0\nunknown_key = 1\n").unwrap();
        let l = Settings::load_from(&p);
        assert_eq!(l.concurrency, 1);
        assert_eq!(l.theme, "Dark");
        std::fs::write(&p, "not toml [").unwrap();
        assert_eq!(Settings::load_from(&p).concurrency, 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rate_split() {
        let s = Settings {
            concurrency: 4,
            rate_limit: "4M".into(),
            ..Default::default()
        };
        assert_eq!(s.per_process_rate(), Some(1024 * 1024));
        assert_eq!(Settings::default().per_process_rate(), None);
    }
}
