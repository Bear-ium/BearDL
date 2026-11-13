//! yt-dlp and ffmpeg maintenance jobs run on the blocking pool.
use super::{Core, Done, Job, Level, Shared, ToolsView};
use crate::tools::{Channel, Tools};
use crate::util::unix_now;

impl Core {
    /// Re-reads installed tools and their versions.
    pub(super) async fn refresh_tools(&mut self) {
        let tools = self.tools.clone();
        let (yo, fo) = (
            self.settings.ytdlp_path.clone(),
            self.settings.ffmpeg_path.clone(),
        );
        let res = tokio::task::spawn_blocking(move || {
            let paths = tools.resolve(&yo, &fo);
            let st = tools.load_state();
            let ytdlp_managed = paths.ytdlp.is_some() && paths.ytdlp == tools.managed_ytdlp();
            let ffmpeg_managed = paths.ffmpeg.is_some() && paths.ffmpeg == tools.managed_ffmpeg();
            let ytdlp_version = st
                .ytdlp
                .as_ref()
                .filter(|_| ytdlp_managed)
                .map(|i| i.version.clone())
                .or_else(|| {
                    paths
                        .ytdlp
                        .as_ref()
                        .and_then(|p| crate::tools::exe_version(p, "--version"))
                });
            let ffmpeg_version = st
                .ffmpeg
                .as_ref()
                .filter(|_| ffmpeg_managed)
                .map(|i| i.version.clone())
                .or_else(|| {
                    paths.ffmpeg.as_ref().and_then(|p| {
                        crate::tools::exe_version(p, "-version")
                            .map(|l| crate::tools::short_ffmpeg_version(&l))
                    })
                });
            let view = ToolsView {
                ytdlp_version,
                ytdlp_channel: st.ytdlp.as_ref().map(|i| i.channel),
                ytdlp_managed,
                ytdlp_prev: tools
                    .has_prev("yt-dlp")
                    .then(|| st.ytdlp_prev.as_ref().map(|i| i.version.clone()))
                    .flatten(),
                ffmpeg_version,
                ffmpeg_managed,
                ffmpeg_prev: tools
                    .has_prev("ffmpeg")
                    .then(|| st.ffmpeg_prev.as_ref().map(|i| i.version.clone()))
                    .flatten(),
                last_check: st.last_check,
                ..Default::default()
            };
            (paths, view)
        })
        .await;
        if let Ok((paths, view)) = res {
            self.shared.update(|s| {
                let old = std::mem::take(&mut s.tools);
                s.tools = ToolsView {
                    ytdlp_latest: old.ytdlp_latest,
                    ffmpeg_latest: old.ffmpeg_latest,
                    busy: old.busy,
                    error: old.error,
                    last_check: view.last_check.max(old.last_check),
                    ..view
                };
                s.paths = paths;
            });
        }
    }

