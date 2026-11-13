//! Subscriptions: followed channels, playlists or any URL yt-dlp lists. Each check lists
//! the newest entries with `--flat-playlist` and queues only IDs not seen before.
use crate::db::Db;
use crate::error::Result;
use crate::util::unix_now;
use crate::ytdlp::{DownloadOptions, Entry};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleChoice {
    /// Whatever the preset says.
    #[default]
    Preset,
    On,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FirstCheck {
    /// Download the newest items (up to the item limit) on the first check.
    #[default]
    Latest,
    /// Only download what is published after the subscription was added.
    OnlyNew,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubConfig {
    /// Preset name; empty uses the default preset.
    pub preset: String,
    /// Folder for this subscription (relative to the download folder, or absolute).
    pub folder: String,
    /// Maximum video height; 0 keeps the preset's choice.
    pub max_height: u32,
    /// Only videos uploaded on or after this date (`YYYYMMDD`).
    pub date_after: String,
    /// Comma-separated keywords; a title must contain at least one (case-insensitive).
    pub include: String,
    /// Comma-separated keywords; a title containing any of them is skipped.
    pub exclude: String,
    /// How many of the newest items each check looks at.
    pub max_items: u32,
    pub subtitles: SubtitleChoice,
    pub interval_minutes: u32,
    pub first_check: FirstCheck,
}

impl Default for SubConfig {
    fn default() -> Self {
        Self {
            preset: String::new(),
            folder: String::new(),
            max_height: 0,
            date_after: String::new(),
            include: String::new(),
            exclude: String::new(),
            max_items: 10,
            subtitles: SubtitleChoice::Preset,
            interval_minutes: 60,
            first_check: FirstCheck::Latest,
        }
    }
}

impl SubConfig {
    pub fn sanitised(mut self) -> Self {
        self.max_items = self.max_items.clamp(1, 500);
        self.interval_minutes = self.interval_minutes.clamp(5, 60 * 24 * 7);
        self
    }

    /// Applies this subscription's overrides to a preset's options.
    pub fn apply(&self, mut o: DownloadOptions) -> DownloadOptions {
        if !self.folder.trim().is_empty() {
            o.folder = self.folder.trim().to_owned();
        }
        if self.max_height > 0 {
            o.max_height = self.max_height;
        }
        if !self.date_after.trim().is_empty() {
            o.date_after = self.date_after.trim().to_owned();
        }
        match self.subtitles {
            SubtitleChoice::Preset => {}
            SubtitleChoice::On => {
                o.subtitles = true;
                o.auto_subtitles = true;
            }
            SubtitleChoice::Off => {
                o.subtitles = false;
                o.auto_subtitles = false;
            }
        }
        o
    }
}

fn keywords(s: &str) -> Vec<String> {
    s.split(',')
        .map(|k| k.trim().to_lowercase())
        .filter(|k| !k.is_empty())
        .collect()
}

/// Title filter: at least one include keyword (if any are set) and no exclude keyword.
pub fn title_passes(title: &str, include: &str, exclude: &str) -> bool {
    let t = title.to_lowercase();
    let inc = keywords(include);
    let exc = keywords(exclude);
    (inc.is_empty() || inc.iter().any(|k| t.contains(k))) && !exc.iter().any(|k| t.contains(k))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Subscription {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub config: SubConfig,
    pub paused: bool,
    pub last_check: i64,
    pub next_check: i64,
    pub created_at: i64,
}

impl Subscription {
    /// When the next check is due (unix seconds): never for paused ones.
    pub fn due_at(&self) -> Option<i64> {
        if self.paused {
            return None;
        }
        Some(if self.last_check == 0 {
            0
        } else {
            self.next_check
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogEntry {
    pub ts: i64,
    pub is_error: bool,
    pub message: String,
}

/// What a check decided: entries to queue, and IDs to remember as seen.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Plan {
    pub queue: Vec<Entry>,
    pub seen: Vec<String>,
    pub filtered: usize,
}

/// Decides what to download from a listing (newest first). Every listed ID is remembered,
/// so filtered or old items are not reconsidered on the next check.
pub fn plan(
    entries: &[Entry],
    already_seen: &dyn Fn(&str) -> bool,
    cfg: &SubConfig,
    first: bool,
) -> Plan {
    let mut p = Plan::default();
    for e in entries
        .iter()
        .filter(|e| !e.is_playlist && !e.id.is_empty())
    {
        if already_seen(&e.id) || p.seen.contains(&e.id) {
            continue;
        }
        p.seen.push(e.id.clone());
        if first && cfg.first_check == FirstCheck::OnlyNew {
            continue;
        }
        if !title_passes(&e.title, &cfg.include, &cfg.exclude) {
            p.filtered += 1;
            continue;
        }
        if p.queue.len() < cfg.max_items as usize {
            p.queue.push(e.clone());
        }
    }
    p
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Subscription> {
    let cfg: String = r.get(3)?;
    Ok(Subscription {
        id: r.get(0)?,
        name: r.get(1)?,
        url: r.get(2)?,
        config: serde_json::from_str::<SubConfig>(&cfg)
            .unwrap_or_default()
            .sanitised(),
        paused: r.get(4)?,
        last_check: r.get(5)?,
        next_check: r.get(6)?,
        created_at: r.get(7)?,
    })
}

const COLS: &str = "id, name, url, config_json, paused, last_check, next_check, created_at";

pub fn list(db: &Db) -> Result<Vec<Subscription>> {
    let mut st = db.conn.prepare(&format!(
        "SELECT {COLS} FROM subscriptions ORDER BY name COLLATE NOCASE"
    ))?;
    let rows = st.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(db: &Db, id: i64) -> Result<Option<Subscription>> {
    Ok(db
        .conn
        .query_row(
            &format!("SELECT {COLS} FROM subscriptions WHERE id = ?1"),
            [id],
            from_row,
        )
        .optional()?)
}

pub fn insert(db: &Db, name: &str, url: &str, cfg: &SubConfig) -> Result<i64> {
    db.conn.execute(
        "INSERT INTO subscriptions(name, url, config_json, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![name, url, serde_json::to_string(cfg)?, unix_now()],
    )?;
    Ok(db.conn.last_insert_rowid())
}

pub fn update(db: &Db, id: i64, name: &str, url: &str, cfg: &SubConfig) -> Result<()> {
    db.conn.execute(
        "UPDATE subscriptions SET name = ?2, url = ?3, config_json = ?4 WHERE id = ?1",
        params![id, name, url, serde_json::to_string(cfg)?],
    )?;
    Ok(())
}

pub fn set_paused(db: &Db, id: i64, paused: bool) -> Result<()> {
    db.conn.execute(
        "UPDATE subscriptions SET paused = ?2 WHERE id = ?1",
        params![id, paused],
    )?;
    Ok(())
}

pub fn delete(db: &Db, id: i64) -> Result<()> {
    db.conn
        .execute("DELETE FROM subscriptions WHERE id = ?1", [id])?;
    Ok(())
}

pub fn set_checked(db: &Db, id: i64, at: i64, next: i64) -> Result<()> {
    db.conn.execute(
        "UPDATE subscriptions SET last_check = ?2, next_check = ?3 WHERE id = ?1",
        params![id, at, next],
    )?;
    Ok(())
}

pub fn is_seen(db: &Db, id: i64, video_id: &str) -> bool {
    db.conn
        .query_row(
            "SELECT 1 FROM sub_seen WHERE subscription_id = ?1 AND video_id = ?2",
            params![id, video_id],
            |_| Ok(()),
        )
        .is_ok()
}

pub fn mark_seen(db: &mut Db, id: i64, video_ids: &[String]) -> Result<()> {
    let tx = db.conn.transaction()?;
    {
        let mut st = tx.prepare(
            "INSERT OR IGNORE INTO sub_seen(subscription_id, video_id, seen_at) VALUES (?1, ?2, ?3)",
        )?;
        let now = unix_now();
        for v in video_ids {
            st.execute(params![id, v, now])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn seen_count(db: &Db, id: i64) -> i64 {
    db.conn
        .query_row(
            "SELECT COUNT(*) FROM sub_seen WHERE subscription_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap_or(0)
}

/// Adds a log line and keeps only the newest 50 per subscription.
pub fn log(db: &Db, id: i64, is_error: bool, message: &str) -> Result<()> {
    db.conn.execute(
        "INSERT INTO sub_log(subscription_id, ts, is_error, message) VALUES (?1, ?2, ?3, ?4)",
        params![id, unix_now(), is_error, message],
    )?;
    db.conn.execute(
        "DELETE FROM sub_log WHERE subscription_id = ?1 AND id NOT IN \
         (SELECT id FROM sub_log WHERE subscription_id = ?1 ORDER BY id DESC LIMIT 50)",
        [id],
    )?;
    Ok(())
}

pub fn logs(db: &Db, id: i64, limit: usize) -> Result<Vec<LogEntry>> {
    let mut st = db.conn.prepare(
        "SELECT ts, is_error, message FROM sub_log WHERE subscription_id = ?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = st.query_map(params![id, limit as i64], |r| {
        Ok(LogEntry {
            ts: r.get(0)?,
            is_error: r.get(1)?,
            message: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: &str, title: &str) -> Entry {
        Entry {
            id: id.into(),
            url: format!("https://x/{id}"),
            title: title.into(),
            channel: String::new(),
            duration: None,
            is_playlist: false,
        }
    }

    #[test]
    fn filters() {
        assert!(title_passes("Rust Tutorial 5", "", ""));
        assert!(title_passes("Rust Tutorial 5", "tutorial, guide", ""));
        assert!(!title_passes("Live stream", "tutorial", ""));
        assert!(!title_passes("Rust Tutorial #shorts", "", "#shorts, live"));
        assert!(title_passes("x", " , ", " ,"));
    }

    #[test]
    fn planning() {
        let list = vec![
            e("a", "New video"),
            e("b", "Live now"),
            e("c", "Older"),
            e("d", "Oldest"),
        ];
        let cfg = SubConfig {
            exclude: "live".into(),
            max_items: 2,
            ..Default::default()
        };
        let none = |_: &str| false;
        let p = plan(&list, &none, &cfg, true);
        assert_eq!(
            p.queue.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert_eq!(p.seen.len(), 4, "all listed IDs are remembered");
        assert_eq!(p.filtered, 1);

        let seen = |id: &str| id != "z";
        let mut list2 = list.clone();
        list2.insert(0, e("z", "Brand new"));
        let p = plan(&list2, &seen, &cfg, false);
        assert_eq!(p.queue.len(), 1);
        assert_eq!(p.queue[0].id, "z");

        let only_new = SubConfig {
            first_check: FirstCheck::OnlyNew,
            ..Default::default()
        };
        let p = plan(&list, &none, &only_new, true);
        assert!(p.queue.is_empty());
        assert_eq!(p.seen.len(), 4);
        let p = plan(&list2, &seen, &only_new, false);
        assert_eq!(p.queue.len(), 1);
    }

    #[test]
    fn overrides() {
        let cfg = SubConfig {
            folder: "Chan".into(),
            max_height: 720,
            date_after: "20240101".into(),
            subtitles: SubtitleChoice::Off,
            ..Default::default()
        };
        let o = cfg.apply(DownloadOptions::default());
        assert_eq!(o.folder, "Chan");
        assert_eq!(o.max_height, 720);
        assert_eq!(o.date_after, "20240101");
        assert!(!o.subtitles && !o.auto_subtitles);
    }

    #[test]
    fn storage_seen_and_logs() {
        let mut db = Db::open_memory().unwrap();
        let id = insert(&db, "Chan", "https://x/c", &SubConfig::default()).unwrap();
        let s = get(&db, id).unwrap().unwrap();
        assert_eq!(s.due_at(), Some(0), "never checked: due now");
        set_checked(&db, id, 100, 3700).unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().due_at(), Some(3700));
        set_paused(&db, id, true).unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().due_at(), None);
        mark_seen(&mut db, id, &["a".into(), "b".into(), "a".into()]).unwrap();
        assert!(is_seen(&db, id, "a"));
        assert!(!is_seen(&db, id, "c"));
        assert_eq!(seen_count(&db, id), 2);
        for i in 0..60 {
            log(&db, id, i % 2 == 0, &format!("m{i}")).unwrap();
        }
        let l = logs(&db, id, 100).unwrap();
        assert_eq!(l.len(), 50);
        assert_eq!(l[0].message, "m59");
        delete(&db, id).unwrap();
        assert!(list(&db).unwrap().is_empty());
        assert_eq!(seen_count(&db, id), 0, "seen IDs cascade");
    }
}
