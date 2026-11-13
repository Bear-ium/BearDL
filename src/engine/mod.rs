//! Background engine: one thread running a current-thread tokio runtime. The UI sends
//! [`Cmd`]s; the engine updates [`State`] behind a mutex and wakes the UI. When nothing is
//! happening it sleeps until the next scheduled task.
use crate::settings::Settings;
use crate::tools::{Channel, ToolPaths, Tools, UpdateInfo};
use crate::util::unix_now;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::mpsc;

pub type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug)]
pub enum Cmd {
    CheckTools { manual: bool },
    UpdateYtdlp,
    RollbackYtdlp,
    InstallFfmpeg,
    RollbackFfmpeg,
    ApplySettings(Box<Settings>),
    Shutdown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Level {
    Info,
    Error,
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub level: Level,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Job {
    pub label: String,
    pub done: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct ToolsView {
    pub ytdlp_version: Option<String>,
    pub ytdlp_channel: Option<Channel>,
    pub ytdlp_managed: bool,
    pub ytdlp_prev: Option<String>,
    pub ytdlp_latest: Option<UpdateInfo>,
    pub ffmpeg_version: Option<String>,
    pub ffmpeg_managed: bool,
    pub ffmpeg_prev: Option<String>,
    pub ffmpeg_latest: Option<UpdateInfo>,
    pub last_check: i64,
    pub busy: Option<Job>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct State {
    pub tools: ToolsView,
    pub paths: ToolPaths,
    pub notices: Vec<Notice>,
}

impl State {
    pub fn notify(&mut self, level: Level, text: impl Into<String>) {
        self.notices.push(Notice {
            level,
            text: text.into(),
        });
    }
}

pub struct Shared {
    state: Mutex<State>,
    waker: Waker,
}

impl Shared {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn wake(&self) {
        (self.waker)();
    }

    /// Mutates state and wakes the UI.
    pub fn update<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let r = f(&mut self.lock());
        self.wake();
        r
    }
}

pub struct Engine {
    tx: mpsc::UnboundedSender<Cmd>,
    pub shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Engine {
    pub fn start(settings: Settings, tools_dir: PathBuf, waker: Waker) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            waker,
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("engine".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                rt.block_on(Core::new(settings, Tools::new(tools_dir), sh, rx).run());
                // Blocking tool jobs notice the cancel flag; do not wait on them for long.
                crate::net::cancel_all();
                rt.shutdown_timeout(std::time::Duration::from_secs(2));
            })
            .expect("engine thread");
        Self {
            tx,
            shared,
            thread: Some(thread),
        }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.shared.lock()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

enum Done {
    Tools,
}

struct Core {
    settings: Settings,
    tools: Arc<Tools>,
    shared: Arc<Shared>,
    rx: mpsc::UnboundedReceiver<Cmd>,
    done_tx: mpsc::UnboundedSender<Done>,
    done_rx: mpsc::UnboundedReceiver<Done>,
    tools_busy: bool,
    tool_queue: std::collections::VecDeque<ToolJob>,
    /// Auto-install the update found by the current check.
    pending_auto: bool,
}

impl Core {
    fn new(
        settings: Settings,
        tools: Tools,
        shared: Arc<Shared>,
        rx: mpsc::UnboundedReceiver<Cmd>,
    ) -> Self {
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        Self {
            settings,
            tools: Arc::new(tools),
            shared,
            rx,
            done_tx,
            done_rx,
            tools_busy: false,
            tool_queue: Default::default(),
            pending_auto: false,
        }
    }

    async fn run(mut self) {
        self.refresh_tools().await;
        if self.ytdlp_overridden() {
            // A user-supplied yt-dlp is never updated by the app.
        } else if self.tools.managed_ytdlp().is_none() && self.shared.lock().paths.ytdlp.is_none() {
            self.spawn_tool_job(ToolJob::InstallYtdlp);
        } else if self.settings.auto_update {
            self.pending_auto = true;
            self.spawn_tool_job(ToolJob::Check);
        }
        // First run: ffmpeg is needed for merging, conversion, thumbnails and trimming.
        if self.shared.lock().paths.ffmpeg.is_none() && self.settings.ffmpeg_path.trim().is_empty()
        {
            self.spawn_tool_job(ToolJob::InstallFfmpeg);
        }
        loop {
            let next = self.next_update_check();
            let sleep = tokio::time::sleep_until(next);
            tokio::select! {
                cmd = self.rx.recv() => match cmd {
                    None | Some(Cmd::Shutdown) => break,
                    Some(cmd) => self.handle(cmd).await,
                },
                Some(done) = self.done_rx.recv() => match done {
                    Done::Tools => {
                        self.tools_busy = false;
                        self.refresh_tools().await;
                        self.after_check();
                        if let Some(job) = self.tool_queue.pop_front() {
                            self.spawn_tool_job(job);
                        }
                    }
                },
                _ = sleep => {
                    if self.settings.auto_update && !self.tools_busy && !self.ytdlp_overridden() {
                        self.pending_auto = true;
                        self.spawn_tool_job(ToolJob::Check);
                    } else {
                        // Push the next check forward so the loop does not spin.
                        self.shared.lock().tools.last_check = unix_now();
                    }
                }
            }
        }
    }

    fn ytdlp_overridden(&self) -> bool {
        !self.settings.ytdlp_path.trim().is_empty()
    }

    fn next_update_check(&self) -> tokio::time::Instant {
        let last = self.shared.lock().tools.last_check;
        let interval = self.settings.update_interval_hours.max(1) as i64 * 3600;
        let due = (last + interval - unix_now()).clamp(60, interval);
        tokio::time::Instant::now() + std::time::Duration::from_secs(due as u64)
    }

    /// After an automatic check, install the update if one was found.
    fn after_check(&mut self) {
        if !self.pending_auto {
            return;
        }
        let avail = self
            .shared
            .lock()
            .tools
            .ytdlp_latest
            .as_ref()
            .is_some_and(|l| l.available);
        if avail {
            self.spawn_tool_job(ToolJob::InstallYtdlp);
        }
        self.pending_auto = false;
    }

    async fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::CheckTools { manual } => {
                self.pending_auto = !manual && self.settings.auto_update;
                self.spawn_tool_job(ToolJob::Check);
            }
            Cmd::UpdateYtdlp => self.spawn_tool_job(ToolJob::InstallYtdlp),
            Cmd::RollbackYtdlp => self.spawn_tool_job(ToolJob::RollbackYtdlp),
            Cmd::InstallFfmpeg => self.spawn_tool_job(ToolJob::InstallFfmpeg),
            Cmd::RollbackFfmpeg => self.spawn_tool_job(ToolJob::RollbackFfmpeg),
            Cmd::ApplySettings(s) => {
                let tools_changed = s.ytdlp_path != self.settings.ytdlp_path
                    || s.ffmpeg_path != self.settings.ffmpeg_path;
                let channel_changed = s.ytdlp_channel != self.settings.ytdlp_channel;
                self.settings = *s;
                if tools_changed {
                    self.refresh_tools().await;
                }
                if channel_changed && !self.ytdlp_overridden() {
                    self.shared.update(|s| s.tools.ytdlp_latest = None);
                    self.spawn_tool_job(ToolJob::Check);
                }
            }
            Cmd::Shutdown => {}
        }
    }

    /// Re-reads installed tools and their versions.
    async fn refresh_tools(&mut self) {
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

    fn spawn_tool_job(&mut self, job: ToolJob) {
        if self.tools_busy {
            if !self.tool_queue.contains(&job) {
                self.tool_queue.push_back(job);
            }
            return;
        }
        self.tools_busy = true;
        let tools = self.tools.clone();
        let shared = self.shared.clone();
        let done = self.done_tx.clone();
        let channel = self.settings.ytdlp_channel;
        let check_ffmpeg = self.shared.lock().tools.ffmpeg_managed;
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
            let result = job.run(&tools, channel, check_ffmpeg, &progress, &shared);
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
enum ToolJob {
    Check,
    InstallYtdlp,
    RollbackYtdlp,
    InstallFfmpeg,
    RollbackFfmpeg,
}

impl ToolJob {
    fn label(self) -> &'static str {
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
                    format!("yt-dlp {} is available", info.latest)
                } else {
                    "yt-dlp is up to date".to_owned()
                };
                shared.update(|s| {
                    s.tools.ytdlp_latest = Some(info);
                    s.tools.ffmpeg_latest = ff;
                    s.tools.last_check = st.last_check;
                });
                Ok(Some(msg))
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
