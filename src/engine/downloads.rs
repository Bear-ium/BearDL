//! Download queue: starts yt-dlp processes up to the concurrency limit, tracks progress,
//! and records results in the library.
use super::{Core, Done, Level, MoveTo, Running, Shared, StopReason};
use crate::db::{Download, Status};
use crate::util::{ProcessGuard, async_command};
use crate::ytdlp::{self, Analysis, DownloadOptions, Line, Meta, Preset};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::oneshot;

impl Status {
    /// Queued or running: will download without further action.
    pub fn is_active(self) -> bool {
        matches!(self, Status::Queued | Status::Running)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemProgress {
    pub fraction: Option<f32>,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub speed: Option<f64>,
    pub eta: Option<f64>,
    pub stage: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueItem {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub channel: String,
    pub status: Status,
    pub preset: String,
    pub error: String,
    pub file_path: String,
    pub pos: f64,
    pub progress: ItemProgress,
}

impl QueueItem {
    fn from_download(d: &Download) -> Self {
        Self {
            id: d.id,
            url: d.url.clone(),
            title: d.title.clone(),
            channel: d.channel.clone(),
            status: d.status.unwrap_or(Status::Queued),
            preset: d.preset.clone(),
            error: d.error.clone(),
            file_path: d.file_path.clone(),
            pos: d.queue_pos,
            progress: ItemProgress {
                fraction: (d.status == Some(Status::Done)).then_some(1.0),
                ..Default::default()
            },
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct NewItem {
    pub url: String,
    pub title: String,
    pub channel: String,
    pub duration: Option<f64>,
    pub thumbnail: String,
    pub video_id: String,
    pub preset: String,
    pub options: DownloadOptions,
    pub subscription_id: Option<i64>,
}

pub(super) enum Outcome {
    Finished {
        meta: Option<Meta>,
        file: Option<String>,
        thumb: Option<String>,
        subs: Option<Value>,
        skipped: Option<String>,
    },
    Failed(String),
    Stopped(StopReason, Option<String>),
}

struct Spec {
    id: i64,
    exe: PathBuf,
    args: Vec<String>,
    parts: usize,
}

/// Turns an analysis into queue items. Channel pages list their tabs; the Videos tab
/// (or the first tab) is opened so a quick download gets videos, not tabs.
async fn expand(exe: &Path, url: &str, template: &NewItem) -> Result<Vec<NewItem>, String> {
    let mut target = url.to_owned();
    for _ in 0..2 {
        let out = run_capture(exe, &ytdlp::analyse_args(&target)).await?;
        let v: Value = serde_json::from_slice(&out)
            .map_err(|e| format!("could not read yt-dlp output: {e}"))?;
        match ytdlp::parse_analysis(&v) {
            Analysis::Video { meta, .. } => {
                return Ok(vec![NewItem {
                    url: url.to_owned(),
                    title: meta.title,
                    channel: meta.channel,
                    duration: meta.duration,
                    thumbnail: meta.thumbnail,
                    video_id: meta.id,
                    ..template.clone()
                }]);
            }
            Analysis::Playlist {
                channel, entries, ..
            } => {
                let videos: Vec<NewItem> = entries
                    .iter()
                    .filter(|e| !e.is_playlist)
                    .map(|e| NewItem {
                        url: e.url.clone(),
                        title: e.title.clone(),
                        channel: if e.channel.is_empty() {
                            channel.clone()
                        } else {
                            e.channel.clone()
                        },
                        duration: e.duration,
                        video_id: e.id.clone(),
                        ..template.clone()
                    })
                    .collect();
                if !videos.is_empty() {
                    return Ok(videos);
                }
                let tab = entries
                    .iter()
                    .find(|e| e.url.trim_end_matches('/').ends_with("/videos"))
                    .or_else(|| entries.iter().find(|e| e.is_playlist));
                match tab {
                    Some(t) => target = t.url.clone(),
                    None => return Err("the playlist is empty".into()),
                }
            }
        }
    }
    Err("could not find videos at this URL".into())
}

impl Core {
    pub(super) fn load_queue_and_presets(&mut self) {
        let Some(db) = &self.db else { return };
        let _ = db.reset_running();
        let queue: Vec<QueueItem> = db
            .queue()
            .unwrap_or_default()
            .iter()
            .map(QueueItem::from_download)
            .collect();
        let mut presets = load_presets(db);
        if presets.is_empty() {
            for p in ytdlp::builtin_presets() {
                if let Ok(json) = serde_json::to_string(&p.options) {
                    let _ = db.upsert_preset(&p.name, &json);
                }
            }
            presets = load_presets(db);
        }
        self.shared.update(|s| {
            s.queue = queue;
            s.presets = presets;
        });
    }

    fn ytdlp_exe(&self) -> Option<PathBuf> {
        self.shared.lock().paths.ytdlp.clone()
    }

    pub(super) fn analyse(&mut self, url: String) {
        self.analysis_gen += 1;
        let generation = self.analysis_gen;
        if let Some(h) = self.analysis_task.take() {
            h.abort();
        }
        self.shared.update(|s| {
            s.analysis = Some(super::AnalysisView {
                url: url.clone(),
                busy: true,
                result: None,
            })
        });
        let Some(exe) = self.ytdlp_exe() else {
            if self.ytdlp_update_pending() {
                self.deferred_analysis = Some(url);
                return;
            }
            let _ = self.done_tx.send(Done::Analysis(
                generation,
                Err("yt-dlp is not installed yet".into()),
            ));
            return;
        };
        let done = self.done_tx.clone();
        self.analysis_task = Some(tokio::spawn(async move {
            let res = run_capture(&exe, &ytdlp::analyse_args(&url))
                .await
                .and_then(|out| {
                    serde_json::from_slice::<Value>(&out)
                        .map(|v| Arc::new(ytdlp::parse_analysis(&v)))
                        .map_err(|e| format!("could not read yt-dlp output: {e}"))
                });
            let _ = done.send(Done::Analysis(generation, res));
        }));
    }

    pub(super) fn cancel_analysis(&mut self) {
        self.analysis_gen += 1;
        if let Some(h) = self.analysis_task.take() {
            h.abort();
        }
        self.deferred_analysis = None;
        self.shared.update(|s| s.analysis = None);
    }

    /// Queues URLs directly with a preset. Playlists and channels are expanded first.
    pub(super) fn quick_add(&mut self, urls: Vec<String>, template: NewItem) {
        let Some(exe) = self.ytdlp_exe() else {
            self.shared
                .update(|s| s.notify(Level::Error, "yt-dlp is not installed yet"));
            return;
        };
        for url in urls {
            let done = self.done_tx.clone();
            let exe = exe.clone();
            let template = template.clone();
            self.shared.update(|s| s.expanding += 1);
            tokio::spawn(async move {
                let res = expand(&exe, &url, &template).await;
                let _ = done.send(Done::Expanded(url, res));
            });
        }
    }

    pub(super) fn validate_args(&mut self, args: String) {
        let local = ytdlp::validate_custom_args(&args);
        self.shared
            .update(|s| s.validation = Some((args.clone(), None)));
        let done = self.done_tx.clone();
        let list = match local {
            Ok(l) => l,
            Err(e) => {
                let _ = done.send(Done::Validation(args, Err(e)));
                return;
            }
        };
        let Some(exe) = self.ytdlp_exe() else {
            let _ = done.send(Done::Validation(
                args,
                Err("yt-dlp is not installed".into()),
            ));
            return;
        };
        tokio::spawn(async move {
            let mut c = async_command(&exe);
            c.args(ytdlp::validation_args(&list))
                .envs(ytdlp::ENV)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            let res = match c.output().await {
                Ok(o) => {
                    ytdlp::validation_result(o.status.code(), &String::from_utf8_lossy(&o.stderr))
                }
                Err(e) => Err(format!("could not start yt-dlp: {e}")),
            };
            let _ = done.send(Done::Validation(args, res));
        });
    }

    pub(super) fn enqueue(&mut self, items: Vec<NewItem>) {
        let Some(db) = self.db.as_mut() else { return };
        if items.is_empty() {
            return;
        }
        let rows: Vec<Download> = items
            .iter()
            .map(|it| Download {
                url: it.url.clone(),
                title: it.title.clone(),
                channel: it.channel.clone(),
                duration: it.duration,
                thumbnail_url: it.thumbnail.clone(),
                video_id: it.video_id.clone(),
                status: Some(Status::Queued),
                preset: it.preset.clone(),
                options_json: serde_json::to_string(&it.options).unwrap_or_default(),
                subscription_id: it.subscription_id,
                ..Default::default()
            })
            .collect();
        let base = self
            .shared
            .lock()
            .queue
            .iter()
            .map(|q| q.pos)
            .fold(0.0, f64::max);
        match db.insert_downloads(&rows) {
            Ok(ids) => {
                let added: Vec<QueueItem> = rows
                    .into_iter()
                    .zip(ids)
                    .enumerate()
                    .map(|(i, (d, id))| {
                        QueueItem::from_download(&Download {
                            id,
                            queue_pos: base + i as f64 + 1.0,
                            ..d
                        })
                    })
                    .collect();
                let n = added.len();
                self.shared.update(|s| {
                    s.queue.extend(added);
                    if n > 1 {
                        s.notify(Level::Info, format!("Added {n} downloads"));
                    }
                });
            }
            Err(e) => self
                .shared
                .update(|s| s.notify(Level::Error, format!("Could not queue downloads: {e}"))),
        }
        self.pump();
    }

    fn set_status(&mut self, id: i64, status: Status, error: &str) {
        if let Some(db) = &self.db {
            let _ = db.set_status(id, status, error);
        }
        self.shared.update(|s| {
            if let Some(q) = s.item_mut(id) {
                q.status = status;
                q.error = error.to_owned();
                if status != Status::Running {
                    q.progress.speed = None;
                    q.progress.eta = None;
                    if status != Status::Done {
                        q.progress.stage.clear();
                    }
                }
            }
        });
    }

    fn status_of(&self, id: i64) -> Option<Status> {
        self.shared
            .lock()
            .queue
            .iter()
            .find(|q| q.id == id)
            .map(|q| q.status)
    }

    fn stop(&mut self, id: i64, reason: StopReason) -> bool {
        match self.running.remove(&id) {
            Some(r) => {
                let _ = r.stop.send(reason);
                // Keep it from restarting until the old process has really exited.
                self.stopping.insert(id);
                true
            }
            None => false,
        }
    }

    pub(super) fn pause(&mut self, id: i64) {
        if self.stop(id, StopReason::Pause) || self.status_of(id) == Some(Status::Queued) {
            self.set_status(id, Status::Paused, "");
        }
    }

    pub(super) fn resume(&mut self, id: i64) {
        if matches!(
            self.status_of(id),
            Some(Status::Paused | Status::Failed | Status::Cancelled | Status::Skipped)
        ) {
            self.set_status(id, Status::Queued, "");
            self.shared.update(|s| {
                if let Some(q) = s.item_mut(id) {
                    q.progress = Default::default();
                }
            });
            self.pump();
        }
    }

    pub(super) fn cancel(&mut self, id: i64) {
        let was_running = self.stop(id, StopReason::Cancel);
        if !was_running && let Some(f) = self.partials.remove(&id) {
            remove_partials(Path::new(&f));
        }
        if was_running
            || matches!(
                self.status_of(id),
                Some(Status::Queued | Status::Running | Status::Paused)
            )
        {
            self.set_status(id, Status::Cancelled, "");
        }
    }

    pub(super) fn remove(&mut self, id: i64) {
        if self.running.contains_key(&id) || self.stopping.contains(&id) {
            self.cancel(id);
            self.remove_after_stop.push(id);
            return;
        }
        if self.status_of(id) == Some(Status::Paused)
            && let Some(f) = self.partials.remove(&id)
        {
            remove_partials(Path::new(&f));
        }
        self.drop_from_queue(id);
    }

    fn drop_from_queue(&mut self, id: i64) {
        let status = self.status_of(id);
        if let Some(db) = &self.db {
            // Never-downloaded entries leave no history; finished and failed ones stay.
            if matches!(
                status,
                Some(Status::Queued | Status::Paused | Status::Cancelled)
            ) {
                let _ = db.delete_download(id);
            } else {
                let _ = db.remove_from_queue(id);
            }
        }
        self.shared.update(|s| {
            s.queue.retain(|q| q.id != id);
            s.library_rev += 1;
        });
    }

    /// Moves one item, writing only its own position (the midpoint between its new
    /// neighbours), so reordering stays cheap in long queues.
    pub(super) fn move_item(&mut self, id: i64, to: MoveTo) {
        let new_pos = self.shared.update(|s| {
            let i = s.queue.iter().position(|q| q.id == id)?;
            let n = s.queue.len();
            let target = match to {
                MoveTo::Up => i.checked_sub(1)?,
                MoveTo::Down => (i + 1 < n).then_some(i + 1)?,
                MoveTo::Top => 0,
                MoveTo::Bottom => n - 1,
            };
            if target == i {
                return None;
            }
            let item = s.queue.remove(i);
            s.queue.insert(target, item);
            let before = target.checked_sub(1).map(|j| s.queue[j].pos);
            let after = s.queue.get(target + 1).map(|q| q.pos);
            let pos = match (before, after) {
                (Some(b), Some(a)) => (a + b) / 2.0,
                (None, Some(a)) => a - 1.0,
                (Some(b), None) => b + 1.0,
                (None, None) => 1.0,
            };
            s.queue[target].pos = pos;
            Some(pos)
        });
        let Some(pos) = new_pos else { return };
        if let Some(db) = self.db.as_mut() {
            let _ = db.set_queue_pos(id, pos);
            // Midpoints converge after many moves; renumber when neighbours get too close.
            let tight = self
                .shared
                .lock()
                .queue
                .windows(2)
                .any(|w| (w[1].pos - w[0].pos).abs() < 1e-9);
            if tight {
                let ids: Vec<i64> = self.shared.lock().queue.iter().map(|q| q.id).collect();
                let _ = db.reorder_queue(&ids);
                self.shared.update(|s| {
                    for (i, q) in s.queue.iter_mut().enumerate() {
                        q.pos = i as f64 + 1.0;
                    }
                });
            }
        }
    }

    pub(super) fn clear_finished(&mut self) {
        if let Some(db) = &self.db {
            let _ = db.clear_finished();
        }
        self.shared.update(|s| {
            s.queue
                .retain(|q| !matches!(q.status, Status::Done | Status::Skipped))
        });
    }

    pub(super) fn save_preset(&mut self, old_name: Option<String>, preset: Preset) {
        let Some(db) = &self.db else { return };
        let name = preset.name.trim().to_owned();
        if name.is_empty() {
            self.shared
                .update(|s| s.notify(Level::Error, "A preset needs a name"));
            return;
        }
        let exists = db.presets().unwrap_or_default().iter().any(|p| p.1 == name);
        if exists && old_name.as_deref() != Some(name.as_str()) {
            self.shared.update(|s| {
                s.notify(
                    Level::Error,
                    format!("A preset called {name} already exists"),
                )
            });
            return;
        }
        if let Some(old) = old_name.filter(|o| *o != name) {
            let _ = db.rename_preset(&old, &name);
        }
        let res = serde_json::to_string(&preset.options)
            .map_err(crate::error::Error::from)
            .and_then(|j| db.upsert_preset(&name, &j));
        let presets = load_presets(db);
        self.shared.update(|s| {
            s.presets = presets;
            match res {
                Ok(()) => s.notify(Level::Info, format!("Preset {name} saved")),
                Err(e) => s.notify(Level::Error, format!("Could not save preset: {e}")),
            }
        });
    }

    pub(super) fn delete_preset(&mut self, name: &str) {
        let Some(db) = &self.db else { return };
        let _ = db.delete_preset(name);
        let presets = load_presets(db);
        self.shared.update(|s| s.presets = presets);
    }

    /// Whether a yt-dlp update is running or waiting; new downloads hold off until it is done.
    pub(super) fn ytdlp_update_pending(&self) -> bool {
        self.tools_busy.is_some_and(|j| j.touches_ytdlp())
            || self.tool_queue.iter().any(|j| j.touches_ytdlp())
    }

    fn ffmpeg_install_pending(&self) -> bool {
        let is = |j: &super::ToolJob| *j == super::ToolJob::InstallFfmpeg;
        self.tools_busy.as_ref().is_some_and(is) || self.tool_queue.iter().any(is)
    }

    /// Starts queued items until the concurrency limit is reached.
    pub(super) fn pump(&mut self) {
        if self.db.is_none() || self.ytdlp_update_pending() || self.ffmpeg_install_pending() {
            return;
        }
        let Some(exe) = self.ytdlp_exe() else { return };
        let limit = self.settings.concurrency.max(1) as usize;
        while self.running.len() < limit {
            let next = self
                .shared
                .lock()
                .queue
                .iter()
                .find(|q| {
                    q.status == Status::Queued
                        && !self.running.contains_key(&q.id)
                        && !self.stopping.contains(&q.id)
                })
                .map(|q| q.id);
            let Some(id) = next else { break };
            self.start(id, &exe);
        }
    }

    fn start(&mut self, id: i64, exe: &Path) {
        let Some(d) = self
            .db
            .as_ref()
            .and_then(|db| db.get_download(id).ok().flatten())
        else {
            self.set_status(id, Status::Failed, "missing from the library");
            return;
        };
        let options: DownloadOptions = serde_json::from_str(&d.options_json).unwrap_or_default();
        let paths = self.shared.lock().paths.clone();
        let ffmpeg_dir = paths.ffmpeg_dir();
        let has_ffmpeg = paths.ffmpeg.is_some();
        if !has_ffmpeg && !self.warned_no_ffmpeg {
            self.warned_no_ffmpeg = true;
            self.shared.update(|s| {
                s.notify(
                    Level::Error,
                    "ffmpeg is missing: downloading single-file formats without merging, \
                     conversion or embedding. Install it in Settings.",
                )
            });
        }
        let date_after = options.date_after.trim().to_owned();
        // Share the global limit among the downloads running now (including this one).
        let rate = crate::util::parse_rate(&self.settings.rate_limit)
            .map(|r| (r / (self.running.len() as u64 + 1)).max(1024));
        let args = ytdlp::download_args(&ytdlp::Job {
            url: &d.url,
            options: &options,
            download_dir: &self.settings.download_dir,
            output_template: &self.settings.output_template,
            sub_langs: &self.settings.subtitle_langs,
            rate_limit: rate,
            ffmpeg_dir: ffmpeg_dir.as_deref(),
            thumbs_dir: &self.thumbs_dir,
            subs_dir: &self.subs_dir,
            date_after: (!date_after.is_empty()).then_some(date_after.as_str()),
            has_ffmpeg,
        });
        let spec = Spec {
            id,
            exe: exe.to_path_buf(),
            args,
            parts: if has_ffmpeg {
                options.expected_parts("")
            } else {
                1
            },
        };
        let (stop_tx, stop_rx) = oneshot::channel();
        self.next_run += 1;
        let run = self.next_run;
        self.running.insert(id, Running { run, stop: stop_tx });
        self.set_status(id, Status::Running, "");
        self.shared.update(|s| {
            if let Some(q) = s.item_mut(id) {
                q.progress = ItemProgress {
                    stage: "Starting".into(),
                    ..Default::default()
                };
            }
        });
        let shared = self.shared.clone();
        let done = self.done_tx.clone();
        tokio::spawn(async move {
            let outcome = run_download(spec, shared, stop_rx).await;
            let _ = done.send(Done::Download(id, run, outcome));
        });
    }

    pub(super) fn finish_download(&mut self, id: i64, run: u64, outcome: Outcome) {
        match self.running.get(&id).map(|r| r.run) {
            Some(r) if r == run => {
                self.running.remove(&id);
            }
            // A newer run of this item is active; this report is from the one it replaced.
            Some(_) => return,
            None => {}
        }
        self.stopping.remove(&id);
        match outcome {
            Outcome::Stopped(reason, filename) => {
                if let Some(f) = filename {
                    if reason == StopReason::Cancel {
                        remove_partials(Path::new(&f));
                    } else {
                        self.partials.insert(id, f);
                    }
                }
            }
            Outcome::Failed(err) => self.set_status(id, Status::Failed, &err),
            Outcome::Finished {
                meta,
                file,
                thumb,
                subs,
                skipped,
            } => {
                self.partials.remove(&id);
                if let Some(m) = &meta {
                    self.store_meta(id, m, file.as_deref(), thumb.as_deref());
                }
                match &file {
                    None => {
                        let reason = skipped.unwrap_or_else(|| {
                            "nothing was downloaded (filtered out, or already in the archive)"
                                .into()
                        });
                        self.set_status(id, Status::Skipped, &reason);
                    }
                    Some(f) => {
                        let size = std::fs::metadata(f).ok().map(|m| m.len() as i64);
                        if let Some(db) = &self.db {
                            let _ = db.set_file(id, f, size);
                        }
                        self.set_status(id, Status::Done, "");
                        self.shared.update(|s| {
                            if let Some(q) = s.item_mut(id) {
                                q.file_path = f.clone();
                                q.progress.fraction = Some(1.0);
                                q.progress.stage.clear();
                            }
                        });
                        self.after_download(id, meta.as_ref(), subs);
                    }
                }
                self.shared.update(|s| s.library_rev += 1);
            }
        }
        if let Some(i) = self.remove_after_stop.iter().position(|x| *x == id) {
            self.remove_after_stop.swap_remove(i);
            self.drop_from_queue(id);
        }
    }

    /// Post-download work off the engine thread: the small list thumbnail.
    fn after_download(&mut self, id: i64, meta: Option<&Meta>, subs: Option<Value>) {
        let tracks = subtitle_files(&self.subs_dir, meta, subs.as_ref());
        if !tracks.is_empty() {
            self.index_subtitles(id, tracks);
        }
        let ffmpeg = self.shared.lock().paths.ffmpeg.clone();
        let thumb = self
            .db
            .as_ref()
            .and_then(|db| db.get_download(id).ok().flatten())
            .map(|d| d.thumb_path)
            .filter(|t| !t.is_empty());
        if let (Some(thumb), Some(ff)) = (thumb, ffmpeg) {
            let thumb = PathBuf::from(thumb);
            if thumb.is_file() {
                let shared = self.shared.clone();
                tokio::task::spawn_blocking(move || {
                    if crate::thumbs::make_small(&ff, &thumb).is_ok() {
                        shared.update(|s| s.library_rev += 1);
                    }
                });
            }
        }
    }

    /// Parses subtitle files and stores every cue in the full-text index, off the engine
    /// thread with its own connection.
    fn index_subtitles(&mut self, id: i64, files: Vec<(String, PathBuf, bool)>) {
        let db_path = self.db_path.clone();
        let shared = self.shared.clone();
        tokio::task::spawn_blocking(move || {
            // One track per language: manual first, then the original-language captions.
            let tracks: Vec<crate::db::SubTrack> = crate::subtitles::choose_tracks(&files)
                .into_iter()
                .filter_map(|(lang, path, is_auto)| {
                    let cues = crate::subtitles::parse_file(&path, is_auto)?;
                    Some(crate::db::SubTrack {
                        is_auto,
                        lang,
                        path: path.to_string_lossy().into_owned(),
                        cues,
                    })
                })
                .collect();
            if tracks.is_empty() {
                return;
            }
            let res =
                crate::db::Db::open(&db_path).and_then(|mut db| db.store_subtitles(id, &tracks));
            // Tell the UI only now, so search pages see the new cues.
            shared.update(|s| match res {
                Ok(_) => s.library_rev += 1,
                Err(e) => s.notify(Level::Error, format!("Could not index subtitles: {e}")),
            });
        });
    }

    fn store_meta(&mut self, id: i64, m: &Meta, file: Option<&str>, thumb: Option<&str>) {
        let Some(db) = &self.db else { return };
        // The extension after conversion or merging, not the one yt-dlp started with.
        let ext = file
            .and_then(|f| Path::new(f).extension())
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_else(|| m.ext.clone());
        let d = Download {
            video_id: m.id.clone(),
            extractor: m.extractor_key.clone(),
            title: m.title.clone(),
            channel: m.channel.clone(),
            channel_id: m.channel_id.clone(),
            duration: m.duration,
            upload_date: m.upload_date.clone(),
            description: m.description.clone(),
            thumbnail_url: m.thumbnail.clone(),
            chapters_json: m.chapters_json.clone(),
            format_note: [m.resolution.as_str(), ext.as_str()]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" "),
            ..Default::default()
        };
        let _ = db.update_meta(id, &d);
        // yt-dlp reports where it wrote the thumbnail (its file name is sanitised).
        let thumb = thumb
            .map(PathBuf::from)
            .filter(|t| t.is_file() && t.extension().is_some_and(|e| e == "jpg"))
            .unwrap_or_else(|| {
                self.thumbs_dir
                    .join(format!("{}-{}.jpg", m.extractor_key, m.id))
            });
        if thumb.is_file() {
            let _ = db.set_thumb(id, &thumb.to_string_lossy());
        }
    }

    /// Deletes library entries, optionally with their files and cached thumbnails.
    pub(super) fn delete_entries(&mut self, ids: Vec<i64>, delete_files: bool) {
        for &id in &ids {
            if self.running.contains_key(&id) {
                self.stop(id, StopReason::Cancel);
            }
        }
        let Some(db) = self.db.as_mut() else { return };
        let mut failed = 0;
        for &id in &ids {
            let Ok(Some(d)) = db.get_download(id) else {
                continue;
            };
            // Only files no remaining entry points at (re-downloads share paths).
            let shared = |p: &str| db.file_refs(p, &ids).unwrap_or(1) > 0;
            if delete_files && d.status == Some(Status::Done) && !d.file_path.is_empty() {
                let p = Path::new(&d.file_path);
                if p.is_file() && !shared(&d.file_path) && std::fs::remove_file(p).is_err() {
                    failed += 1;
                }
            }
            for p in db.sub_track_paths(id).unwrap_or_default() {
                let p = Path::new(&p);
                if p.starts_with(&self.subs_dir) && !shared(&p.to_string_lossy()) {
                    let _ = std::fs::remove_file(p);
                }
            }
            if !d.thumb_path.is_empty() && !shared(&d.thumb_path) {
                let t = Path::new(&d.thumb_path);
                // Only cached thumbnails are removed, never anything outside the cache.
                if t.starts_with(&self.thumbs_dir) {
                    let _ = std::fs::remove_file(crate::thumbs::small_path(t));
                    let _ = std::fs::remove_file(t);
                }
            }
        }
        let res = db.delete_downloads(&ids);
        self.shared.update(|s| {
            s.queue.retain(|q| !ids.contains(&q.id));
            s.library_rev += 1;
            if let Err(e) = res {
                s.notify(Level::Error, format!("Could not delete: {e}"));
            } else if failed > 0 {
                s.notify(
                    Level::Error,
                    format!("{failed} file(s) could not be deleted (in use?)"),
                );
            }
        });
    }

    pub(super) fn redownload(&mut self, id: i64) {
        let Some(d) = self
            .db
            .as_ref()
            .and_then(|db| db.get_download(id).ok().flatten())
        else {
            return;
        };
        let item = NewItem {
            url: d.url,
            title: d.title,
            channel: d.channel,
            duration: d.duration,
            thumbnail: d.thumbnail_url,
            video_id: d.video_id,
            preset: d.preset,
            options: serde_json::from_str(&d.options_json).unwrap_or_default(),
            subscription_id: d.subscription_id,
        };
        self.enqueue(vec![item]);
        self.shared
            .update(|s| s.notify(Level::Info, "Queued for download again"));
    }

    /// Checks every finished file in the background. Missing files are looked for by
    /// name under the download folder, so moved files are re-linked rather than lost.
    pub(super) fn scan_files(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let db_path = self.db_path.clone();
        let root = self.settings.download_dir.clone();
        let done = self.done_tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = crate::db::Db::open(&db_path)
                .and_then(|db| db.files())
                .map(|files| scan(&files, &root));
            let _ = done.send(Done::Scan(result.map_err(|e| e.to_string())));
        });
    }

    pub(super) fn apply_scan(&mut self, changes: Vec<ScanChange>) {
        self.scanning = false;
        let Some(db) = self.db.as_mut() else { return };
        if changes.is_empty() {
            return;
        }
        let mut missing = 0;
        let mut relinked = 0;
        let mut flags = Vec::new();
        let _ = db.conn.execute_batch("BEGIN");
        for c in &changes {
            let new = c
                .new_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned());
            match new {
                // Never re-link to a file another entry already owns.
                Some(p) if db.file_refs(&p, &[c.id]).unwrap_or(1) == 0 => {
                    let size = std::fs::metadata(&p).ok().map(|m| m.len() as i64);
                    let _ = db.set_file(c.id, &p, size);
                    relinked += 1;
                }
                _ => {
                    let gone = c.missing || c.new_path.is_some();
                    flags.push((c.id, gone));
                    if gone {
                        missing += 1;
                    }
                }
            }
        }
        let _ = db.conn.execute_batch("COMMIT");
        let _ = db.set_missing(&flags);
        self.shared.update(|s| {
            s.library_rev += 1;
            if missing > 0 || relinked > 0 {
                let mut parts = Vec::new();
                if missing > 0 {
                    parts.push(format!("{missing} missing"));
                }
                if relinked > 0 {
                    parts.push(format!("{relinked} moved file(s) found again"));
                }
                s.notify(Level::Info, format!("Library check: {}", parts.join(", ")));
            }
        });
    }
}

/// `(language, file)` for each subtitle yt-dlp wrote. They are written to the cache as
/// `<extractor>-<id>.<lang>.<ext>`; yt-dlp's reported `filepath` can be stale, so the cache is
/// read first and reported paths are only a fallback.
pub(super) fn subtitle_files(
    subs_dir: &Path,
    meta: Option<&Meta>,
    reported: Option<&Value>,
) -> Vec<(String, PathBuf, bool)> {
    // Automatic captions are recognisable by their source URL (YouTube: `kind=asr`).
    let is_auto = |lang: &str| {
        reported
            .and_then(|v| v.get(lang))
            .and_then(|i| i.get("url"))
            .and_then(Value::as_str)
            .is_some_and(|u| u.contains("kind=asr"))
            || lang.ends_with("-orig")
    };
    let mut out: Vec<(String, PathBuf, bool)> = Vec::new();
    if let Some(m) = meta.filter(|m| !m.id.is_empty()) {
        let prefix = format!("{}-{}.", m.extractor_key, m.id);
        if let Ok(rd) = std::fs::read_dir(subs_dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let Some(rest) = name.strip_prefix(&prefix) else {
                    continue;
                };
                if let Some((lang, ext)) = rest.rsplit_once('.')
                    && matches!(ext, "vtt" | "srt")
                {
                    out.push((lang.to_owned(), e.path(), is_auto(lang)));
                }
            }
        }
    }
    if out.is_empty()
        && let Some(map) = reported.and_then(Value::as_object)
    {
        out = map
            .iter()
            .filter_map(|(lang, info)| {
                let p = PathBuf::from(info.get("filepath")?.as_str()?);
                p.is_file().then(|| (lang.clone(), p, is_auto(lang)))
            })
            .collect();
    }
    out.sort();
    out
}

#[derive(Debug, PartialEq)]
pub(super) struct ScanChange {
    pub id: i64,
    pub missing: bool,
    pub new_path: Option<PathBuf>,
}

/// Compares stored paths with the disk. Only changes are returned. A missing file is
/// matched to a file of the same name and the same size under the download folder.
pub(super) fn scan(files: &[crate::db::FileRow], root: &Path) -> Vec<ScanChange> {
    let mut changes = Vec::new();
    let mut index: Option<std::collections::HashMap<std::ffi::OsString, PathBuf>> = None;
    for (id, path, was_missing, size) in files {
        let p = Path::new(path);
        if p.exists() {
            if *was_missing {
                changes.push(ScanChange {
                    id: *id,
                    missing: false,
                    new_path: None,
                });
            }
            continue;
        }
        let idx = index.get_or_insert_with(|| index_dir(root, 4, 200_000));
        let found = p
            .file_name()
            .and_then(|n| idx.get(n))
            .filter(|f| {
                let actual = std::fs::metadata(f).ok().map(|m| m.len() as i64);
                size.is_some() && actual == *size
            })
            .cloned();
        if found.is_some() || !*was_missing {
            changes.push(ScanChange {
                id: *id,
                missing: found.is_none(),
                new_path: found,
            });
        }
    }
    changes
}

fn index_dir(
    root: &Path,
    depth: u32,
    limit: usize,
) -> std::collections::HashMap<std::ffi::OsString, PathBuf> {
    let mut map = std::collections::HashMap::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, d)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            if map.len() >= limit {
                return map;
            }
            // file_type() does not follow links, so symlinks and junctions are skipped.
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                if d < depth {
                    stack.push((e.path(), d + 1));
                }
            } else if t.is_file() {
                map.entry(e.file_name()).or_insert_with(|| e.path());
            }
        }
    }
    map
}

