//! Background engine: one thread running a current-thread tokio runtime. The UI sends
//! [`Cmd`]s; the engine updates [`State`] behind a mutex and wakes the UI. When nothing is
//! happening it sleeps until the next scheduled task.
mod downloads;
mod subscriptions;
mod tooljobs;
mod trims;

pub use downloads::{ItemProgress, NewItem, QueueItem};
pub use subscriptions::{SubView, name_from_url};
pub use trims::TrimJob;

use crate::db::Db;
use crate::settings::Settings;
use crate::tools::{Channel, ToolPaths, Tools, UpdateInfo};
use crate::util::unix_now;
use crate::ytdlp::{Analysis, Preset};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tooljobs::ToolJob;

/// Asks the UI to repaint after the given delay (zero means now).
pub type Waker = Arc<dyn Fn(Duration) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveTo {
    Up,
    Down,
    Top,
    Bottom,
}

#[derive(Debug)]
pub enum Cmd {
    CheckTools {
        manual: bool,
    },
    UpdateYtdlp,
    RollbackYtdlp,
    InstallFfmpeg,
    RollbackFfmpeg,
    ApplySettings(Box<Settings>),
    Analyse(String),
    ClearAnalysis,
    Enqueue(Vec<NewItem>),
    /// Queue URLs with a preset; playlists and channels are expanded first.
    QuickAdd {
        urls: Vec<String>,
        template: NewItem,
    },
    ScanFiles,
    DeleteEntries {
        ids: Vec<i64>,
        delete_files: bool,
    },
    Redownload(i64),
    Pause(i64),
    Resume(i64),
    Cancel(i64),
    Retry(i64),
    Remove(i64),
    Move(i64, MoveTo),
    PauseAll,
    ResumeAll,
    ClearFinished,
    SavePreset {
        old_name: Option<String>,
        preset: Preset,
    },
    DeletePreset(String),
    ValidateArgs(String),
    /// Playback position to resume from next time.
    SaveResume(i64, f64),
    AddSubscription {
        name: String,
        url: String,
        config: crate::subs::SubConfig,
    },
    UpdateSubscription {
        id: i64,
        name: String,
        url: String,
        config: crate::subs::SubConfig,
    },
    DeleteSubscription(i64),
    PauseSubscription(i64, bool),
    CheckSubscription(i64),
    CheckAllSubscriptions,
    /// Cut `[start, end]` seconds of a library item into a new clip.
    Trim {
        id: i64,
        start: f64,
        end: f64,
        accurate: bool,
    },
    Export {
        path: PathBuf,
        settings: Box<Settings>,
    },
    Import {
        path: PathBuf,
        mode: crate::export::ImportMode,
        current: Box<Settings>,
    },
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

#[derive(Clone, Debug, Default)]
pub struct AnalysisView {
    pub url: String,
    pub busy: bool,
    pub result: Option<Result<Arc<Analysis>, String>>,
}

#[derive(Default)]
pub struct State {
    pub tools: ToolsView,
    pub paths: ToolPaths,
    pub notices: Vec<Notice>,
    pub queue: Vec<QueueItem>,
    pub presets: Vec<Preset>,
    pub analysis: Option<AnalysisView>,
    /// Result of the last custom-argument validation: (args, outcome).
    pub validation: Option<(String, Option<Result<(), String>>)>,
    /// Bumped whenever the library changes, so views know to re-query.
    pub library_rev: u64,
    /// Quick downloads still being expanded into items.
    pub expanding: usize,
    /// Settings read from an import, for the UI to adopt (it owns settings).
    pub imported_settings: Option<Settings>,
    /// Bumped when theme files change on disk (after an import).
    pub themes_rev: u64,
    pub subscriptions: Vec<SubView>,
    /// Desktop notifications for the UI to show (title, body).
    pub desktop_notices: Vec<(String, String)>,
    pub trims: Vec<TrimJob>,
}

impl State {
    pub fn notify(&mut self, level: Level, text: impl Into<String>) {
        self.notices.push(Notice {
            level,
            text: text.into(),
        });
    }

    pub fn item_mut(&mut self, id: i64) -> Option<&mut QueueItem> {
        self.queue.iter_mut().find(|q| q.id == id)
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
        (self.waker)(Duration::ZERO);
    }

