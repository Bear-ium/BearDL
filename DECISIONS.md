# Decisions

- **eframe glow, not wgpu.** eframe 0.37 defaults to wgpu. glow is several MB smaller and gives mpv's OpenGL render API a GL context.
- **No `default_fonts`.** Bundled egui fonts add ~1 MB. The app loads the platform UI font at startup (Segoe UI and Segoe UI Symbol on Windows, SF/Helvetica on macOS, DejaVu/Noto/Liberation on Linux). The optional `bundled-fonts` feature restores egui's fonts for systems without any of them.
- **No accesskit.** It saves size, but costs screen-reader support. This can be revisited.
- **Release `opt-level = "s"`.** Baseline window: z = 3.96 MB, s = 3.36 MB, 3 = 3.90 MB. "s" is smallest here. This is re-measured in M9.
- **System curl and tar instead of an HTTP/TLS crate.** Windows 10 1803+, macOS and nearly all Linux distros ship both. On Windows they are called from `%SystemRoot%\System32` so Git's copies don't shadow them. This saves ~2 MB of TLS stack. Downloads are verified with SHA-256 (`sha2`).
- **libmpv via hand-written FFI and `libloading`.** `libmpv2`/`libmpv-sys` link at build time, so the exe would not start without the DLL.
- **Tray.** Windows uses a hand-rolled `Shell_NotifyIconW` (windows-sys is already in the tree via winit), which also provides balloon notifications. macOS and Linux use `tray-icon`, with the `ksni` backend on Linux so GTK is not needed. Notifications on Linux and macOS go through `notify-send` and `osascript`, not notify-rust and its zbus stack.
- **Global rate limit.** yt-dlp only limits per process, so each process gets `global / concurrency`. The approximation is documented in the UI.
- **Title filters are comma-separated keywords** (case-insensitive substring match), not regex. This avoids the regex crate.
- **Cold start is measured from launch to the second painted frame**, using wall-clock timestamps written by `BEARDL_STARTUP_LOG`. Process teardown is excluded: the GL driver takes ~700 ms to unload. About 200 ms of the 233 ms baseline is window and GL context creation, which egui cannot avoid.
- **Idle memory is ~70 MiB private at baseline.** Nearly all of it is the OpenGL driver (NVIDIA). The app's own heap is a few MiB.
- **M0 review folded into M1's.** The M0 diff is a 100-line scaffold, so its review runs together with M1's.
