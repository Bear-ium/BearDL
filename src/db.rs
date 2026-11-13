//! SQLite storage: library/history, queue, presets, subscriptions and the FTS5 indexes.
use crate::error::Result;
use crate::util::unix_now;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS downloads (
    id INTEGER PRIMARY KEY,
    video_id TEXT NOT NULL DEFAULT '',
    extractor TEXT NOT NULL DEFAULT '',
    url TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    channel TEXT NOT NULL DEFAULT '',
    channel_id TEXT NOT NULL DEFAULT '',
    duration REAL,
    upload_date TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    thumbnail_url TEXT NOT NULL DEFAULT '',
    thumb_path TEXT NOT NULL DEFAULT '',
    file_path TEXT NOT NULL DEFAULT '',
    file_size INTEGER,
    format_note TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'queued',
    error TEXT NOT NULL DEFAULT '',
    preset TEXT NOT NULL DEFAULT '',
    options_json TEXT NOT NULL DEFAULT '{}',
    subscription_id INTEGER,
    chapters_json TEXT NOT NULL DEFAULT '',
    resume_pos REAL NOT NULL DEFAULT 0,
    queue_pos REAL NOT NULL DEFAULT 0,
    in_queue INTEGER NOT NULL DEFAULT 1,
    missing INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS downloads_status ON downloads(status, created_at);
CREATE INDEX IF NOT EXISTS downloads_file ON downloads(file_path) WHERE file_path != '';
CREATE INDEX IF NOT EXISTS downloads_queue ON downloads(in_queue, queue_pos);
CREATE INDEX IF NOT EXISTS downloads_video ON downloads(video_id);

CREATE VIRTUAL TABLE IF NOT EXISTS downloads_fts USING fts5(
    title, channel, description,
    content='downloads', content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS downloads_ai AFTER INSERT ON downloads BEGIN
    INSERT INTO downloads_fts(rowid, title, channel, description)
    VALUES (new.id, new.title, new.channel, new.description);
END;
CREATE TRIGGER IF NOT EXISTS downloads_ad AFTER DELETE ON downloads BEGIN
    INSERT INTO downloads_fts(downloads_fts, rowid, title, channel, description)
    VALUES ('delete', old.id, old.title, old.channel, old.description);
END;
CREATE TRIGGER IF NOT EXISTS downloads_au AFTER UPDATE OF title, channel, description ON downloads BEGIN
    INSERT INTO downloads_fts(downloads_fts, rowid, title, channel, description)
    VALUES ('delete', old.id, old.title, old.channel, old.description);
    INSERT INTO downloads_fts(rowid, title, channel, description)
    VALUES (new.id, new.title, new.channel, new.description);
END;

CREATE TABLE IF NOT EXISTS sub_tracks (
    id INTEGER PRIMARY KEY,
    download_id INTEGER NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
    lang TEXT NOT NULL,
    is_auto INTEGER NOT NULL DEFAULT 0,
    path TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS sub_tracks_download ON sub_tracks(download_id);
CREATE VIRTUAL TABLE IF NOT EXISTS cues USING fts5(
    text, download_id UNINDEXED, track_id UNINDEXED, start UNINDEXED, end UNINDEXED,
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TABLE IF NOT EXISTS presets (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    options_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS subscriptions (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    config_json TEXT NOT NULL DEFAULT '{}',
    paused INTEGER NOT NULL DEFAULT 0,
    last_check INTEGER NOT NULL DEFAULT 0,
    next_check INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sub_seen (
    subscription_id INTEGER NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    video_id TEXT NOT NULL,
    seen_at INTEGER NOT NULL,
    PRIMARY KEY (subscription_id, video_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS sub_log (
    id INTEGER PRIMARY KEY,
    subscription_id INTEGER NOT NULL REFERENCES subscriptions(id) ON DELETE CASCADE,
    ts INTEGER NOT NULL,
    is_error INTEGER NOT NULL DEFAULT 0,
    message TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS sub_log_sub ON sub_log(subscription_id, ts);

CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Running,
    Paused,
    Done,
    Failed,
    Cancelled,
    Skipped,
}

impl Status {
    pub const ALL: [Status; 7] = [
        Status::Queued,
        Status::Running,
        Status::Paused,
        Status::Done,
        Status::Failed,
        Status::Cancelled,
        Status::Skipped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Running => "running",
            Status::Paused => "paused",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
            Status::Skipped => "skipped",
        }
    }

    pub fn parse(s: &str) -> Status {
        Status::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .unwrap_or(Status::Failed)
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Queued => "Queued",
            Status::Running => "Downloading",
            Status::Paused => "Paused",
            Status::Done => "Done",
            Status::Failed => "Failed",
            Status::Cancelled => "Cancelled",
            Status::Skipped => "Skipped",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Download {
    pub id: i64,
    pub video_id: String,
    pub extractor: String,
    pub url: String,
    pub title: String,
    pub channel: String,
    pub channel_id: String,
    pub duration: Option<f64>,
    pub upload_date: String,
    pub description: String,
    pub thumbnail_url: String,
    pub thumb_path: String,
    pub file_path: String,
    pub file_size: Option<i64>,
    pub format_note: String,
    pub status: Option<Status>,
    pub error: String,
    pub preset: String,
    pub options_json: String,
    pub subscription_id: Option<i64>,
    pub chapters_json: String,
    pub resume_pos: f64,
    pub queue_pos: f64,
    pub in_queue: bool,
    pub missing: bool,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

const DOWNLOAD_COLS: &str = "id, video_id, extractor, url, title, channel, channel_id, duration, \
    upload_date, description, thumbnail_url, thumb_path, file_path, file_size, format_note, status, \
    error, preset, options_json, subscription_id, chapters_json, resume_pos, queue_pos, in_queue, \
    missing, created_at, finished_at";

fn download_from_row(r: &Row) -> rusqlite::Result<Download> {
    Ok(Download {
        id: r.get(0)?,
        video_id: r.get(1)?,
        extractor: r.get(2)?,
        url: r.get(3)?,
        title: r.get(4)?,
        channel: r.get(5)?,
        channel_id: r.get(6)?,
        duration: r.get(7)?,
        upload_date: r.get(8)?,
        description: r.get(9)?,
        thumbnail_url: r.get(10)?,
        thumb_path: r.get(11)?,
        file_path: r.get(12)?,
        file_size: r.get(13)?,
        format_note: r.get(14)?,
        status: Some(Status::parse(&r.get::<_, String>(15)?)),
        error: r.get(16)?,
        preset: r.get(17)?,
        options_json: r.get(18)?,
        subscription_id: r.get(19)?,
        chapters_json: r.get(20)?,
        resume_pos: r.get(21)?,
        queue_pos: r.get(22)?,
        in_queue: r.get(23)?,
        missing: r.get(24)?,
        created_at: r.get(25)?,
        finished_at: r.get(26)?,
    })
}

/// Lightweight row for the virtualised library list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibraryRow {
    pub id: i64,
    pub title: String,
    pub channel: String,
    pub duration: Option<f64>,
    pub file_size: Option<i64>,
    pub status: Option<Status>,
    pub created_at: i64,
    pub missing: bool,
    pub thumb_path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Date,
    Title,
    Channel,
    Duration,
    Size,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibraryQuery {
    pub text: String,
    pub status: Option<Status>,
    pub sort: SortKey,
    pub descending: bool,
    pub missing_only: bool,
    pub channel: String,
}

/// Turns free text into an FTS5 query: each word becomes a quoted prefix term.
pub fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split_whitespace()
        // Quotes would end the FTS5 string; control characters (NUL) break the query.
        .map(|w| {
            w.chars()
                .filter(|c| *c != '"' && !c.is_control())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// A subtitle track to index: language, whether it is automatic, file path and cues.
#[derive(Clone, Debug, PartialEq)]
pub struct SubTrack {
    pub lang: String,
    pub is_auto: bool,
    pub path: String,
    pub cues: Vec<Cue>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CueHit {
    pub download_id: i64,
    pub title: String,
    pub channel: String,
    pub start: f64,
    pub snippet: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubLog {
    pub ts: i64,
    pub is_error: bool,
    pub message: String,
}

/// Library sort indexes are partial on `status != 'queued'`, matching the library filter, so a
/// page deep into a sorted library walks only library rows.
const LIBRARY_INDEXES: &str = r#"
DROP INDEX IF EXISTS downloads_created;
DROP INDEX IF EXISTS downloads_title;
DROP INDEX IF EXISTS downloads_channel;
DROP INDEX IF EXISTS downloads_size;
DROP INDEX IF EXISTS downloads_duration;
CREATE INDEX IF NOT EXISTS lib_created ON downloads(created_at) WHERE status != 'queued';
CREATE INDEX IF NOT EXISTS lib_title ON downloads(title COLLATE NOCASE) WHERE status != 'queued';
CREATE INDEX IF NOT EXISTS lib_channel ON downloads(channel COLLATE NOCASE) WHERE status != 'queued';
CREATE INDEX IF NOT EXISTS lib_size ON downloads(file_size) WHERE status != 'queued';
CREATE INDEX IF NOT EXISTS lib_duration ON downloads(duration) WHERE status != 'queued';
CREATE INDEX IF NOT EXISTS lib_missing ON downloads(created_at) WHERE missing = 1;
"#;

const SCHEMA_VERSION: i64 = 2;

fn migrate(conn: &Connection) -> Result<()> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v < 2 {
        conn.execute_batch(LIBRARY_INDEXES)?;
        conn.execute_batch("ANALYZE")?;
    }
    if v != SCHEMA_VERSION {
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
    }
    Ok(())
}

/// `(id, file_path, missing, file_size)`.
pub type FileRow = (i64, String, bool, Option<i64>);

pub struct Db {
    pub conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; \
             PRAGMA temp_store=MEMORY; PRAGMA cache_size=-16000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn kv_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Adds a queue entry and returns its id.
    pub fn insert_download(&self, d: &Download) -> Result<i64> {
        let pos: f64 = self.conn.query_row(
            "SELECT COALESCE(MAX(queue_pos), 0) + 1 FROM downloads WHERE in_queue = 1",
            [],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO downloads (video_id, extractor, url, title, channel, duration, thumbnail_url, \
             status, preset, options_json, subscription_id, queue_pos, in_queue, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13)",
            params![
                d.video_id,
                d.extractor,
                d.url,
                d.title,
                d.channel,
                d.duration,
                d.thumbnail_url,
                d.status.unwrap_or(Status::Queued).as_str(),
                d.preset,
                d.options_json,
                d.subscription_id,
                pos,
                if d.created_at > 0 { d.created_at } else { unix_now() },
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Adds many queue entries in one transaction; returns their ids in order.
    pub fn insert_downloads(&mut self, ds: &[Download]) -> Result<Vec<i64>> {
        let tx = self.conn.transaction()?;
        let mut pos: f64 = tx.query_row(
            "SELECT COALESCE(MAX(queue_pos), 0) FROM downloads WHERE in_queue = 1",
            [],
            |r| r.get(0),
        )?;
        let mut ids = Vec::with_capacity(ds.len());
        {
            let mut st = tx.prepare(
                "INSERT INTO downloads (video_id, extractor, url, title, channel, duration, thumbnail_url,                  status, preset, options_json, subscription_id, queue_pos, in_queue, created_at)                  VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13)",
            )?;
            let now = unix_now();
            for d in ds {
                pos += 1.0;
                st.execute(params![
                    d.video_id,
                    d.extractor,
                    d.url,
                    d.title,
                    d.channel,
                    d.duration,
                    d.thumbnail_url,
                    d.status.unwrap_or(Status::Queued).as_str(),
                    d.preset,
                    d.options_json,
                    d.subscription_id,
                    pos,
                    if d.created_at > 0 { d.created_at } else { now },
                ])?;
                ids.push(tx.last_insert_rowid());
            }
        }
        tx.commit()?;
        Ok(ids)
    }

    pub fn delete_downloads(&mut self, ids: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut cues = tx.prepare("DELETE FROM cues WHERE download_id = ?1")?;
            let mut rows = tx.prepare("DELETE FROM downloads WHERE id = ?1")?;
            for id in ids {
                cues.execute([id])?;
                rows.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_download(&self, id: i64) -> Result<Option<Download>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {DOWNLOAD_COLS} FROM downloads WHERE id = ?1"),
                [id],
                download_from_row,
            )
            .optional()?)
    }

    pub fn queue(&self) -> Result<Vec<Download>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {DOWNLOAD_COLS} FROM downloads WHERE in_queue = 1 ORDER BY queue_pos, id"
        ))?;
        let rows = st.query_map([], download_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_status(&self, id: i64, status: Status, error: &str) -> Result<()> {
        let finished = matches!(status, Status::Done | Status::Skipped).then(unix_now);
        self.conn.execute(
            "UPDATE downloads SET status = ?2, error = ?3, finished_at = COALESCE(?4, finished_at) WHERE id = ?1",
            params![id, status.as_str(), error, finished],
        )?;
        Ok(())
    }

    /// Stores metadata reported by yt-dlp for a download.
    pub fn update_meta(&self, id: i64, d: &Download) -> Result<()> {
        self.conn.execute(
            "UPDATE downloads SET video_id = ?2, extractor = ?3, title = ?4, channel = ?5, channel_id = ?6, \
             duration = ?7, upload_date = ?8, description = ?9, thumbnail_url = ?10, chapters_json = ?11, \
             format_note = ?12 WHERE id = ?1",
            params![
                id,
                d.video_id,
                d.extractor,
                d.title,
                d.channel,
                d.channel_id,
                d.duration,
                d.upload_date,
                d.description,
                d.thumbnail_url,
                d.chapters_json,
                d.format_note
            ],
        )?;
        Ok(())
    }

    pub fn set_file(&self, id: i64, path: &str, size: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE downloads SET file_path = ?2, file_size = ?3, missing = 0 WHERE id = ?1",
            params![id, path, size],
        )?;
        Ok(())
    }

    pub fn set_thumb(&self, id: i64, path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE downloads SET thumb_path = ?2 WHERE id = ?1",
            params![id, path],
        )?;
        Ok(())
    }

    pub fn set_resume(&self, id: i64, pos: f64) -> Result<()> {
        self.conn.execute(
            "UPDATE downloads SET resume_pos = ?2 WHERE id = ?1",
            params![id, pos],
        )?;
        Ok(())
    }

    pub fn set_queue_pos(&self, id: i64, pos: f64) -> Result<()> {
        self.conn.execute(
            "UPDATE downloads SET queue_pos = ?2 WHERE id = ?1",
            params![id, pos],
        )?;
        Ok(())
    }

    /// Rewrites queue positions to match `ids` order.
    pub fn reorder_queue(&mut self, ids: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare("UPDATE downloads SET queue_pos = ?2 WHERE id = ?1")?;
            for (i, id) in ids.iter().enumerate() {
                st.execute(params![id, i as f64 + 1.0])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_from_queue(&self, id: i64) -> Result<()> {
        self.conn
            .execute("UPDATE downloads SET in_queue = 0 WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Removes finished entries from the queue view; they stay in history.
    pub fn clear_finished(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE downloads SET in_queue = 0 WHERE in_queue = 1 AND status IN ('done', 'skipped')",
            [],
        )?)
    }

    /// Entries interrupted by a crash or exit are put back to queued.
    pub fn reset_running(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE downloads SET status = 'queued' WHERE status = 'running'",
            [],
        )?;
        Ok(())
    }

    pub fn delete_download(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM cues WHERE download_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM downloads WHERE id = ?1", [id])?;
        Ok(())
    }

    /// How many rows other than `except` point at `path`. Files shared by several rows
    /// (re-downloads, re-linked files) are never deleted while still referenced.
    pub fn file_refs(&self, path: &str, except: &[i64]) -> Result<usize> {
        let mut st = self
            .conn
            .prepare_cached("SELECT id FROM downloads WHERE file_path = ?1 OR thumb_path = ?1")?;
        let ids = st.query_map([path], |r| r.get::<_, i64>(0))?;
        let mut n = 0;
        for id in ids {
            if !except.contains(&id?) {
                n += 1;
            }
        }
        Ok(n)
    }

    pub fn set_missing(&mut self, flags: &[(i64, bool)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare("UPDATE downloads SET missing = ?2 WHERE id = ?1")?;
            for (id, m) in flags {
                st.execute(params![id, m])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// `(id, file_path, missing, file_size)` for every finished entry, for the
    /// moved/deleted file scan.
    pub fn files(&self) -> Result<Vec<FileRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, file_path, missing, file_size FROM downloads \
             WHERE status = 'done' AND file_path != ''",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn library_where(q: &LibraryQuery) -> (String, Vec<rusqlite::types::Value>) {
        use rusqlite::types::Value;
        let mut clauses = vec!["d.status != 'queued'".to_owned()];
        let mut args: Vec<Value> = Vec::new();
        if let Some(fts) = fts_query(&q.text) {
            args.push(Value::Text(fts));
            clauses.push(format!(
                "d.id IN (SELECT rowid FROM downloads_fts WHERE downloads_fts MATCH ?{})",
                args.len()
            ));
        }
        if let Some(s) = q.status {
            args.push(Value::Text(s.as_str().into()));
            clauses.push(format!("d.status = ?{}", args.len()));
        }
        if q.missing_only {
            clauses.push("d.missing = 1".into());
        }
        if !q.channel.is_empty() {
            args.push(Value::Text(q.channel.clone()));
            clauses.push(format!("d.channel = ?{} COLLATE NOCASE", args.len()));
        }
        (clauses.join(" AND "), args)
    }

    pub fn library_count(&self, q: &LibraryQuery) -> Result<usize> {
        let (w, args) = Self::library_where(q);
        let n: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM downloads d WHERE {w}"),
            rusqlite::params_from_iter(args),
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// One page of library rows. Paged with LIMIT/OFFSET over indexed sort columns.
    pub fn library_page(
        &self,
        q: &LibraryQuery,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<LibraryRow>> {
        let (w, args) = Self::library_where(q);
        let col = match q.sort {
            SortKey::Date => "d.created_at",
            SortKey::Title => "d.title COLLATE NOCASE",
            SortKey::Channel => "d.channel COLLATE NOCASE",
            SortKey::Duration => "d.duration",
            SortKey::Size => "d.file_size",
        };
        let dir = if q.descending { "DESC" } else { "ASC" };
        let sql = format!(
            "SELECT d.id, d.title, d.channel, d.duration, d.file_size, d.status, d.created_at, \
             d.missing, d.thumb_path FROM downloads d WHERE {w} ORDER BY {col} {dir}, d.id {dir} \
             LIMIT {limit} OFFSET {offset}"
        );
        let mut st = self.conn.prepare_cached(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(args), |r| {
            Ok(LibraryRow {
                id: r.get(0)?,
                title: r.get(1)?,
                channel: r.get(2)?,
                duration: r.get(3)?,
                file_size: r.get(4)?,
                status: Some(Status::parse(&r.get::<_, String>(5)?)),
                created_at: r.get(6)?,
                missing: r.get(7)?,
                thumb_path: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn presets(&self) -> Result<Vec<(i64, String, String)>> {
        let mut st = self
            .conn
            .prepare("SELECT id, name, options_json FROM presets ORDER BY name COLLATE NOCASE")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn upsert_preset(&self, name: &str, options_json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO presets(name, options_json) VALUES (?1, ?2) \
             ON CONFLICT(name) DO UPDATE SET options_json = excluded.options_json",
            params![name, options_json],
        )?;
        Ok(())
    }

    pub fn rename_preset(&self, old: &str, new: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE presets SET name = ?2 WHERE name = ?1",
            params![old, new],
        )?;
        Ok(())
    }

    pub fn delete_preset(&self, name: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM presets WHERE name = ?1", [name])?;
        Ok(())
    }

    /// Replaces the subtitle index of a download: its tracks and every cue, in one
    /// transaction. Returns the number of cues stored.
    pub fn store_subtitles(&mut self, download_id: i64, tracks: &[SubTrack]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM cues WHERE download_id = ?1", [download_id])?;
        tx.execute(
            "DELETE FROM sub_tracks WHERE download_id = ?1",
            [download_id],
        )?;
        let mut n = 0;
        {
            let mut ins_track = tx.prepare(
                "INSERT INTO sub_tracks(download_id, lang, is_auto, path) VALUES (?1, ?2, ?3, ?4)",
            )?;
            let mut ins_cue = tx.prepare(
                "INSERT INTO cues(text, download_id, track_id, start, end) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for t in tracks {
                ins_track.execute(params![download_id, t.lang, t.is_auto, t.path])?;
                let track_id = tx.last_insert_rowid();
                for c in &t.cues {
                    ins_cue.execute(params![c.text, download_id, track_id, c.start, c.end])?;
                    n += 1;
                }
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Subtitle files kept for a download (to delete with it).
    pub fn sub_track_paths(&self, download_id: i64) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare("SELECT path FROM sub_tracks WHERE download_id = ?1")?;
        let rows = st.query_map([download_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Searches spoken words. `fts` is an FTS5 query (see `subtitles::cue_query`). Results
    /// are ranked; the snippet marks matches with \u{2} ... \u{3}.
    pub fn search_cues(&self, fts: &str, limit: usize) -> Result<Vec<CueHit>> {
        let mut st = self.conn.prepare_cached(
            "SELECT c.download_id, d.title, d.channel, c.start, \
             snippet(cues, 0, char(2), char(3), '…', 14) \
             FROM cues c JOIN downloads d ON d.id = c.download_id \
             WHERE cues MATCH ?1 ORDER BY rank LIMIT ?2",
        )?;
        let rows = st.query_map(params![fts, limit as i64], |r| {
            Ok(CueHit {
                download_id: r.get(0)?,
                title: r.get(1)?,
                channel: r.get(2)?,
                start: r.get(3)?,
                snippet: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// (indexed downloads, cues) for the search page.
    pub fn cue_stats(&self) -> Result<(i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT (SELECT COUNT(DISTINCT download_id) FROM sub_tracks), \
             (SELECT COUNT(*) FROM sub_tracks)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    pub fn clear_presets(&self) -> Result<()> {
        self.conn.execute("DELETE FROM presets", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dl(title: &str, status: Status) -> Download {
        Download {
            url: format!("https://example.com/{title}"),
            title: title.into(),
            channel: "Chan".into(),
            status: Some(status),
            ..Default::default()
        }
    }

    #[test]
    fn queue_order_and_status() {
        let mut db = Db::open_memory().unwrap();
        let batch = db
            .insert_downloads(&[dl("x", Status::Queued), dl("y", Status::Queued)])
            .unwrap();
        assert_eq!(batch.len(), 2);
        db.delete_downloads(&batch).unwrap();
        assert!(db.queue().unwrap().is_empty());
        let a = db.insert_download(&dl("a", Status::Queued)).unwrap();
        let b = db.insert_download(&dl("b", Status::Queued)).unwrap();
        let c = db.insert_download(&dl("c", Status::Queued)).unwrap();
        let ids: Vec<i64> = db.queue().unwrap().iter().map(|d| d.id).collect();
        assert_eq!(ids, vec![a, b, c]);
        db.reorder_queue(&[c, a, b]).unwrap();
        let ids: Vec<i64> = db.queue().unwrap().iter().map(|d| d.id).collect();
        assert_eq!(ids, vec![c, a, b]);

        db.set_status(a, Status::Running, "").unwrap();
        db.reset_running().unwrap();
        assert_eq!(
            db.get_download(a).unwrap().unwrap().status,
            Some(Status::Queued)
        );
        db.set_status(b, Status::Done, "").unwrap();
        assert!(db.get_download(b).unwrap().unwrap().finished_at.is_some());
        assert_eq!(db.clear_finished().unwrap(), 1);
        assert_eq!(db.queue().unwrap().len(), 2);
        assert!(db.get_download(b).unwrap().is_some());
    }

    #[test]
    fn library_search_sort_filter() {
        let db = Db::open_memory().unwrap();
        for (t, ch, st) in [
            ("Rust tutorial", "Alpha", Status::Done),
            ("Cooking pasta", "Beta", Status::Done),
            ("Rustic café tour", "Beta", Status::Failed),
            ("Waiting", "Beta", Status::Queued),
        ] {
            let mut d = dl(t, st);
            d.channel = ch.into();
            db.insert_download(&d).unwrap();
        }
        let q = |text: &str| LibraryQuery {
            text: text.into(),
            sort: SortKey::Title,
            ..Default::default()
        };
        assert_eq!(db.library_count(&q("")).unwrap(), 3);
        let titles: Vec<String> = db
            .library_page(&q("rust"), 0, 10)
            .unwrap()
            .into_iter()
            .map(|r| r.title)
            .collect();
        assert_eq!(titles, vec!["Rust tutorial", "Rustic café tour"]);
        assert_eq!(db.library_count(&q("cafe")).unwrap(), 1);
        assert_eq!(db.library_count(&q("\"")).unwrap(), 3);
        let mut f = q("");
        f.status = Some(Status::Done);
        f.channel = "beta".into();
        assert_eq!(db.library_count(&f).unwrap(), 1);
        let mut s = q("");
        s.descending = true;
        assert_eq!(
            db.library_page(&s, 0, 1).unwrap()[0].title,
            "Rustic café tour"
        );
        assert_eq!(db.library_page(&s, 2, 5).unwrap().len(), 1);
    }

    #[test]
    fn fts_follows_updates_and_deletes() {
        let db = Db::open_memory().unwrap();
        let id = db.insert_download(&dl("old name", Status::Done)).unwrap();
        let mut d = db.get_download(id).unwrap().unwrap();
        d.title = "brand new".into();
        db.update_meta(id, &d).unwrap();
        let q = |t: &str| LibraryQuery {
            text: t.into(),
            ..Default::default()
        };
        assert_eq!(db.library_count(&q("old")).unwrap(), 0);
        assert_eq!(db.library_count(&q("brand")).unwrap(), 1);
        db.delete_download(id).unwrap();
        assert_eq!(db.library_count(&q("brand")).unwrap(), 0);
    }

    #[test]
    fn subtitle_index_and_search() {
        let mut db = Db::open_memory().unwrap();
        let id = db.insert_download(&dl("Zoo video", Status::Done)).unwrap();
        let other = db.insert_download(&dl("Other", Status::Done)).unwrap();
        let track = |lang: &str, text: &[(&str, f64)]| SubTrack {
            lang: lang.into(),
            is_auto: false,
            path: format!("/subs/{lang}.vtt"),
            cues: text
                .iter()
                .map(|(t, s)| Cue {
                    start: *s,
                    end: s + 2.0,
                    text: (*t).into(),
                })
                .collect(),
        };
        let n = db
            .store_subtitles(
                id,
                &[track(
                    "en",
                    &[
                        ("all right so here we are", 1.0),
                        ("in front of the elephants", 4.5),
                    ],
                )],
            )
            .unwrap();
        assert_eq!(n, 2);
        db.store_subtitles(other, &[track("en", &[("elephants are big", 9.0)])])
            .unwrap();
        let hits = db.search_cues("\"elephant\"*", 10).unwrap();
        assert_eq!(hits.len(), 2);
        let zoo = hits.iter().find(|h| h.download_id == id).unwrap();
        assert_eq!(zoo.start, 4.5);
        assert_eq!(zoo.title, "Zoo video");
        assert!(zoo.snippet.contains('\u{2}'));
        assert_eq!(db.search_cues("\"front of the\"", 10).unwrap().len(), 1);
        assert_eq!(db.cue_stats().unwrap(), (2, 2));
        // Re-indexing replaces; deleting the download removes its cues.
        db.store_subtitles(id, &[track("de", &[("hallo", 0.0)])])
            .unwrap();
        assert_eq!(db.search_cues("\"elephant\"*", 10).unwrap().len(), 1);
        assert_eq!(db.sub_track_paths(id).unwrap(), vec!["/subs/de.vtt"]);
        db.delete_downloads(&[other]).unwrap();
        assert!(db.search_cues("\"elephant\"*", 10).unwrap().is_empty());
        assert_eq!(db.cue_stats().unwrap(), (1, 1));
    }

    #[test]
    fn presets_and_kv() {
        let db = Db::open_memory().unwrap();
        db.upsert_preset("A", "{}").unwrap();
        db.upsert_preset("A", "{\"x\":1}").unwrap();
        db.upsert_preset("b", "{}").unwrap();
        let p = db.presets().unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].2, "{\"x\":1}");
        db.rename_preset("b", "C").unwrap();
        db.delete_preset("A").unwrap();
        assert_eq!(db.presets().unwrap()[0].1, "C");
        assert_eq!(db.kv_get("k").unwrap(), None);
        db.kv_set("k", "1").unwrap();
        db.kv_set("k", "2").unwrap();
        assert_eq!(db.kv_get("k").unwrap().as_deref(), Some("2"));
    }
}