fn load_presets(db: &crate::db::Db) -> Vec<Preset> {
    db.presets()
        .unwrap_or_default()
        .into_iter()
        .map(|(_, name, json)| Preset {
            name,
            options: serde_json::from_str(&json).unwrap_or_default(),
        })
        .collect()
}

/// Runs yt-dlp to completion and returns stdout, or the error it printed.
pub(super) async fn run_capture(exe: &Path, args: &[String]) -> Result<Vec<u8>, String> {
    let mut c = async_command(exe);
    c.args(args)
        .envs(ytdlp::ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c
        .spawn()
        .map_err(|e| format!("could not start yt-dlp: {e}"))?;
    // Dropping this future (an aborted analysis) kills the whole tree.
    let _guard = ProcessGuard::attach(&child);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let (mut so, mut se) = (child.stdout.take(), child.stderr.take());
    let read_out = async {
        if let Some(s) = so.as_mut() {
            let _ = s.read_to_end(&mut out).await;
        }
    };
    let read_err = async {
        if let Some(s) = se.as_mut() {
            let _ = s.read_to_end(&mut err).await;
        }
    };
    tokio::join!(read_out, read_err);
    let status = child.wait().await.map_err(|e| e.to_string())?;
    if status.success() {
        Ok(out)
    } else {
        Err(error_text(&String::from_utf8_lossy(&err)))
    }
}

/// The useful part of yt-dlp's stderr: its ERROR lines, or the last lines.
pub fn error_text(stderr: &str) -> String {
    let errors: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("ERROR:").map(str::trim))
        .collect();
    if !errors.is_empty() {
        return errors.join("\n");
    }
    let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(3)..];
    if tail.is_empty() {
        "yt-dlp failed".into()
    } else {
        tail.join("\n")
    }
}

