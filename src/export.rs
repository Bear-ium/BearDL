//! Settings export and import: one JSON file with settings, presets, subscriptions and
//! user themes. Import either merges into what is here or replaces it.
use crate::db::Db;
use crate::error::Result;
use crate::settings::Settings;
use crate::ytdlp::Preset;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const FORMAT: &str = "beardl-export";
pub const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionRecord {
    pub name: String,
    pub url: String,
    /// Subscription options as stored (kept as raw JSON so exports survive schema changes).
    pub config: serde_json::Value,
    pub paused: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemeFile {
    pub file_name: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bundle {
    pub format: String,
    pub version: u32,
    pub exported_at: i64,
    pub settings: Option<Settings>,
    #[serde(default)]
    pub presets: Vec<Preset>,
    #[serde(default)]
    pub subscriptions: Vec<SubscriptionRecord>,
    #[serde(default)]
    pub themes: Vec<ThemeFile>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportMode {
    /// Add and update; keep everything not in the file. Machine-specific paths stay local.
    Merge,
    /// Make presets, subscriptions and themes match the file exactly.
    Replace,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportSummary {
    pub settings: Option<Settings>,
    pub presets: usize,
    pub subscriptions: usize,
    pub themes: usize,
    pub warnings: Vec<String>,
}

impl ImportSummary {
    pub fn describe(&self) -> String {
        let mut s = format!(
            "Imported {} preset(s), {} subscription(s), {} theme(s){}",
            self.presets,
            self.subscriptions,
            self.themes,
            if self.settings.is_some() {
                " and settings"
            } else {
                ""
            }
        );
        for w in &self.warnings {
            s.push('\n');
            s.push_str(w);
        }
        s
    }
}

pub fn subscriptions(db: &Db) -> Result<Vec<SubscriptionRecord>> {
    let mut st = db
        .conn
        .prepare("SELECT name, url, config_json, paused FROM subscriptions ORDER BY id")?;
    let rows = st.query_map([], |r| {
        let cfg: String = r.get(2)?;
        Ok(SubscriptionRecord {
            name: r.get(0)?,
            url: r.get(1)?,
            config: serde_json::from_str(&cfg).unwrap_or(serde_json::Value::Null),
            paused: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Collects everything to export. Built-in themes are not included; user theme files are.
pub fn build(db: &Db, settings: &Settings, themes_dir: &Path) -> Result<Bundle> {
    let presets = db
        .presets()?
        .into_iter()
        .map(|(_, name, json)| Preset {
            name,
            options: serde_json::from_str(&json).unwrap_or_default(),
        })
        .collect();
    let mut themes = Vec::new();
    if let Ok(rd) = std::fs::read_dir(themes_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
                && let Ok(content) = std::fs::read_to_string(&p)
            {
                themes.push(ThemeFile {
                    file_name: e.file_name().to_string_lossy().into_owned(),
                    content,
                });
            }
        }
    }
    themes.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    Ok(Bundle {
        format: FORMAT.into(),
        version: VERSION,
        exported_at: crate::util::unix_now(),
        settings: Some(settings.clone()),
        presets,
        subscriptions: subscriptions(db)?,
        themes,
    })
}

/// Writes atomically, so a failed export never leaves a half-written file.
pub fn write(path: &Path, bundle: &Bundle) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(bundle)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Exports are small; anything much larger is not one of ours.
const MAX_IMPORT_BYTES: u64 = 16 * 1024 * 1024;

pub fn read(path: &Path) -> Result<Bundle> {
    if std::fs::metadata(path)?.len() > MAX_IMPORT_BYTES {
        crate::bail!("the file is too large to be a BearDL export");
    }
    let bytes = std::fs::read(path)?;
    let b: Bundle = serde_json::from_slice(&bytes)
        .map_err(|e| crate::error::Error(format!("not a BearDL export: {e}")))?;
    if b.format != FORMAT {
        crate::bail!("not a BearDL export file");
    }
    if b.version > VERSION {
        crate::bail!(
            "this export is from a newer BearDL (format {}); update BearDL first",
            b.version
        );
    }
    Ok(b)
}

/// A theme file name that stays inside the themes folder.
fn safe_theme_name(name: &str) -> Option<String> {
    let base = Path::new(name).file_name()?.to_str()?;
    let clean = crate::util::sanitise_file_name(base);
    clean
        .to_ascii_lowercase()
        .ends_with(".toml")
        .then_some(clean)
}

/// A folder or template that stays inside the download folder: relative, no `..`, no drive.
pub fn is_safe_relative(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return true;
    }
    let p = Path::new(t);
    !p.is_absolute()
        && !p.has_root()
        && !t.starts_with(['/', '\\', '~'])
        && !t.contains('$')
        && !(t.len() >= 2 && t.as_bytes()[1] == b':')
        && !t.split(['/', '\\']).any(|c| c.trim() == "..")
        && plain_template_fields(t)
}

/// yt-dlp expands environment variables (`%VAR%`, `$VAR`) and template replacements
/// (`%(field&text|default)s`) in paths. Imported values may only use `%%` and plain
/// `%(field)x` fields, so nothing can expand into a path outside the download folder.
fn plain_template_fields(t: &str) -> bool {
    let b = t.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            i += 1;
            continue;
        }
        match b.get(i + 1) {
            Some(b'%') => i += 2,
            Some(b'(') => {
                let Some(close) = t[i + 2..].find(')') else {
                    return false;
                };
                let field = &t[i + 2..i + 2 + close];
                if field.is_empty()
                    || !field
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b','))
                {
                    return false;
                }
                // The conversion letter after `)`.
                let conv = b.get(i + 3 + close);
                if !conv.is_some_and(|c| c.is_ascii_alphabetic()) {
                    return false;
                }
                i += close + 4;
            }
            _ => return false,
        }
    }
    true
}

/// A preset name not used yet: "Name (imported)", then "Name (imported 2)", ...
fn free_preset_name(tx: &rusqlite::Transaction, name: &str) -> Result<String> {
    for i in 1..1000 {
        let candidate = if i == 1 {
            format!("{name} (imported)")
        } else {
            format!("{name} (imported {i})")
        };
        let taken: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM presets WHERE name = ?1)",
            [&candidate],
            |r| r.get(0),
        )?;
        if !taken {
            return Ok(candidate);
        }
    }
    crate::bail!("too many imported copies of preset {name}")
}

/// Clears `folder` and `output_template` strings inside an imported JSON value that would
/// write outside the download folder. Returns what was removed.
fn sanitise_paths(v: &mut serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if matches!(k.as_str(), "folder" | "output_template") {
                    let bad = val.as_str().is_none_or(|s| !is_safe_relative(s));
                    if bad {
                        out.push(k.clone());
                        *val = serde_json::Value::String(String::new());
                    }
                } else {
                    out.extend(sanitise_paths(val));
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                out.extend(sanitise_paths(item));
            }
        }
        _ => {}
    }
    out
}

