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
| M9 Hardening and size | in progress | | | | | |

## Features (checked against the real app in Phase 3)

- [ ] 1 Unlimited downloads
- [ ] 2 All formats and max quality
- [ ] 3 Queue
- [ ] 4 Library and history
- [ ] 5 Quick view
- [ ] 6 Player with metadata
- [ ] 7 Subtitle indexing
- [ ] 8 Subscriptions
- [ ] 9 yt-dlp updates
- [ ] 10 Presets
- [ ] 11 Themes
- [ ] 12 Export and import
- [ ] 13 Trimming
