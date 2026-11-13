# Progress

Measurements on Windows 11, x86_64, release build. Cold start = launch to first painted frame (`--bench-exit`, median of 5). Idle memory and idle CPU are taken after a warm-up (5 s until M7, 30 s from then on, so the start-up update check is not counted as idle).

| Milestone | Status | Date | Binary | Cold start | Idle memory | Idle CPU |
|---|---|---|---|---|---|---|
| M0 Scaffold | done | 2026-11-13 | 3.21 MiB | 233 ms | 70.6 MiB private | 0 ms / 10 s |
| M1 Tool manager | done | 2026-11-13 | 3.75 MiB | 282 ms | 79.9 MiB private | 16 ms / 10 s |
| M2 Download core | done | 2026-11-13 | 4.89 MiB | 276 ms | 80.7 MiB private | 0 ms / 10 s |
| M3 Library and quick view | done | 2026-11-13 | 5.15 MiB | 265 ms | 79.6 MiB private | 0 ms / 10 s |
| M4 Settings, themes, export | done | 2026-11-13 | 5.33 MiB | 266 ms | 80.1 MiB private | 0 ms / 10 s |
| M5 Player and metadata | done | 2026-11-13 | 5.37 MiB | 267 ms | 79.8 MiB private | 0 ms / 10 s |
| M6 Subtitle indexing | done | 2026-11-13 | 5.43 MiB | 275 ms | 80.7 MiB private | 0 ms / 10 s |
| M7 Subscriptions | done | 2026-11-13 | 5.51 MiB | 252 ms | 79.2 MiB private | 0 ms / 10 s |
| M8 Trimming | done | 2026-11-13 | 5.55 MiB | 252 ms | 75.2 MiB private | 0 ms / 10 s |
| M9 Hardening and size | done | 2026-11-13 | 5.56 MiB | 254 ms | 74.3 MiB private | 0 ms / 10 s |

## Features (checked against the real app, 2026-11-13)

Checked through live end-to-end tests (`tests/live.rs`, real yt-dlp, ffmpeg and network) and screenshots of the running app.

- [x] 1 Unlimited downloads: no caps. Concurrency setting, global rate limit split per process (live: 2 at 40K measured about 20 KB/s each).
- [x] 2 All formats and max quality:
  - Analysis lists every format (24 for the test video), with video/audio pickers.
  - The default merges best video and best audio (live: an MKV with both streams).
  - Audio conversion to M4A tested live; remux is a preset option.
- [x] 3 Queue: live progress, speed, ETA, pause, resume (continues the `.part`), cancel (partials removed), retry, reorder; playlist and channel item selection (screenshot).
- [x] 4 Library and history:
  - Every field is recorded, with search, sort, filter, delete, re-download and show in folder.
  - Moved and deleted files are detected: 100k missing flagged in under 6 s.
  - 100,000 entries work in the UI.
- [x] 5 Quick view: thumbnail, metadata, description and chapters on selection or Space (screenshot).
- [x] 6 Player:
  - Embedded libmpv (screenshot of real playback), with chapters, audio and subtitle tracks, speed, resume and a details panel.
  - Metadata, cover and chapters are embedded (checked with ffprobe).
  - External mpv is the fallback.
- [x] 7 Subtitle indexing: manual and auto captions, VTT/SRT parser, FTS5 cues. Searching "elephants" finds the video at 0:01, and a click plays from there (screenshot).
- [x] 8 Subscriptions:
  - Per-subscription options, a `--flat-playlist` scheduler with seen IDs (live: the second check queued 0), and a notification.
  - Tray, notification and start-with-OS checked live on Windows. Close-to-tray and `--minimised` checked.
- [x] 9 yt-dlp updates: start-up and scheduled checks, checksum-verified install, stable/nightly/master, rollback (live).
- [x] 10 Presets: built-in and custom presets, custom arguments checked by yt-dlp (live: an unknown option is rejected).
- [x] 11 Themes: TOML themes, built-in light and dark, live switching, user themes folder (screenshots).
- [x] 12 Export and import: one JSON file, merge or replace (live round trip through the engine, plus unit tests).
- [x] 13 Trimming: by time or chapter, preview loop, fast and accurate modes (live: 5.08 s keyframe-aligned clip and an exact 3.00 s clip).

## Not verified here

- macOS and Linux builds: only the Windows build was compiled and run. The tray-icon/ksni code, notification commands and autostart files for those platforms were written against their APIs and reviewed, but never compiled or run.