#[derive(Default)]
struct RunState {
    meta: Option<Meta>,
    filename: Option<String>,
    file: Option<String>,
    thumb: Option<String>,
    subs: Option<Value>,
    errors: Vec<String>,
    tail: Vec<String>,
    skipped: Option<String>,
    part: usize,
    last_format: String,
    last_downloaded: u64,
    parts: usize,
}

impl RunState {
    fn feed(&mut self, line: &str, shared: &Shared, id: i64) {
        match ytdlp::parse_line(line) {
            Line::Progress(p) => {
                // Each requested format (video, then audio) reports its own progress.
                let downloaded = p.downloaded.unwrap_or(0);
                let new_stream = (!p.format_id.is_empty()
                    && !self.last_format.is_empty()
                    && p.format_id != self.last_format)
                    || (p.format_id.is_empty() && downloaded + 1_000_000 < self.last_downloaded);
                if new_stream {
                    self.part += 1;
                }
                self.last_format = p.format_id.clone();
                self.last_downloaded = downloaded;
                let parts = self.parts.max(self.part + 1);
                let frac = p
                    .total
                    .filter(|t| *t > 0)
                    .map(|t| (downloaded as f64 / t as f64).min(1.0));
                let overall = frac.map(|f| ((self.part as f64 + f) / parts as f64) as f32);
                let stage = if parts > 1 {
                    format!("Downloading {}/{}", self.part + 1, parts)
                } else {
                    "Downloading".into()
                };
                if let Some(q) = shared.lock().item_mut(id) {
                    q.progress = ItemProgress {
                        fraction: overall.or(q.progress.fraction),
                        downloaded,
                        total: p.total,
                        speed: p.speed,
                        eta: p.eta,
                        stage,
                    };
                }
                shared.wake_soon();
            }
            Line::PostProcess { status, name } => {
                if status == "started" {
                    let label = ytdlp::postprocessor_label(&name);
                    shared.update(|s| {
                        if let Some(q) = s.item_mut(id) {
                            q.progress.stage = label.into();
                            q.progress.speed = None;
                            q.progress.eta = None;
                        }
                    });
                }
            }
            Line::Meta(v) => {
                let m = ytdlp::parse_meta(&v);
                self.filename = v.get("filename").and_then(Value::as_str).map(str::to_owned);
                if !m.format_id.is_empty() {
                    self.parts = m.format_id.split('+').count().max(1);
                }
                shared.update(|s| {
                    if let Some(q) = s.item_mut(id) {
                        if !m.title.is_empty() {
                            q.title = m.title.clone();
                        }
                        if !m.channel.is_empty() {
                            q.channel = m.channel.clone();
                        }
                    }
                });
                self.meta = Some(m);
            }
            Line::File(f) => self.file = Some(f),
            Line::Thumb(t) => self.thumb = Some(t),
            Line::Subs(v) => self.subs = Some(*v),
            Line::Error(e) => self.errors.push(e),
            Line::Other => {
                let t = line.trim();
                if t.is_empty() {
                    return;
                }
                if ytdlp::is_filter_skip(t) {
                    self.skipped = Some(t.trim_start_matches("[download]").trim().to_owned());
                }
                if self.tail.len() >= 5 {
                    self.tail.remove(0);
                }
                self.tail.push(t.to_owned());
            }
        }
    }
}