    pub(super) fn spawn_tool_job(&mut self, job: ToolJob) {
        // Replacing yt-dlp while it runs fails on Windows, so wait for downloads to finish.
        // Subscription checks run yt-dlp too.
        let wait_for_downloads =
            job.touches_ytdlp() && (!self.running.is_empty() || !self.subs_checking.is_empty());
        if self.tools_busy.is_some() || wait_for_downloads {
            if !self.tool_queue.contains(&job) {
                self.tool_queue.push_back(job);
                if wait_for_downloads {
                    self.shared.update(|s| {
                        s.notify(
                            Level::Info,
                            "yt-dlp will update when the running downloads finish",
                        )
                    });
                }
            }
            return;
        }
        self.tools_busy = Some(job);
        let tools = self.tools.clone();
        let shared = self.shared.clone();
        let done = self.done_tx.clone();
        let channel = self.settings.ytdlp_channel;
        let check_ffmpeg = self.shared.lock().tools.ffmpeg_managed;
        // Automatic checks only speak up when there is something new.
        let quiet = self.pending_auto;
        shared.update(|s| {
            if matches!(job, ToolJob::Check) {
                // Count the attempt, so an offline check retries after the interval, not at once.
                s.tools.last_check = unix_now();
            }
            s.tools.error = None;
            s.tools.busy = Some(Job {
                label: job.label().into(),
                ..Default::default()
            });
        });
        tokio::task::spawn_blocking(move || {
            let progress = |label: &str, d: u64, t: Option<u64>| {
                shared.update(|s| {
                    s.tools.busy = Some(Job {
                        label: label.to_owned(),
                        done: d,
                        total: t,
                    })
                });
            };
            let result = job.run(&tools, channel, check_ffmpeg, quiet, &progress, &shared);
            shared.update(|s| {
                s.tools.busy = None;
                match result {
                    Ok(Some(msg)) => s.notify(Level::Info, msg),
                    Ok(None) => {}
                    Err(e) => {
                        s.tools.error = Some(e.to_string());
                        s.notify(Level::Error, format!("{}: {e}", job.label()));
                    }
                }
            });
            let _ = done.send(Done::Tools);
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum ToolJob {
    Check,
    InstallYtdlp,
    RollbackYtdlp,
    InstallFfmpeg,
    RollbackFfmpeg,
}

impl ToolJob {
    pub(super) fn touches_ytdlp(self) -> bool {
        matches!(self, ToolJob::InstallYtdlp | ToolJob::RollbackYtdlp)
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            ToolJob::Check => "Checking for updates",
            ToolJob::InstallYtdlp => "Updating yt-dlp",
            ToolJob::RollbackYtdlp => "Rolling back yt-dlp",
            ToolJob::InstallFfmpeg => "Installing ffmpeg",
            ToolJob::RollbackFfmpeg => "Rolling back ffmpeg",
        }
    }

    fn run(
        self,
        tools: &Tools,
        channel: Channel,
        check_ffmpeg: bool,
        quiet: bool,
        progress: crate::tools::Progress,
        shared: &Shared,
    ) -> crate::error::Result<Option<String>> {
        match self {
            ToolJob::Check => {
                let info = tools.check_ytdlp(channel)?;
                let mut st = tools.load_state();
                st.last_check = unix_now();
                tools.save_state(&st)?;
                let ff = if check_ffmpeg {
                    match tools.check_ffmpeg() {
                        Ok(i) => Some(i),
                        Err(e) => {
                            shared.update(|s| {
                                s.notify(Level::Error, format!("ffmpeg update check: {e}"))
                            });
                            None
                        }
                    }
                } else {
                    None
                };
                let msg = if info.available {
                    Some(format!("yt-dlp {} is available", info.latest))
                } else {
                    (!quiet).then(|| "yt-dlp is up to date".to_owned())
                };
                shared.update(|s| {
                    s.tools.ytdlp_latest = Some(info);
                    s.tools.ffmpeg_latest = ff;
                    s.tools.last_check = st.last_check;
                });
                Ok(msg)
            }
            ToolJob::InstallYtdlp => {
                let i = tools.install_ytdlp(channel, progress)?;
                shared.update(|s| {
                    if let Some(l) = s.tools.ytdlp_latest.as_mut() {
                        l.available = false;
                    }
                });
                Ok(Some(format!(
                    "yt-dlp {} ({}) installed",
                    i.version,
                    i.channel.label()
                )))
            }
            ToolJob::RollbackYtdlp => {
                tools.rollback_ytdlp()?;
                Ok(Some("Rolled back to the previous yt-dlp".into()))
            }
            ToolJob::InstallFfmpeg => {
                let i = tools.install_ffmpeg(progress)?;
                shared.update(|s| s.tools.ffmpeg_latest = None);
                Ok(Some(format!("ffmpeg {} installed", i.version)))
            }
            ToolJob::RollbackFfmpeg => {
                tools.rollback_ffmpeg()?;
                Ok(Some("Rolled back to the previous ffmpeg".into()))
            }
        }
    }
}
