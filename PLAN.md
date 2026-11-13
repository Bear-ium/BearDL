# BearDL plan

A tiny native yt-dlp manager. Rust, egui/eframe (glow backend), tokio, SQLite with FTS5. No webview.

## Crate layout

One package, `beardl`, with a library (`src/lib.rs`, everything testable) and a thin binary (`src/main.rs`).

```
src/
  main.rs            entry: arg parsing, single-instance hint, eframe launch
  lib.rs             module tree
  paths.rs           config/data/cache dirs per platform (std only)
  util.rs            time formatting, byte/duration formatting, shell-word splitting, open folder/file
  db/                SQLite schema, migrations, queries (library, queue, subs, cues, presets)
  tools/             tool manager: yt-dlp and ffmpeg download, checksum, update, rollback, channels
  net.rs             thin wrapper over system curl (and tar for extraction)
  ytdlp/             argument building from presets, progress-template and JSON parsing, formats
  engine/            tokio runtime thread: download queue, scheduler, subscription checks, jobs
  subtitles.rs       VTT and SRT parsing into cues
  thumbs.rs          thumbnail cache (ffmpeg downscale), lazy JPEG decode
  settings.rs        settings.toml model
  theme.rs           TOML themes, built-in light/dark, user themes folder
  export.rs          settings/presets/subscriptions/themes export and import (merge or replace)
  trim.rs            ffmpeg cut by time or chapter
  player/            libmpv dynamic loading (FFI), GL render into an FBO texture, external mpv fallback
  platform/          tray, notifications, autostart, process-tree kill (cfg per OS)
  ui/                egui views: downloads, library, quick view, player, search, subscriptions, settings
```

Threads: the UI thread (eframe) and one engine thread running a current-thread tokio runtime. They talk over channels: UI sends `Command`s, engine updates a shared `Arc<Mutex<EngineState>>` snapshot and calls `ctx.request_repaint()` (coalesced to at most ~10 Hz while downloads run). Nothing ticks when idle. The scheduler is a single `sleep_until(next_due)` that is woken by a `Notify` when subscriptions change.

## Dependencies and expected size impact

| Crate | Purpose | Expected release size |
|---|---|---|
| eframe 0.37 (glow, clipboard, x11, wayland; no default fonts, no accesskit, no wgpu) | GUI | ~3.3 MB baseline |
| tokio (rt, process, io-util, sync, time, macros) | child processes, scheduler | ~250 KB |
| rusqlite 0.42 (bundled) | storage + FTS5 | ~1 MB |
| serde, serde_json | yt-dlp JSON, export file | ~150 KB |
| toml | settings, themes | ~150 KB |
| sha2 | download checksums | ~20 KB |
| libloading | dynamic libmpv | ~10 KB |
| jpeg-decoder | thumbnail decode with DCT downscaling | ~80 KB |
| windows-sys 0.61 (already in tree via winit) | Win32 tray + balloons, autostart registry | ~30 KB |
| tray-icon (macOS, Linux via ksni only) | tray on non-Windows | 0 on Windows |

Avoided: reqwest/ureq/rustls (system curl instead, ~2 MB saved), image (jpeg-decoder only), notify-rust (Win32 balloon / notify-send / osascript), rfd (path fields), chrono (hand-rolled civil date), regex (keyword filters), dirs/opener (std).

## Data model

```
downloads(id PK, video_id, extractor, url, title, channel, channel_id, duration REAL,
          upload_date, description, thumbnail_url, thumb_path, file_path, file_size,
          format_note, status, error, preset_id, subscription_id, chapters_json,
          resume_pos REAL, queue_pos REAL, options_json, created_at, finished_at)
  indexes: created_at, status, channel, title COLLATE NOCASE, queue_pos
downloads_fts  FTS5 external content (title, channel, description) with triggers
sub_tracks(id PK, download_id, lang, is_auto, path)
cues  FTS5 (text, download_id UNINDEXED, start UNINDEXED, end UNINDEXED)
presets(id PK, name UNIQUE, options_json, custom_args)
subscriptions(id PK, name, url, preset_id, folder, max_height, date_after, include_terms,
              exclude_terms, max_items, subtitles, interval_min, paused, last_check,
              next_check, created_at)
sub_seen(subscription_id, video_id, seen_at, PK(subscription_id, video_id))
sub_log(id PK, subscription_id, ts, level, message)
kv(key PK, value)
```

Settings live in `settings.toml`, themes in `themes/*.toml`, both under the config dir.

## Milestone verification

- M0: fmt, clippy, test pass; release builds; `scripts/measure.ps1` records size, cold start (`--bench-exit`), idle memory and idle CPU.
- M1: unit tests for checksum parsing and asset selection; manual: first run downloads yt-dlp, `--version` works, update and rollback swap binaries.
- M2: unit tests for progress line parsing, format JSON parsing, argument building, custom-arg validation; manual: real download with progress, pause/resume/cancel/retry, playlist selection.
- M3: DB tests for insert/search/sort/filter, missing-file detection; manual quick view with spacebar.
- M4: theme TOML round-trip and export/import merge/replace tests; manual live theme switch.
- M5: manual playback with libmpv (downloaded for testing), external mpv fallback path; tests for chapter parsing.
- M6: VTT/SRT parser tests, FTS query tests; manual click-to-timestamp.
- M7: scheduler due-time tests, filter tests, seen-ID tests; manual tray, notification, check now.
- M8: trim argument tests; manual cut by time and chapter.
- M9: 100,000-row library benchmark, idle CPU profile, release profile comparison, README.
