//! Subscription checks: list the newest entries with `--flat-playlist`, queue unseen ones.
use super::downloads::{NewItem, run_capture};
use super::{Core, Done, Level};
use crate::subs::{self, LogEntry, SubConfig, Subscription};
use crate::util::unix_now;
use crate::ytdlp;

/// A subscription as the UI shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct SubView {
    pub sub: Subscription,
    pub seen: i64,
    pub logs: Vec<LogEntry>,
    pub checking: bool,
}

/// Parses `--flat-playlist -j` output (one JSON object per line).
pub fn parse_listing(out: &[u8]) -> (Vec<ytdlp::Entry>, Option<String>) {
    let mut entries = Vec::new();
    let mut title = None;
    for line in String::from_utf8_lossy(out).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if title.is_none() {
            title = [
                "playlist_uploader",
                "playlist_channel",
                "playlist_title",
                "channel",
            ]
            .iter()
            .find_map(|k| v.get(*k).and_then(|t| t.as_str()))
            .filter(|t| !t.is_empty())
            .map(str::to_owned);
        }
        if let Some(e) = ytdlp::parse_entry(&v) {
            entries.push(e);
        }
    }
    (entries, title)
}

/// A YouTube channel's root URL lists its tabs, not videos: point it at the Videos tab.
pub fn normalise_url(url: &str) -> String {
    let lower = url.to_ascii_lowercase();
    let is_yt = lower.contains("youtube.com/");
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .trim_end_matches('/');
    let segs: Vec<&str> = path.split('/').skip(3).collect();
    let channel_root = match segs.as_slice() {
        [h] => h.starts_with('@'),
        [kind, _] => matches!(*kind, "channel" | "c" | "user"),
        _ => false,
    };
    if is_yt && channel_root {
        format!("{path}/videos")
    } else {
        url.to_owned()
    }
}

/// A readable default name for a URL: its last meaningful path segment.
pub fn name_from_url(url: &str) -> String {
    let u = url.split(['?', '#']).next().unwrap_or(url);
    let parts: Vec<&str> = u
        .trim_end_matches('/')
        .split('/')
        .filter(|p| !p.is_empty() && !p.contains(':'))
        .collect();
    let skip = [
        "videos",
        "streams",
        "shorts",
        "featured",
        "playlists",
        "www",
    ];
    parts
        .iter()
        .rev()
        .find(|p| !skip.contains(p))
        .map(|p| p.trim_start_matches('@').to_owned())
        .unwrap_or_else(|| url.to_owned())
}

impl Core {
    pub(super) fn refresh_subs(&mut self) {
        let Some(db) = &self.db else { return };
        let views: Vec<SubView> = subs::list(db)
            .unwrap_or_default()
            .into_iter()
            .map(|s| SubView {
                seen: subs::seen_count(db, s.id),
                logs: subs::logs(db, s.id, 20).unwrap_or_default(),
                checking: self.subs_checking.contains(&s.id),
                sub: s,
            })
            .collect();
        self.shared.update(|s| s.subscriptions = views);
    }

    /// Unix time of the earliest due subscription check, if any. None while yt-dlp is missing
    /// or being updated: checks resume when that finishes (the tool job wakes the loop).
    pub(super) fn next_sub_due(&self) -> Option<i64> {
        if self.shared.lock().paths.ytdlp.is_none() || self.ytdlp_update_pending() {
            return None;
        }
        let now = unix_now();
        self.shared
            .lock()
            .subscriptions
            .iter()
            .filter(|v| !v.checking)
            // A clock set backwards must not postpone checks beyond one interval.
            .filter_map(|v| {
                v.sub
                    .due_at()
                    .map(|d| d.min(now + v.sub.config.interval_minutes as i64 * 60))
            })
            .min()
    }

    pub(super) fn check_due_subs(&mut self) {
        let now = unix_now();
        let due: Vec<i64> = self
            .shared
            .lock()
            .subscriptions
            .iter()
            .filter(|v| v.sub.due_at().is_some_and(|d| d <= now))
            .map(|v| v.sub.id)
            .collect();
        for id in due {
            self.check_sub(id);
        }
    }

