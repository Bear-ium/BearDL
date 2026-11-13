//! Trim jobs: ffmpeg cuts a clip from a library file; the clip becomes a library entry.
//! Jobs run one at a time (accurate cuts re-encode and are CPU-heavy).
use super::{Core, Done, Level};
use crate::db::{Download, Status};
use crate::trim::{self, TrimSpec};
use crate::util::{ProcessGuard, async_command, format_duration, unix_now};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

#[derive(Clone, Debug, PartialEq)]
pub struct TrimJob {
    pub key: u64,
    pub label: String,
    pub fraction: Option<f32>,
    pub error: Option<String>,
    pub done: bool,
}

/// A trim waiting to run or running. The source entry is captured when it is requested.
pub(super) struct Pending {
    key: u64,
    source: Download,
    spec: TrimSpec,
}

pub(super) struct TrimResult {
    pub key: u64,
    pub source: Download,
    pub spec: TrimSpec,
    /// Length of the written clip in seconds (fast cuts snap to keyframes).
    pub outcome: Result<f64, String>,
}

/// Whether new files can be created in `dir`.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".beardl-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

impl Core {
    pub(super) fn start_trim(&mut self, id: i64, start: f64, end: f64, accurate: bool) {
        let fail = |core: &mut Core, msg: String| {
            core.shared
                .update(|s| s.notify(Level::Error, format!("Trim: {msg}")))
        };
        let Some(d) = self
            .db
            .as_ref()
            .and_then(|db| db.get_download(id).ok().flatten())
        else {
            return fail(self, "the item is no longer in the library".into());
        };
        if self.shared.lock().paths.ffmpeg.is_none() {
            return fail(self, "ffmpeg is needed; install it in Settings".into());
        }
        let input = PathBuf::from(&d.file_path);
        if !input.is_file() {
            return fail(self, "the file is missing".into());
        }
        if let Err(e) = trim::validate(start, end, d.duration) {
            return fail(self, e);
        }
        let end = d.duration.map_or(end, |dur| end.min(dur));
        // Write next to the source, or to the download folder if that is read-only.
        let dir_ok = input.parent().is_some_and(writable);
        let base = if dir_ok {
            input.clone()
        } else {
            let _ = std::fs::create_dir_all(&self.settings.download_dir);
            self.settings
                .download_dir
                .join(input.file_name().unwrap_or_default())
        };
        let reserved = &self.trim_outputs;
        let output =
            trim::clip_path_avoiding(&base, start, end, accurate, &|p| reserved.contains(p));
        self.trim_outputs.insert(output.clone());
        let spec = TrimSpec {
            output,
            input,
            start,
            end,
            accurate,
        };
        self.next_trim += 1;
        let key = self.next_trim;
        let label = format!(
            "{} ({}\u{2013}{})",
            d.title,
            format_duration(start),
            format_duration(end)
        );
        self.shared.update(|s| {
            s.trims.retain(|t| !t.done);
            s.trims.push(TrimJob {
                key,
                label,
                fraction: None,
                error: None,
                done: false,
            });
        });
        self.trim_queue.push_back(Pending {
            key,
            source: d,
            spec,
        });
        self.next_trim_job();
    }

    fn next_trim_job(&mut self) {
        if self.trim_running {
            return;
        }
        let Some(job) = self.trim_queue.pop_front() else {
            return;
        };
        let (Some(ffmpeg), ffprobe) = ({
            let p = &self.shared.lock().paths;
            (p.ffmpeg.clone(), p.ffprobe.clone())
        }) else {
            return;
        };
        self.trim_running = true;
        self.shared.update(|s| {
            if let Some(t) = s.trims.iter_mut().find(|t| t.key == job.key) {
                t.fraction = Some(0.0);
            }
        });
        let shared = self.shared.clone();
        let done = self.done_tx.clone();
        tokio::spawn(async move {
            let outcome = run(&ffmpeg, &job.spec, job.key, &shared)
                .await
                .map(|written| {
                    // Stream-copy clips start at a keyframe, so the file can be longer than asked.
                    ffprobe
                        .and_then(|p| probe_duration(&p, &job.spec.output))
                        .unwrap_or(written)
                });
            let _ = done.send(Done::Trimmed(TrimResult {
                key: job.key,
                source: job.source,
                spec: job.spec,
                outcome,
            }));
        });
    }

