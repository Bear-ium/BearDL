# BearDL

A small, fast, native desktop app for [yt-dlp](https://github.com/yt-dlp/yt-dlp). It is written in Rust with egui (OpenGL) and has no Electron and no web view. A release build is about 5.6 MiB, paints its first frame about 250 ms after launch, and uses no CPU while idle.

yt-dlp and ffmpeg are not bundled. BearDL downloads them on first run into its data folder, verifies their checksums, and keeps them up to date.

## Features

- **Downloads**
  - Analyse any URL to see every format.
  - Choose video, audio or both. The default is best video plus best audio, merged.
  - Remux to MP4, MKV, WebM or MOV, or convert audio to MP3, M4A, Opus, FLAC or WAV.
  - No limits on how many downloads you add. Concurrency is configurable (default 3), with an optional global rate limit.
- **Queue**
  - Live progress, speed and ETA.
  - Pause, resume, cancel, retry and reorder.
  - For a playlist or channel, pick which items to download.
- **Presets**
  - Named yt-dlp option sets, with a raw custom-arguments field.
  - Custom arguments are checked locally, then parsed by yt-dlp itself.
- **Library and history**
  - Every download is recorded: title, channel, duration, thumbnail, size, date, source, path and status.
  - Search, sort and filter, delete (optionally with the file), re-download and show in folder.
  - Files that were moved are found again, and deleted ones are flagged.
  - Handles 100,000 entries.
- **Quick view**: select an item and press Space to see its thumbnail, metadata, description and chapters without opening a player.
- **Player**
  - Embedded libmpv playback with chapters, audio and subtitle tracks, speed, volume and fullscreen.
  - Playback resumes where you left off, with a metadata panel alongside.
  - Metadata, cover art and chapters are embedded in downloaded files.
- **Spoken-word search**
  - Manual subtitles and automatic captions are downloaded and every line is indexed.
  - Search for what was said, then click a result to play from that moment.
- **Subscriptions**
  - Follow channels, playlists or any URL yt-dlp can list.
  - Each subscription has its own preset, folder, quality, date cutoff, title filters, item limit, subtitle option and check interval.
  - Only new uploads are fetched.
  - Runs in the tray, can start with the system, and sends notifications for new downloads. Each subscription has Check now, Pause and a log.
- **Tool updates**
  - yt-dlp is updated on start and on a schedule from the stable, nightly or master channel, with checksum checks.
  - Rollback is one click, and a rolled-back version is not reinstalled automatically.
- **Themes**
  - Built-in light and dark themes.
  - Your own TOML themes (colours, fonts, spacing) in the themes folder, switched live.
- **Export and import**: one file holds settings, presets, subscriptions and themes. Import can merge or replace.
- **Trimming**
  - Cut by start and end time or by chapter.
  - Preview the cut as a loop in the player.
  - Fast mode copies the streams and snaps to keyframes. Accurate mode re-encodes.

## Requirements to run

- **Windows 10 1803 or later.** This includes the `curl` and `tar` that BearDL uses to download tools.
- **macOS 11 or later.** `curl` and `tar` are built in.
- **Linux** with OpenGL 3 (or GLES/EGL), `curl`, and `tar` with xz support.
  - Window system libraries, loaded at runtime. On Debian/Ubuntu: `libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libwayland-egl1 libx11-6 libxcursor1 libxi6 libxrandr2 libxcb1 libegl1 libgl1`. Desktop installs normally have them.
  - To extract the yt-dlp archive: `unzip` or `python3`. Without either, BearDL falls back to the single-file build.
  - Optional: `notify-send` for notifications, and a desktop with StatusNotifierItem tray support (KDE, or GNOME with the AppIndicator extension).
- **Embedded player (optional):** libmpv.
  - Windows: press **Download libmpv** on the Player page or in Settings, under Tools. Or put `libmpv-2.dll` next to `beardl.exe`.
  - macOS: `brew install mpv`.
  - Linux: install your distribution's `libmpv2` (or `mpv-libs`) package.
  
  Without libmpv, files open in an external `mpv`, or in your system player.

## Network access

Apart from the downloads you ask yt-dlp for, BearDL connects to the internet only for:

- **yt-dlp**: installing it on first run and checking for updates, from the yt-dlp releases on GitHub.
- **ffmpeg**: installing it when it is missing, or when you press its button. The builds come from yt-dlp's FFmpeg-Builds on GitHub, or from ffmpeg.martin-riedl.de on macOS.
- **libmpv (Windows)**: only when you press **Download libmpv**. BearDL asks the GitHub API for the latest [mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake/releases) release, then downloads its `mpv-dev` archive from GitHub.

yt-dlp and ffmpeg are checked against their published SHA-256 checksums, and libmpv against the SHA-256 that GitHub lists for the file. There is no telemetry.

## Building

You need the latest stable Rust (1.99 or newer) and a C compiler; SQLite is compiled in.

### Windows

1. Install the Visual Studio Build Tools with the "Desktop development with C++" workload.
2. Install Rust from rustup.rs.
3. Build:

```
cargo build --release
```

The program is `target\release\beardl.exe`.

### macOS

```
xcode-select --install
cargo build --release
```

### Linux

```
sudo apt install build-essential    # Fedora: sudo dnf install gcc
cargo build --release
```

No development packages are needed: the window system, OpenGL and the tray (DBus) are all loaded at runtime, and no GTK is used.

### Optional features

- `bundled-fonts`: includes egui's own fonts, for systems without any of the common UI fonts BearDL looks for.
- `dev-shots`: development only. It saves timed screenshots for checking the UI; see `scripts/shot.ps1`.

## Development

```
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

- **Network tests** download real tools and media. They are ignored by default:

  ```
  cargo test --test live -- --ignored --test-threads 1 --skip tray_notification
  ```

  `tray_notification_and_autostart` shows a real tray icon and notification, and briefly adds and removes the system start-up entry. Run it on its own when you mean to.
- **The 100,000-row performance test:**

  ```
  cargo test --release --test library_db -- --ignored --nocapture library_100k
  ```
- **Measurements:** `scripts/measure.ps1` measures binary size, cold start (launch to first painted frame), idle memory and idle CPU on Windows.

`BEARDL_HOME=<folder>` keeps every file BearDL writes in one folder, which is useful for tests and portable use. `BEARDL_LIBMPV=<path>` points at a specific libmpv: only that file is tried, and it takes precedence over **Custom libmpv** in Settings.

Project notes: `PLAN.md` (structure and data model), `DECISIONS.md` (choices and trade-offs) and `PROGRESS.md` (milestones and measurements).

## Where files go

| | Windows | macOS | Linux |
|---|---|---|---|
| Settings, themes | `%APPDATA%\BearDL` | `~/Library/Application Support/BearDL` | `~/.config/BearDL` |
| Library database, tools | `%LOCALAPPDATA%\BearDL` | `~/Library/Application Support/BearDL` | `~/.local/share/BearDL` |
| Thumbnail and subtitle cache | `%LOCALAPPDATA%\BearDL\cache` | `~/Library/Caches/BearDL` | `~/.cache/BearDL` |
| Downloads (default) | `Downloads\BearDL` | `~/Downloads/BearDL` | `~/Downloads/BearDL` |

## Licence

BSD Zero Clause. See `LICENSE`. yt-dlp and ffmpeg are downloaded from their official releases and keep their own licences.