    pub(super) fn add_sub(&mut self, name: String, url: String, cfg: SubConfig) {
        let Some(db) = &self.db else { return };
        let url = normalise_url(url.trim());
        if url.is_empty() {
            return;
        }
        let name = if name.trim().is_empty() {
            name_from_url(&url)
        } else {
            name.trim().to_owned()
        };
        match subs::insert(db, &name, &url, &cfg.sanitised()) {
            Ok(id) => {
                self.refresh_subs();
                self.check_sub(id);
            }
            Err(e) => self
                .shared
                .update(|s| s.notify(Level::Error, format!("Could not add subscription: {e}"))),
        }
    }

    pub(super) fn update_sub(&mut self, id: i64, name: String, url: String, cfg: SubConfig) {
        if let Some(db) = &self.db {
            let _ = subs::update(db, id, name.trim(), url.trim(), &cfg.sanitised());
            // A new interval applies from the last check.
            if let Ok(Some(s)) = subs::get(db, id)
                && s.last_check > 0
            {
                let next = s.last_check + s.config.interval_minutes as i64 * 60;
                let _ = subs::set_checked(db, id, s.last_check, next);
            }
        }
        self.refresh_subs();
    }

    pub(super) fn delete_sub(&mut self, id: i64) {
        if let Some(db) = &self.db {
            let _ = subs::delete(db, id);
        }
        self.refresh_subs();
    }

    pub(super) fn pause_sub(&mut self, id: i64, paused: bool) {
        if let Some(db) = &self.db {
            let _ = subs::set_paused(db, id, paused);
        }
        self.refresh_subs();
    }

    pub(super) fn check_sub(&mut self, id: i64) {
        if self.subs_checking.contains(&id) {
            return;
        }
        let Some(sub) = self
            .db
            .as_ref()
            .and_then(|db| subs::get(db, id).ok().flatten())
        else {
            return;
        };
        if self.ytdlp_update_pending() {
            // Checked when the update finishes.
            return;
        }
        let Some(exe) = self.shared.lock().paths.ytdlp.clone() else {
            // Checked once yt-dlp is installed.
            return;
        };
        self.subs_checking.insert(id);
        self.refresh_subs();
        let done = self.done_tx.clone();
        let args = ytdlp::list_args(&sub.url, sub.config.max_items);
        tokio::spawn(async move {
            let res = match tokio::time::timeout(
                std::time::Duration::from_secs(300),
                run_capture(&exe, &args),
            )
            .await
            {
                Ok(r) => r.map(|out| parse_listing(&out)),
                // Dropping the future kills yt-dlp through its process guard.
                Err(_) => Err("the check took more than 5 minutes".into()),
            };
            let _ = done.send(Done::SubChecked(id, res));
        });
    }