    pub(super) fn finish_trim(&mut self, r: TrimResult) {
        self.trim_running = false;
        self.trim_outputs.remove(&r.spec.output);
        match &r.outcome {
            Err(e) => {
                let _ = std::fs::remove_file(&r.spec.output);
                self.shared
                    .update(|s| s.notify(Level::Error, format!("Trim failed: {e}")));
            }
            Ok(length) => self.add_clip(&r, *length),
        }
        self.shared.update(|s| {
            if let Some(t) = s.trims.iter_mut().find(|t| t.key == r.key) {
                t.done = true;
                t.fraction = r.outcome.is_ok().then_some(1.0);
                t.error = r.outcome.as_ref().err().cloned();
            }
        });
        self.next_trim_job();
    }

    /// Records the clip in the library with the source's metadata. It is a new entry: dated
    /// now and not part of a subscription.
    fn add_clip(&mut self, r: &TrimResult, length: f64) {
        let Some(db) = self.db.as_mut() else { return };
        let src = &r.source;
        let out = r.spec.output.to_string_lossy().into_owned();
        let size = std::fs::metadata(&r.spec.output)
            .ok()
            .map(|m| m.len() as i64);
        let clip = Download {
            title: format!(
                "{} (clip {}\u{2013}{})",
                src.title,
                format_duration(r.spec.start),
                format_duration(r.spec.end)
            ),
            duration: Some(if length > 0.0 {
                length
            } else {
                r.spec.end - r.spec.start
            }),
            chapters_json: String::new(),
            status: Some(Status::Done),
            resume_pos: 0.0,
            subscription_id: None,
            created_at: unix_now(),
            ..src.clone()
        };
        let res = db.insert_download(&clip).and_then(|id| {
            db.update_meta(id, &clip)?;
            db.set_file(id, &out, size)?;
            if !src.thumb_path.is_empty() {
                db.set_thumb(id, &src.thumb_path)?;
            }
            db.set_status(id, Status::Done, "")?;
            db.remove_from_queue(id)
        });
        self.shared.update(|s| {
            s.library_rev += 1;
            match res {
                Ok(()) => s.notify(Level::Info, format!("Clip saved: {out}")),
                Err(e) => s.notify(
                    Level::Error,
                    format!("Clip saved, but not added to the library: {e}"),
                ),
            }
        });
    }
}

/// Container duration in seconds, from ffprobe.
fn probe_duration(ffprobe: &Path, file: &Path) -> Option<f64> {
    let out = crate::util::command(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(file)
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .ok()
        .filter(|d: &f64| d.is_finite() && *d > 0.0)
}

/// Deletes the output unless the job completed (app exit or a crash mid-trim).
struct PartialFile<'a> {
    path: &'a Path,
    keep: bool,
}

impl Drop for PartialFile<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(self.path);
        }
    }
}

async fn run(
    ffmpeg: &Path,
    spec: &TrimSpec,
    key: u64,
    shared: &super::Shared,
) -> Result<f64, String> {
    let mut partial = PartialFile {
        path: &spec.output,
        keep: false,
    };
    let mut c = async_command(ffmpeg);
    c.args(trim::args(spec))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    let _guard = ProcessGuard::attach(&child);
    let total = (spec.end - spec.start).max(0.001);
    let mut err = Vec::new();
    let mut written = 0.0f64;
    let mut se = child.stderr.take();
    let read_err = async {
        if let Some(s) = se.as_mut() {
            let _ = s.read_to_end(&mut err).await;
        }
    };
    let read_progress = async {
        if let Some(so) = child.stdout.take() {
            let mut lines = BufReader::new(so).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if let Some(t) = trim::progress_seconds(&l) {
                    written = written.max(t);
                    let f = (t / total).clamp(0.0, 1.0) as f32;
                    if let Some(j) = shared.lock().trims.iter_mut().find(|j| j.key == key) {
                        j.fraction = Some(f);
                    }
                    shared.wake_soon();
                }
            }
        }
    };
    tokio::join!(read_err, read_progress);
    let status = child.wait().await.map_err(|e| e.to_string())?;
    if status.success() && spec.output.is_file() {
        partial.keep = true;
        Ok(written)
    } else {
        let e = String::from_utf8_lossy(&err);
        Err(e
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("ffmpeg failed")
            .to_owned())
    }
}