/// Clears any `custom_args` string inside an imported JSON value that fails the import
/// allowlist. Returns the reasons.
fn sanitise_custom_args(v: &mut serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if k == "custom_args" {
                    let bad = match val.as_str() {
                        Some(a) => crate::ytdlp::check_imported_args(a).err(),
                        None => Some("not text".into()),
                    };
                    if let Some(e) = bad {
                        out.push(e);
                        *val = serde_json::Value::String(String::new());
                    }
                } else {
                    out.extend(sanitise_custom_args(val));
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                out.extend(sanitise_custom_args(item));
            }
        }
        _ => {}
    }
    out
}

/// Applies a bundle. Returns the settings the app should adopt (if any) and a summary.
pub fn apply(
    db: &mut Db,
    bundle: &Bundle,
    current: &Settings,
    themes_dir: &Path,
    mode: ImportMode,
) -> Result<ImportSummary> {
    let mut sum = ImportSummary::default();
    if let Some(s) = &bundle.settings {
        let mut s = s.clone();
        // Programs the app runs are never taken from a file: an export could point them
        // at anything. These always stay as configured on this machine.
        s.ytdlp_path = current.ytdlp_path.clone();
        s.ffmpeg_path = current.ffmpeg_path.clone();
        s.mpv_path = current.mpv_path.clone();
        s.libmpv_path = current.libmpv_path.clone();
        // Where files are written is not taken from a file either.
        s.download_dir = current.download_dir.clone();
        s.output_template = current.output_template.clone();
        sum.settings = Some(s.sanitised());
    }

    let tx = db.conn.transaction()?;
    if mode == ImportMode::Replace {
        tx.execute("DELETE FROM presets", [])?;
    }
    for p in &bundle.presets {
        let mut name = p.name.trim().to_owned();
        if name.is_empty() {
            continue;
        }
        let mut options = p.options.clone();
        for (what, value) in [
            ("folder", &mut options.folder),
            ("file name template", &mut options.output_template),
        ] {
            if !is_safe_relative(value) {
                sum.warnings.push(format!(
                    "Preset “{name}”: {what} removed (must stay inside the download folder)."
                ));
                value.clear();
            }
        }
        // Imported presets keep custom arguments only if every option is on the allowlist.
        if let Err(e) = crate::ytdlp::check_imported_args(&options.custom_args) {
            sum.warnings
                .push(format!("Preset “{name}”: custom arguments removed ({e})."));
            options.custom_args.clear();
        }
        let json = serde_json::to_string(&options)?;
        if mode == ImportMode::Merge {
            // Never overwrite a different local preset of the same name.
            let existing: Option<String> = tx
                .query_row(
                    "SELECT options_json FROM presets WHERE name = ?1",
                    [&name],
                    |r| r.get(0),
                )
                .ok();
            if existing.is_some_and(|e| e != json) {
                name = free_preset_name(&tx, &name)?;
            }
        }
        tx.execute(
            "INSERT INTO presets(name, options_json) VALUES (?1, ?2) \
             ON CONFLICT(name) DO UPDATE SET options_json = excluded.options_json",
            params![name, json],
        )?;
        sum.presets += 1;
    }

    // Subscriptions are matched by URL, so their seen-video history survives a replace.
    let now = crate::util::unix_now();
    for s in &bundle.subscriptions {
        if s.url.trim().is_empty() {
            continue;
        }
        let mut config = s.config.clone();
        for w in sanitise_paths(&mut config) {
            sum.warnings.push(format!(
                "Subscription “{}”: {w} removed (must stay inside the download folder).",
                s.name
            ));
        }
        for w in sanitise_custom_args(&mut config) {
            sum.warnings.push(format!(
                "Subscription “{}”: custom arguments removed ({w}).",
                s.name
            ));
        }
        let cfg = config.to_string();
        let updated = tx.execute(
            "UPDATE subscriptions SET name = ?2, config_json = ?3, paused = ?4 WHERE url = ?1",
            params![s.url, s.name, cfg, s.paused],
        )?;
        if updated == 0 {
            tx.execute(
                "INSERT INTO subscriptions(name, url, config_json, paused, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![s.name, s.url, cfg, s.paused, now],
            )?;
        }
        sum.subscriptions += 1;
    }
    if mode == ImportMode::Replace {
        let keep: Vec<&str> = bundle
            .subscriptions
            .iter()
            .map(|s| s.url.as_str())
            .collect();
        let existing: Vec<(i64, String)> = {
            let mut st = tx.prepare("SELECT id, url FROM subscriptions")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (id, url) in existing {
            if !keep.contains(&url.as_str()) {
                tx.execute("DELETE FROM subscriptions WHERE id = ?1", [id])?;
            }
        }
    }
    tx.commit()?;

    std::fs::create_dir_all(themes_dir)?;
    if mode == ImportMode::Replace
        && let Ok(rd) = std::fs::read_dir(themes_dir)
    {
        for e in rd.flatten() {
            if e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    for t in &bundle.themes {
        let Some(name) = safe_theme_name(&t.file_name) else {
            sum.warnings.push(format!(
                "Skipped theme with an unsafe name: {}",
                t.file_name
            ));
            continue;
        };
        let mut theme = match crate::theme::Theme::parse(&t.content) {
            Ok(th) => th,
            Err(e) => {
                sum.warnings
                    .push(format!("Skipped invalid theme {name}: {e}"));
                continue;
            }
        };
        // A font path from another machine could point anywhere; drop it.
        let mut content = t.content.clone();
        if theme.fonts.family.take().is_some() {
            sum.warnings.push(format!(
                "Theme {name}: custom font removed (set it again in the theme file)."
            ));
            content = toml::to_string_pretty(&theme).unwrap_or_default();
        }
        let mut target = themes_dir.join(&name);
        if mode == ImportMode::Merge
            && target.is_file()
            && std::fs::read_to_string(&target).ok().as_deref() != Some(content.as_str())
        {
            // Keep the local file; store the imported one beside it.
            let stem = name.trim_end_matches(".toml");
            target = (1..)
                .map(|i| {
                    themes_dir.join(format!(
                        "{stem}-imported{}.toml",
                        if i == 1 {
                            String::new()
                        } else {
                            format!("-{i}")
                        }
                    ))
                })
                .find(|p| !p.exists())
                .unwrap_or(target);
        }
        match std::fs::write(&target, content) {
            Ok(()) => sum.themes += 1,
            Err(e) => sum
                .warnings
                .push(format!("Could not write theme {name}: {e}")),
        }
    }
    Ok(sum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ytdlp::DownloadOptions;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("beardl-export-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn add_sub(db: &Db, name: &str, url: &str) {
        db.conn
            .execute(
                "INSERT INTO subscriptions(name, url, config_json, created_at) VALUES (?1, ?2, '{\"max_items\":5}', 0)",
                params![name, url],
            )
            .unwrap();
    }

    #[test]
    fn round_trip_merge_and_replace() {
        let d = tmp("rt");
        let themes = d.join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(themes.join("mine.toml"), "name = \"Mine\"\n").unwrap();
        let src = Db::open_memory().unwrap();
        src.upsert_preset(
            "A",
            &serde_json::to_string(&DownloadOptions::default()).unwrap(),
        )
        .unwrap();
        add_sub(&src, "Chan", "https://example.com/c");
        let settings = Settings {
            concurrency: 7,
            ffmpeg_path: "/evil/ffmpeg".into(),
            libmpv_path: "/evil/libmpv.so".into(),
            ..Default::default()
        };
        let b = build(&src, &settings, &themes).unwrap();
        let file = d.join("export.json");
        write(&file, &b).unwrap();
        let back = read(&file).unwrap();
        assert_eq!(back, b);
        assert_eq!(back.subscriptions[0].config["max_items"], 5);

        // Merge keeps local entries and local paths.
        let mut dst = Db::open_memory().unwrap();
        dst.upsert_preset("Local", "{}").unwrap();
        add_sub(&dst, "Other", "https://example.com/o");
        let here = Settings {
            download_dir: "/local".into(),
            ffmpeg_path: "/local/ffmpeg".into(),
            ..Default::default()
        };
        let dst_themes = d.join("dst_themes");
        let sum = apply(&mut dst, &back, &here, &dst_themes, ImportMode::Merge).unwrap();
        assert_eq!((sum.presets, sum.subscriptions, sum.themes), (1, 1, 1));
        let s = sum.settings.unwrap();
        assert_eq!(s.concurrency, 7);
        assert_eq!(s.download_dir, std::path::PathBuf::from("/local"));
        assert_eq!(s.ffmpeg_path, "/local/ffmpeg", "tool paths never imported");
        assert_eq!(s.libmpv_path, "", "the library path is never imported");
        assert_eq!(dst.presets().unwrap().len(), 2);
        assert_eq!(subscriptions(&dst).unwrap().len(), 2);
        assert!(dst_themes.join("mine.toml").is_file());

        // Merging again does not duplicate.
        apply(&mut dst, &back, &here, &dst_themes, ImportMode::Merge).unwrap();
        assert_eq!(subscriptions(&dst).unwrap().len(), 2);

        // Replace makes it match, keeping the matching subscription's row.
        let id_before: i64 = dst
            .conn
            .query_row(
                "SELECT id FROM subscriptions WHERE url = 'https://example.com/c'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        std::fs::write(dst_themes.join("extra.toml"), "name = \"Extra\"\n").unwrap();
        apply(&mut dst, &back, &here, &dst_themes, ImportMode::Replace).unwrap();
        let p = dst.presets().unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].1, "A");
        let subs = subscriptions(&dst).unwrap();
        assert_eq!(subs.len(), 1);
        let id_after: i64 = dst
            .conn
            .query_row("SELECT id FROM subscriptions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(id_before, id_after);
        assert!(!dst_themes.join("extra.toml").exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn rejects_bad_files_and_unsafe_themes() {
        let d = tmp("bad");
        let f = d.join("x.json");
        std::fs::write(
            &f,
            "{\"format\":\"other\",\"version\":1,\"exported_at\":0,\"settings\":null}",
        )
        .unwrap();
        assert!(read(&f).is_err());
        std::fs::write(&f, "not json").unwrap();
        assert!(read(&f).is_err());
        std::fs::write(
            &f,
            "{\"format\":\"beardl-export\",\"version\":99,\"exported_at\":0,\"settings\":null}",
        )
        .unwrap();
        assert!(read(&f).unwrap_err().0.contains("newer"));

        assert_eq!(
            safe_theme_name("../../evil.toml").as_deref(),
            Some("evil.toml")
        );
        assert_eq!(safe_theme_name("ok.toml").as_deref(), Some("ok.toml"));
        assert!(safe_theme_name("run.exe").is_none());
        assert!(is_safe_relative("music/%(uploader)s"));
        assert!(is_safe_relative(""));
        assert!(!is_safe_relative("../up"));
        assert!(!is_safe_relative("a/../../b"));
        assert!(is_safe_relative("%(uploader)s/%(title)s [%(id)s].%(ext)s"));
        assert!(is_safe_relative("100%% fun"));
        assert!(!is_safe_relative("%APPDATA%/x"));
        assert!(!is_safe_relative("$HOME/x"));
        assert!(!is_safe_relative("~/x"));
        assert!(!is_safe_relative("%(id&../../x|)s"));
        assert!(!is_safe_relative("%(title"));
        assert!(!is_safe_relative("/etc"));
        assert!(!is_safe_relative("C:\\Users\\x"));
        assert!(!is_safe_relative("\\\\host\\share"));
        let mut cfg = serde_json::json!({"folder": "../x", "inner": {"output_template": "C:/t.%(ext)s"}, "ok": {"folder": "subs"}});
        assert_eq!(sanitise_paths(&mut cfg).len(), 2);
        assert_eq!(cfg["ok"]["folder"], "subs");
        let bundle = Bundle {
            format: FORMAT.into(),
            version: 1,
            exported_at: 0,
            settings: None,
            presets: vec![Preset {
                name: "X".into(),
                options: DownloadOptions {
                    custom_args: "--exec 'rm -rf /'".into(),
                    ..Default::default()
                },
            }],
            subscriptions: vec![],
            themes: vec![
                ThemeFile {
                    file_name: "../outside.toml".into(),
                    content: "name = \"Out\"\n".into(),
                },
                ThemeFile {
                    file_name: "broken.toml".into(),
                    content: "name = ".into(),
                },
            ],
        };
        let mut db = Db::open_memory().unwrap();
        let themes = d.join("themes");
        let sum = apply(
            &mut db,
            &bundle,
            &Settings::default(),
            &themes,
            ImportMode::Merge,
        )
        .unwrap();
        assert!(sum.warnings.iter().any(|w| w.contains("--exec")));
        let mut cfg = serde_json::json!({
            "max_items": 3,
            "options": {"custom_args": "--alias x --exec"},
            "list": [{"custom_args": "-N 4"}]
        });
        let warnings = sanitise_custom_args(&mut cfg);
        assert_eq!(warnings.len(), 1);
        assert_eq!(cfg["options"]["custom_args"], "");
        assert_eq!(cfg["list"][0]["custom_args"], "-N 4");
        let stored = db.presets().unwrap();
        let opts: DownloadOptions = serde_json::from_str(&stored[0].2).unwrap();
        assert!(opts.custom_args.is_empty(), "--exec stripped on import");
        assert!(sum.warnings.iter().any(|w| w.contains("invalid theme")));
        assert!(themes.join("outside.toml").is_file());
        assert!(!d.join("outside.toml").exists());
        std::fs::remove_dir_all(d).unwrap();
    }
}