    pub(super) fn finish_check(
        &mut self,
        id: i64,
        res: Result<(Vec<ytdlp::Entry>, Option<String>), String>,
    ) {
        self.subs_checking.remove(&id);
        let Some(sub) = self
            .db
            .as_ref()
            .and_then(|db| subs::get(db, id).ok().flatten())
        else {
            self.refresh_subs();
            return;
        };
        let now = unix_now();
        let next = now + sub.config.interval_minutes as i64 * 60;
        let mut queued = Vec::new();
        if let Some(db) = self.db.as_mut() {
            match res {
                Err(e) => {
                    // Keep last_check, so a failed first check is still the first one.
                    let _ = subs::set_checked(db, id, sub.last_check, next);
                    let first = e.lines().next().unwrap_or("check failed").to_owned();
                    let _ = subs::log(db, id, true, &first);
                }
                Ok((entries, _))
                    if !entries.is_empty() && entries.iter().all(|e| e.is_playlist) =>
                {
                    let _ = subs::set_checked(db, id, sub.last_check, next);
                    let _ = subs::log(
                        db,
                        id,
                        true,
                        "this URL lists other lists (such as channel tabs), not videos; \
                         use the channel's /videos URL or a playlist",
                    );
                }
                Ok((mut entries, title)) => {
                    let _ = subs::set_checked(db, id, now, next);
                    // Playlists list oldest first; plan newest first like channels.
                    if ytdlp::is_playlist_url(&sub.url) {
                        entries.reverse();
                    }
                    // A name derived from the URL is replaced by the channel or list title.
                    if let Some(t) = title
                        && sub.name == name_from_url(&sub.url)
                    {
                        let _ = subs::update(db, id, &t, &sub.url, &sub.config);
                    }
                    let first = sub.last_check == 0;
                    let seen = |v: &str| subs::is_seen(db, id, v);
                    let plan = subs::plan(&entries, &seen, &sub.config, first);
                    let _ = subs::mark_seen(db, id, &plan.seen);
                    let mut msg = format!(
                        "Listed {}: {} new, {} queued",
                        entries.len(),
                        plan.seen.len(),
                        plan.queue.len()
                    );
                    if plan.filtered > 0 {
                        msg.push_str(&format!(", {} filtered by title", plan.filtered));
                    }
                    if first && sub.config.first_check == subs::FirstCheck::OnlyNew {
                        msg.push_str(" (first check: existing items marked as seen)");
                    }
                    let _ = subs::log(db, id, false, &msg);
                    queued = plan.queue;
                }
            }
        }
        if !queued.is_empty() {
            let preset_name = if sub.config.preset.is_empty() {
                self.settings.default_preset.clone()
            } else {
                sub.config.preset.clone()
            };
            let base = self
                .shared
                .lock()
                .presets
                .iter()
                .find(|p| p.name == preset_name)
                .map(|p| p.options.clone())
                .unwrap_or_default();
            let options = sub.config.apply(base);
            let items = queued
                .into_iter()
                .map(|e| NewItem {
                    url: e.url,
                    title: e.title,
                    channel: if e.channel.is_empty() {
                        sub.name.clone()
                    } else {
                        e.channel
                    },
                    duration: e.duration,
                    video_id: e.id,
                    preset: preset_name.clone(),
                    options: options.clone(),
                    subscription_id: Some(id),
                    ..Default::default()
                })
                .collect();
            self.enqueue(items);
        }
        self.refresh_subs();
    }

    /// Queues a desktop notification for a finished subscription download.
    pub(super) fn notify_sub_download(&mut self, download_id: i64) {
        if !self.settings.notifications {
            return;
        }
        let Some(db) = &self.db else { return };
        let Ok(Some(d)) = db.get_download(download_id) else {
            return;
        };
        let Some(sid) = d.subscription_id else { return };
        let name = subs::get(db, sid)
            .ok()
            .flatten()
            .map(|s| s.name)
            .unwrap_or_default();
        self.shared.update(|s| {
            s.desktop_notices
                .push((format!("New from {name}"), d.title.clone()))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_urls() {
        assert_eq!(
            name_from_url("https://www.youtube.com/@SomeChannel/videos"),
            "SomeChannel"
        );
        assert_eq!(
            name_from_url("https://www.youtube.com/playlist?list=PL123"),
            "playlist"
        );
        assert_eq!(
            name_from_url("https://vimeo.com/channels/staffpicks/"),
            "staffpicks"
        );
    }

    #[test]
    fn channel_roots_get_videos_tab() {
        assert_eq!(
            normalise_url("https://www.youtube.com/@jawed"),
            "https://www.youtube.com/@jawed/videos"
        );
        assert_eq!(
            normalise_url("https://www.youtube.com/channel/UCx/"),
            "https://www.youtube.com/channel/UCx/videos"
        );
        for same in [
            "https://www.youtube.com/@jawed/streams",
            "https://www.youtube.com/playlist?list=PL1",
            "https://vimeo.com/user",
        ] {
            assert_eq!(normalise_url(same), same);
        }
    }

    #[test]
    fn listing() {
        let out = br#"{"_type":"url","id":"a","url":"https://www.youtube.com/watch?v=a","title":"A","playlist_uploader":"Chan","ie_key":"Youtube"}
not json
{"_type":"url","id":"b","url":"https://www.youtube.com/watch?v=b","title":"B","ie_key":"Youtube"}
"#;
        let (e, title) = parse_listing(out);
        assert_eq!(e.len(), 2);
        assert_eq!(title.as_deref(), Some("Chan"));
    }
}