async fn run_download(
    spec: Spec,
    shared: Arc<Shared>,
    mut stop: oneshot::Receiver<StopReason>,
) -> Outcome {
    let mut c = async_command(&spec.exe);
    c.args(&spec.args)
        .envs(ytdlp::ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return Outcome::Failed(format!("could not start yt-dlp: {e}")),
    };
    let guard = ProcessGuard::attach(&child);
    let (Some(so), Some(se)) = (child.stdout.take(), child.stderr.take()) else {
        return Outcome::Failed("could not read yt-dlp output".into());
    };
    let mut out = BufReader::new(so).split(b'\n');
    let mut err = BufReader::new(se).split(b'\n');
    let mut st = RunState {
        parts: spec.parts,
        ..Default::default()
    };
    let stop_now = |reason: Result<StopReason, _>, st: RunState| {
        if let Some(g) = &guard {
            g.kill();
        }
        Outcome::Stopped(reason.unwrap_or(StopReason::Pause), st.filename.or(st.file))
    };
    let (mut out_open, mut err_open) = (true, true);
    while out_open || err_open {
        tokio::select! {
            reason = &mut stop => {
                let o = stop_now(reason, st);
                let _ = child.kill().await;
                let _ = child.wait().await;
                return o;
            }
            seg = out.next_segment(), if out_open => match seg {
                Ok(Some(b)) => st.feed(&String::from_utf8_lossy(&b), &shared, spec.id),
                _ => out_open = false,
            },
            seg = err.next_segment(), if err_open => match seg {
                Ok(Some(b)) => st.feed(&String::from_utf8_lossy(&b), &shared, spec.id),
                _ => err_open = false,
            },
        }
    }
    let status = tokio::select! {
        s = child.wait() => s,
        reason = &mut stop => {
            let o = stop_now(reason, st);
            let _ = child.kill().await;
            return o;
        }
    };
    let ok = status.as_ref().is_ok_and(|s| s.success());
    // 101: stopped by --break-on-reject / --max-downloads, which only happens on filters.
    let filtered = status.as_ref().ok().and_then(|s| s.code()) == Some(101);
    if ok || filtered || st.file.is_some() || (st.skipped.is_some() && st.errors.is_empty()) {
        return Outcome::Finished {
            meta: st.meta,
            file: st.file,
            thumb: st.thumb,
            subs: st.subs,
            skipped: st.skipped.or(filtered.then(|| "filtered out".to_owned())),
        };
    }
    let msg = if st.errors.is_empty() {
        error_text(&st.tail.join("\n"))
    } else {
        st.errors.join("\n")
    };
    Outcome::Failed(msg)
}