    /// Coalesced wake for frequent updates such as progress: at most a few repaints per second.
    pub fn wake_soon(&self) {
        (self.waker)(Duration::from_millis(150));
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

pub struct EngineConfig {
    pub settings: Settings,
    pub tools_dir: PathBuf,
    pub db_path: PathBuf,
    pub thumbs_dir: PathBuf,
    pub subs_dir: PathBuf,
    pub themes_dir: PathBuf,
}

impl Engine {
    pub fn start(cfg: EngineConfig, waker: Waker) -> Self {
        crate::net::reset_cancel();
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
                rt.block_on(Core::new(cfg, sh, rx).run());
                // Blocking tool jobs notice the cancel flag; do not wait on them for long.
                crate::net::cancel_all();
                rt.shutdown_timeout(Duration::from_secs(2));
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
    Download(i64, u64, downloads::Outcome),
    Analysis(u64, Result<Arc<Analysis>, String>),
    Validation(String, Result<(), String>),
    Expanded(String, Result<Vec<NewItem>, String>),
    Scan(Result<Vec<downloads::ScanChange>, String>),
    SubChecked(
        i64,
        Result<(Vec<crate::ytdlp::Entry>, Option<String>), String>,
    ),
    Trimmed(trims::TrimResult),
}

/// A running download: its run number (to tell restarts apart) and its stop signal.
struct Running {
    run: u64,
    stop: oneshot::Sender<StopReason>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopReason {
    Pause,
    Cancel,
}

struct Core {
    settings: Settings,
    tools: Arc<Tools>,
    shared: Arc<Shared>,
    db: Option<Db>,
    thumbs_dir: PathBuf,
    subs_dir: PathBuf,
    themes_dir: PathBuf,
    rx: mpsc::UnboundedReceiver<Cmd>,
    done_tx: mpsc::UnboundedSender<Done>,
    done_rx: mpsc::UnboundedReceiver<Done>,
    tools_busy: Option<ToolJob>,
    tool_queue: VecDeque<ToolJob>,
    /// Auto-install the update found by the current check.
    pending_auto: bool,
    running: HashMap<i64, Running>,
    next_run: u64,
    /// Items whose process was told to stop but has not exited yet.
    stopping: HashSet<i64>,
    /// Final file names of paused items, to clean up partial files if they are cancelled.
    partials: HashMap<i64, String>,
    /// Items to remove from the queue once their process has stopped.
    remove_after_stop: Vec<i64>,
    analysis_gen: u64,
    analysis_task: Option<tokio::task::JoinHandle<()>>,
    warned_no_ffmpeg: bool,
    scanning: bool,
    db_path: PathBuf,
    /// Analysis requested while yt-dlp was still being installed.
    deferred_analysis: Option<String>,
    subs_checking: HashSet<i64>,
    next_trim: u64,
    /// Clip files being written, so simultaneous trims never pick the same name.
    trim_outputs: HashSet<PathBuf>,
    trim_queue: VecDeque<trims::Pending>,
    trim_running: bool,
}

impl Core {
    fn new(cfg: EngineConfig, shared: Arc<Shared>, rx: mpsc::UnboundedReceiver<Cmd>) -> Self {
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        let db = match Db::open(&cfg.db_path) {
            Ok(db) => Some(db),
            Err(e) => {
                shared
                    .update(|s| s.notify(Level::Error, format!("Could not open the library: {e}")));
                None
            }
        };
        Self {
            settings: cfg.settings,
            tools: Arc::new(Tools::new(cfg.tools_dir)),
            shared,
            db,
            thumbs_dir: cfg.thumbs_dir,
            subs_dir: cfg.subs_dir,
            themes_dir: cfg.themes_dir,
            rx,
            done_tx,
            done_rx,
            tools_busy: None,
            tool_queue: VecDeque::new(),
            pending_auto: false,
            running: HashMap::new(),
            next_run: 0,
            stopping: HashSet::new(),
            partials: HashMap::new(),
            remove_after_stop: Vec::new(),
            analysis_gen: 0,
            analysis_task: None,
            warned_no_ffmpeg: false,
            scanning: false,
            db_path: cfg.db_path.clone(),
            subs_checking: HashSet::new(),
            next_trim: 0,
            trim_outputs: HashSet::new(),
            trim_queue: VecDeque::new(),
            trim_running: false,
            deferred_analysis: None,
        }
    }

    async fn run(mut self) {
        self.load_queue_and_presets();
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
        self.pump();
        self.refresh_subs();
        loop {
            // One timer: the sooner of the next update check and the next subscription check.
            let mut next = self.next_update_check();
            if let Some(due) = self.next_sub_due() {
                // Capped, because the monotonic clock may not count time spent suspended.
                let secs = (due - unix_now()).clamp(0, 15 * 60) as u64;
                next = next.min(tokio::time::Instant::now() + Duration::from_secs(secs));
            }
            let sleep = tokio::time::sleep_until(next);
            tokio::select! {
                cmd = self.rx.recv() => match cmd {
                    None | Some(Cmd::Shutdown) => break,
                    Some(cmd) => self.handle(cmd).await,
                },
                Some(done) = self.done_rx.recv() => self.on_done(done).await,
                _ = sleep => {
                    self.check_due_subs();
                    if self.update_due() {
                        if self.settings.auto_update && self.tools_busy.is_none() && !self.ytdlp_overridden() {
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
        // Stop downloads; they resume on the next start. Give the tasks a moment to kill
        // their process trees (dropping them would too, via the process guards).
        let mut waiting = self.running.len();
        for (_, r) in self.running.drain() {
            let _ = r.stop.send(StopReason::Pause);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while waiting > 0 {
            match tokio::time::timeout_at(deadline, self.done_rx.recv()).await {
                Ok(Some(Done::Download(..))) => waiting -= 1,
                Ok(Some(_)) => {}
                _ => break,
            }
        }
        if let Some(db) = &self.db {
            let _ = db.reset_running();
        }
    }

    async fn on_done(&mut self, done: Done) {
        match done {
            Done::Tools => {
                self.tools_busy = None;
                self.refresh_tools().await;
                self.after_check();
                self.next_tool_job();
                self.pump();
                // Subscriptions waiting for yt-dlp can run now.
                self.check_due_subs();
                if self.tools_busy.is_none()
                    && let Some(url) = self.deferred_analysis.take()
                {
                    self.analyse(url);
                }
            }
            Done::Download(id, run, outcome) => {
                self.finish_download(id, run, outcome);
                if self.running.is_empty() {
                    self.next_tool_job();
                }
                self.pump();
            }
            Done::Analysis(generation, result) => {
                if generation == self.analysis_gen {
                    self.shared.update(|s| {
                        if let Some(a) = s.analysis.as_mut() {
                            a.busy = false;
                            a.result = Some(result);
                        }
                    });
                }
            }
            Done::Expanded(url, result) => {
                self.shared
                    .update(|s| s.expanding = s.expanding.saturating_sub(1));
                match result {
                    Ok(items) => self.enqueue(items),
                    Err(e) => self
                        .shared
                        .update(|s| s.notify(Level::Error, format!("{url}: {e}"))),
                }
            }
            Done::SubChecked(id, res) => self.finish_check(id, res),
            Done::Trimmed(r) => self.finish_trim(r),
            Done::Scan(result) => match result {
                Ok(changes) => self.apply_scan(changes),
                Err(e) => {
                    self.scanning = false;
                    self.shared
                        .update(|s| s.notify(Level::Error, format!("Library check failed: {e}")));
                }
            },
            Done::Validation(args, result) => {
                self.shared.update(|s| {
                    if s.validation.as_ref().is_some_and(|(a, _)| *a == args) {
                        s.validation = Some((args, Some(result)));
                    }
                });
            }
        }
    }

    fn export(&mut self, path: &std::path::Path, settings: &Settings) {
        let Some(db) = &self.db else { return };
        let res = crate::export::build(db, settings, &self.themes_dir)
            .and_then(|b| crate::export::write(path, &b).map(|_| b));
        self.shared.update(|s| match res {
            Ok(b) => s.notify(
                Level::Info,
                format!(
                    "Exported settings, {} preset(s), {} subscription(s) and {} theme(s) to {}",
                    b.presets.len(),
                    b.subscriptions.len(),
                    b.themes.len(),
                    path.display()
                ),
            ),
            Err(e) => s.notify(Level::Error, format!("Export failed: {e}")),
        });
    }

    fn import(
        &mut self,
        path: &std::path::Path,
        mode: crate::export::ImportMode,
        current: &Settings,
    ) {
        let Some(db) = self.db.as_mut() else { return };
        let res = crate::export::read(path)
            .and_then(|b| crate::export::apply(db, &b, current, &self.themes_dir, mode));
        match res {
            Ok(sum) => {
                let presets = db
                    .presets()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(_, name, json)| Preset {
                        name,
                        options: serde_json::from_str(&json).unwrap_or_default(),
                    })
                    .collect();
                self.refresh_subs();
                self.shared.update(|s| {
                    s.presets = presets;
                    s.imported_settings = sum.settings.clone();
                    s.themes_rev += 1;
                    s.notify(Level::Info, sum.describe());
                });
            }
            Err(e) => self
                .shared
                .update(|s| s.notify(Level::Error, format!("Import failed: {e}"))),
        }
    }

    fn next_tool_job(&mut self) {
        if self.tools_busy.is_some() {
            return;
        }
        if let Some(job) = self.tool_queue.pop_front() {
            self.spawn_tool_job(job);
        }
    }

    fn ytdlp_overridden(&self) -> bool {
        !self.settings.ytdlp_path.trim().is_empty()
    }

    /// Whether the scheduled yt-dlp update check is due (wall-clock time).
    fn update_due(&self) -> bool {
        let last = self.shared.lock().tools.last_check;
        let interval = self.settings.update_interval_hours.max(1) as i64 * 3600;
        unix_now() >= last + interval
    }

    fn next_update_check(&self) -> tokio::time::Instant {
        let last = self.shared.lock().tools.last_check;
        let interval = self.settings.update_interval_hours.max(1) as i64 * 3600;
        let due = (last + interval - unix_now()).clamp(0, interval);
        tokio::time::Instant::now() + Duration::from_secs(due.max(1) as u64)
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
                let more_slots = s.concurrency > self.settings.concurrency;
                self.settings = *s;
                if tools_changed {
                    self.refresh_tools().await;
                }
                if channel_changed && !self.ytdlp_overridden() {
                    self.shared.update(|s| s.tools.ytdlp_latest = None);
                    self.spawn_tool_job(ToolJob::Check);
                }
                if more_slots || tools_changed {
                    self.pump();
                }
            }
            Cmd::Analyse(url) => self.analyse(url),
            Cmd::ClearAnalysis => self.cancel_analysis(),
            Cmd::Enqueue(items) => self.enqueue(items),
            Cmd::QuickAdd { urls, template } => self.quick_add(urls, template),
            Cmd::ScanFiles => self.scan_files(),
            Cmd::DeleteEntries { ids, delete_files } => self.delete_entries(ids, delete_files),
            Cmd::Redownload(id) => self.redownload(id),
            Cmd::Pause(id) => self.pause(id),
            Cmd::Resume(id) | Cmd::Retry(id) => self.resume(id),
            Cmd::Cancel(id) => self.cancel(id),
            Cmd::Remove(id) => self.remove(id),
            Cmd::Move(id, to) => self.move_item(id, to),
            Cmd::PauseAll => {
                let ids: Vec<i64> = self
                    .shared
                    .lock()
                    .queue
                    .iter()
                    .filter(|q| q.status.is_active())
                    .map(|q| q.id)
                    .collect();
                for id in ids {
                    self.pause(id);
                }
            }
            Cmd::ResumeAll => {
                let ids: Vec<i64> = self
                    .shared
                    .lock()
                    .queue
                    .iter()
                    .filter(|q| q.status == crate::db::Status::Paused)
                    .map(|q| q.id)
                    .collect();
                for id in ids {
                    self.resume(id);
                }
            }
            Cmd::ClearFinished => self.clear_finished(),
            Cmd::SavePreset { old_name, preset } => self.save_preset(old_name, preset),
            Cmd::DeletePreset(name) => self.delete_preset(&name),
            Cmd::ValidateArgs(args) => self.validate_args(args),
            Cmd::AddSubscription { name, url, config } => self.add_sub(name, url, config),
            Cmd::UpdateSubscription {
                id,
                name,
                url,
                config,
            } => self.update_sub(id, name, url, config),
            Cmd::DeleteSubscription(id) => self.delete_sub(id),
            Cmd::PauseSubscription(id, p) => self.pause_sub(id, p),
            Cmd::CheckSubscription(id) => self.check_sub(id),
            Cmd::Trim {
                id,
                start,
                end,
                accurate,
            } => self.start_trim(id, start, end, accurate),
            Cmd::CheckAllSubscriptions => {
                let ids: Vec<i64> = self
                    .shared
                    .lock()
                    .subscriptions
                    .iter()
                    .filter(|v| !v.sub.paused)
                    .map(|v| v.sub.id)
                    .collect();
                for id in ids {
                    self.check_sub(id);
                }
            }
            Cmd::SaveResume(id, pos) => {
                if let Some(db) = &self.db {
                    let _ = db.set_resume(id, pos);
                }
            }
            Cmd::Export { path, settings } => self.export(&path, &settings),
            Cmd::Import {
                path,
                mode,
                current,
            } => self.import(&path, mode, &current),
            Cmd::Shutdown => {}
        }
    }
}