/// Removes yt-dlp's partial files for a cancelled download (`.part`, `.ytdl`, `.fNNN.ext`).
pub fn remove_partials(final_path: &Path) {
    let (Some(dir), Some(stem)) = (
        final_path.parent(),
        final_path.file_stem().and_then(|s| s.to_str()),
    ) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if is_partial_of(&name, stem) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// True for yt-dlp's intermediate files of `stem`, never for a finished file.
pub fn is_partial_of(name: &str, stem: &str) -> bool {
    let Some(rest) = name.strip_prefix(stem) else {
        return false;
    };
    if !rest.starts_with('.') {
        return false;
    }
    if rest.ends_with(".part") || rest.ends_with(".ytdl") || rest.contains(".part-Frag") {
        return true;
    }
    if rest.starts_with(".temp.") {
        return true;
    }
    // `.f<format id>.<ext>`: a per-format stream before merging. Requires a second dot so
    // `.flac` or `.flv` never match.
    if let Some(after) = rest.strip_prefix(".f")
        && let Some((id, ext)) = after.split_once('.')
    {
        return !id.is_empty()
            && !ext.is_empty()
            && id.chars().next().is_some_and(|c| c.is_ascii_digit())
            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_names() {
        let stem = "Title [abc]";
        assert!(is_partial_of("Title [abc].mp4.part", stem));
        assert!(is_partial_of("Title [abc].f137.mp4", stem));
        assert!(is_partial_of("Title [abc].f251.webm.part", stem));
        assert!(is_partial_of("Title [abc].f303-1.webm", stem));
        assert!(is_partial_of("Title [abc].mp4.ytdl", stem));
        assert!(is_partial_of("Title [abc].temp.mkv", stem));
        assert!(!is_partial_of("Title [abc].mkv", stem));
        assert!(!is_partial_of("Title [abc].flac", stem));
        assert!(!is_partial_of("Title [abc].flv", stem));
        assert!(!is_partial_of("Title [abc] 2.mp4.part", stem));
        assert!(!is_partial_of("Other.mp4.part", stem));
    }

    #[test]
    fn finds_subtitle_files_in_cache() {
        let d = std::env::temp_dir().join(format!("beardl-subs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for f in [
            "Youtube-abc.en.vtt",
            "Youtube-abc.de.srt",
            "Youtube-abc.jpg",
            "Youtube-abcd.en.vtt",
        ] {
            std::fs::write(d.join(f), "x").unwrap();
        }
        let meta = Meta {
            id: "abc".into(),
            extractor_key: "Youtube".into(),
            ..Default::default()
        };
        let langs: Vec<String> = subtitle_files(&d, Some(&meta), None)
            .into_iter()
            .map(|(l, _, _)| l)
            .collect();
        assert_eq!(langs, vec!["de", "en"]);
        assert!(subtitle_files(&d, None, None).is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn error_text_prefers_error_lines() {
        assert_eq!(
            error_text("WARNING: x\nERROR: [youtube] a: Private video\n"),
            "[youtube] a: Private video"
        );
        assert_eq!(error_text("a\nb\nc\nd\n"), "b\nc\nd");
        assert_eq!(error_text(""), "yt-dlp failed");
    }

    #[test]
    fn scan_detects_missing_and_moved() {
        let root = std::env::temp_dir().join(format!("beardl-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let here = root.join("here.mp4");
        std::fs::write(&here, "x").unwrap();
        std::fs::write(root.join("sub/moved.mp4"), "x").unwrap();
        let s = |p: std::path::PathBuf, missing: bool, size: i64| {
            (p.to_string_lossy().into_owned(), missing, Some(size))
        };
        let row = |id: i64, (p, m, sz): (String, bool, Option<i64>)| (id, p, m, sz);
        std::fs::write(root.join("sub/other.mp4"), "different size").unwrap();
        let files = vec![
            row(1, s(here.clone(), false, 1)),
            row(2, s(root.join("old/moved.mp4"), false, 1)),
            row(3, s(root.join("gone.mp4"), false, 1)),
            row(4, s(root.join("gone2.mp4"), true, 1)),
            row(5, s(here.clone(), true, 1)),
            row(6, s(root.join("x/other.mp4"), true, 1)),
        ];
        let ch = scan(&files, &root);
        assert_eq!(ch.len(), 3, "{ch:?}");
        assert_eq!(ch[0].id, 2);
        assert!(ch[0].new_path.as_ref().unwrap().ends_with("moved.mp4"));
        assert_eq!(
            ch[1],
            ScanChange {
                id: 3,
                missing: true,
                new_path: None
            }
        );
        assert_eq!(
            ch[2],
            ScanChange {
                id: 5,
                missing: false,
                new_path: None
            }
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
