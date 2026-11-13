//! OS integration: tray icon with menu, desktop notifications, start with the OS, app icon.
use eframe::egui;
use std::sync::mpsc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    Show,
    CheckSubscriptions,
    Quit,
}

/// The app icon (rounded square with a download arrow), drawn in code so no image decoder
/// or bundled file is needed. RGBA, `size`×`size`.
pub fn app_icon(size: u32) -> Vec<u8> {
    let s = size as f32;
    let accent = [47u8, 127, 209];
    let mut out = vec![0u8; (size * size * 4) as usize];
    let inside_box = |x: f32, y: f32| {
        let r = 0.22 * s;
        let cx = x.clamp(r, s - r);
        let cy = y.clamp(r, s - r);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };
    let in_arrow = |x: f32, y: f32| {
        let (u, v) = (x / s, y / s);
        let shaft = (0.43..=0.57).contains(&u) && (0.18..=0.55).contains(&v);
        let head = (0.45..=0.72).contains(&v) && (u - 0.5).abs() <= 0.24 * (0.72 - v) / 0.27;
        let bar = (0.25..=0.75).contains(&u) && (0.78..=0.86).contains(&v);
        shaft || head || bar
    };
    for py in 0..size {
        for px in 0..size {
            // 4×4 supersampling for smooth edges.
            let (mut cover, mut white) = (0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = px as f32 + (sx as f32 + 0.5) / 4.0;
                    let y = py as f32 + (sy as f32 + 0.5) / 4.0;
                    if inside_box(x, y) {
                        cover += 1;
                        if in_arrow(x, y) {
                            white += 1;
                        }
                    }
                }
            }
            let i = ((py * size + px) * 4) as usize;
            if cover > 0 {
                let w = white as f32 / cover as f32;
                for c in 0..3 {
                    out[i + c] = (accent[c] as f32 * (1.0 - w) + 255.0 * w) as u8;
                }
                out[i + 3] = (cover * 255 / 16) as u8;
            }
        }
    }
    out
}

pub fn window_icon() -> egui::IconData {
    egui::IconData {
        rgba: app_icon(64),
        width: 64,
        height: 64,
    }
}

/// A tray icon. Events arrive on a channel; the UI is woken when one arrives.
pub struct Tray {
    rx: mpsc::Receiver<TrayEvent>,
    #[cfg(windows)]
    win: win::WinTray,
    #[cfg(not(windows))]
    _icon: tray_icon::TrayIcon,
}

impl Tray {
    pub fn new(ctx: &egui::Context) -> Result<Tray, String> {
        let (tx, rx) = mpsc::channel();
        #[cfg(windows)]
        {
            let win = win::WinTray::start(tx, ctx.clone())?;
            Ok(Tray { rx, win })
        }
        #[cfg(not(windows))]
        {
            use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
            let show = MenuItem::new("Show BearDL", true, None);
            let check = MenuItem::new("Check subscriptions now", true, None);
            let quit = MenuItem::new("Quit", true, None);
            let menu = Menu::new();
            menu.append_items(&[&show, &check, &PredefinedMenuItem::separator(), &quit])
                .map_err(|e| e.to_string())?;
            let ids = (show.id().clone(), check.id().clone(), quit.id().clone());
            let c = ctx.clone();
            let mtx = tx.clone();
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let ev = if e.id == ids.0 {
                    TrayEvent::Show
                } else if e.id == ids.1 {
                    TrayEvent::CheckSubscriptions
                } else if e.id == ids.2 {
                    TrayEvent::Quit
                } else {
                    return;
                };
                let _ = mtx.send(ev);
                c.request_repaint();
            }));
            let c = ctx.clone();
            tray_icon::TrayIconEvent::set_event_handler(Some(
                move |e: tray_icon::TrayIconEvent| {
                    if let tray_icon::TrayIconEvent::DoubleClick { .. } = e {
                        let _ = tx.send(TrayEvent::Show);
                        c.request_repaint();
                    }
                },
            ));
            let icon =
                tray_icon::Icon::from_rgba(app_icon(32), 32, 32).map_err(|e| e.to_string())?;
            let tray = tray_icon::TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("BearDL")
                .with_icon(icon)
                .build()
                .map_err(|e| e.to_string())?;
            Ok(Tray { rx, _icon: tray })
        }
    }

    pub fn events(&self) -> Vec<TrayEvent> {
        self.rx.try_iter().collect()
    }

    /// Shows a desktop notification.
    pub fn notify(&self, title: &str, body: &str) {
        #[cfg(windows)]
        self.win.balloon(title, body);
        #[cfg(not(windows))]
        notify(title, body);
    }
}

/// Desktop notification without a tray (macOS and Linux use the system tools).
pub fn notify(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            esc(body),
            esc(title)
        );
        let _ = crate::util::command("osascript")
            .arg("-e")
            .arg(script)
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = crate::util::command("notify-send")
            .args(["--app-name=BearDL", "--", title, body])
            .spawn();
    }
    #[cfg(windows)]
    {
        let _ = (title, body);
    }
}

/// Registers or removes the app from the OS's start-up list. It starts with `--minimised`.
pub fn set_autostart(enable: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        win::set_run_key(enable, &format!("\"{}\" --minimised", exe.display()))
    }
    #[cfg(target_os = "macos")]
    {
        let dir = crate::paths::home().join("Library/LaunchAgents");
        let file = dir.join("dev.beardl.app.plist");
        if !enable {
            let _ = std::fs::remove_file(&file);
            return Ok(());
        }
        let xml_esc = |s: &str| {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>dev.beardl.app</string>\n<key>ProgramArguments</key><array><string>{}</string><string>--minimised</string></array>\n<key>RunAtLoad</key><true/>\n</dict></plist>\n",
            xml_esc(&exe.to_string_lossy())
        );
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(file, plist).map_err(|e| e.to_string())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| crate::paths::home().join(".config"))
            .join("autostart");
        let file = dir.join("beardl.desktop");
        if !enable {
            let _ = std::fs::remove_file(&file);
            return Ok(());
        }
        let desktop = format!(
            "[Desktop Entry]\nType=Application\nName=BearDL\nExec=\"{}\" --minimised\nX-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(file, desktop).map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
mod win {
    //! Hand-rolled Win32 tray: a message-only window on its own thread owns the icon.
    //! `Shell_NotifyIconW` with `NIF_INFO` gives native toast-style notifications.
    use super::TrayEvent;
    use eframe::egui;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::{Mutex, OnceLock, mpsc};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
        NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    const WM_TRAY: u32 = WM_APP + 1;
    const ID_SHOW: usize = 1;
    const ID_CHECK: usize = 2;
    const ID_QUIT: usize = 3;

    type Sink = Mutex<Option<(mpsc::Sender<TrayEvent>, egui::Context)>>;
    static SINK: OnceLock<Sink> = OnceLock::new();
    static HWND_VALUE: AtomicIsize = AtomicIsize::new(0);
    static TASKBAR_CREATED: AtomicIsize = AtomicIsize::new(0);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn copy_into(dst: &mut [u16], s: &str) {
        let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
        dst[..w.len()].copy_from_slice(&w);
        dst[w.len()] = 0;
    }

    fn emit(ev: TrayEvent) {
        if let Some(m) = SINK.get()
            && let Ok(g) = m.lock()
            && let Some((tx, ctx)) = g.as_ref()
        {
            let _ = tx.send(ev);
            ctx.request_repaint();
        }
    }

    unsafe fn make_icon() -> HICON {
        let size = 32;
        let rgba = super::app_icon(size);
        // CreateIcon takes BGRA colour bits and a 1-bit AND mask (all zero: use alpha).
        let bgra: Vec<u8> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[2], p[1], p[0], p[3]])
            .collect();
        let mask = vec![0u8; (size * size / 8) as usize];
        // SAFETY: buffers match the size and bit depth given.
        unsafe {
            CreateIcon(
                GetModuleHandleW(std::ptr::null()),
                size as i32,
                size as i32,
                1,
                32,
                mask.as_ptr(),
                bgra.as_ptr(),
            )
        }
    }

    unsafe fn base_data(hwnd: HWND) -> NOTIFYICONDATAW {
        // SAFETY: plain-data struct; zero is a valid starting state.
        let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        d.hWnd = hwnd;
        d.uID = 1;
        d
    }

    unsafe fn add_icon(hwnd: HWND) {
        // SAFETY: valid window handle; the icon handle is owned by the shell after NIM_ADD.
        unsafe {
            let mut d = base_data(hwnd);
            d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            d.uCallbackMessage = WM_TRAY;
            d.hIcon = make_icon();
            copy_into(&mut d.szTip, "BearDL");
            Shell_NotifyIconW(NIM_ADD, &d);
        }
    }

    unsafe fn show_menu(hwnd: HWND) {
        // SAFETY: standard popup menu sequence on our own window.
        unsafe {
            let menu = CreatePopupMenu();
            let items = [
                (ID_SHOW, "Show BearDL"),
                (ID_CHECK, "Check subscriptions now"),
                (0, ""),
                (ID_QUIT, "Quit"),
            ];
            for (id, text) in items {
                if id == 0 {
                    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                } else {
                    let w = wide(text);
                    AppendMenuW(menu, MF_STRING, id, w.as_ptr());
                }
            }
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            // Needed so the menu closes when clicking elsewhere.
            SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
                pt.x,
                pt.y,
                0,
                hwnd,
                std::ptr::null(),
            );
            DestroyMenu(menu);
            match cmd as usize {
                ID_SHOW => emit(TrayEvent::Show),
                ID_CHECK => emit(TrayEvent::CheckSubscriptions),
                ID_QUIT => emit(TrayEvent::Quit),
                _ => {}
            }
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        let taskbar = TASKBAR_CREATED.load(Ordering::Relaxed) as u32;
        // SAFETY: called by Windows with valid arguments for our window class.
        unsafe {
            if msg == WM_TRAY {
                match (lp as u32) & 0xffff {
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK => emit(TrayEvent::Show),
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                    _ => {}
                }
                return 0;
            }
            if taskbar != 0 && msg == taskbar {
                // Explorer restarted: put the icon back.
                add_icon(hwnd);
                return 0;
            }
            if msg == WM_DESTROY {
                let d = base_data(hwnd);
                Shell_NotifyIconW(NIM_DELETE, &d);
                PostQuitMessage(0);
                return 0;
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
    }

    pub struct WinTray;

    impl WinTray {
        pub fn start(tx: mpsc::Sender<TrayEvent>, ctx: egui::Context) -> Result<WinTray, String> {
            let sink = SINK.get_or_init(|| Mutex::new(None));
            *sink.lock().map_err(|e| e.to_string())? = Some((tx, ctx));
            let (ready_tx, ready_rx) = mpsc::channel();
            std::thread::Builder::new()
                .name("tray".into())
                .spawn(move || {
                    // SAFETY: window class registration and message loop on this thread.
                    unsafe {
                        let class = wide("BearDLTray");
                        let hinst = GetModuleHandleW(std::ptr::null());
                        let wc = WNDCLASSW {
                            lpfnWndProc: Some(wndproc),
                            hInstance: hinst,
                            lpszClassName: class.as_ptr(),
                            // SAFETY: zero is valid for the remaining fields.
                            ..std::mem::zeroed()
                        };
                        RegisterClassW(&wc);
                        let hwnd = CreateWindowExW(
                            0,
                            class.as_ptr(),
                            class.as_ptr(),
                            0,
                            0,
                            0,
                            0,
                            0,
                            HWND_MESSAGE,
                            std::ptr::null_mut(),
                            hinst,
                            std::ptr::null(),
                        );
                        if hwnd.is_null() {
                            let _ = ready_tx.send(false);
                            return;
                        }
                        let tb = wide("TaskbarCreated");
                        TASKBAR_CREATED.store(
                            RegisterWindowMessageW(tb.as_ptr()) as isize,
                            Ordering::Relaxed,
                        );
                        HWND_VALUE.store(hwnd as isize, Ordering::SeqCst);
                        add_icon(hwnd);
                        let _ = ready_tx.send(true);
                        // Blocks in GetMessage while idle: no CPU use.
                        let mut msg: MSG = std::mem::zeroed();
                        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            match ready_rx.recv() {
                Ok(true) => Ok(WinTray),
                _ => Err("could not create the tray icon".into()),
            }
        }

        pub fn balloon(&self, title: &str, body: &str) {
            let hwnd = HWND_VALUE.load(Ordering::SeqCst) as HWND;
            if hwnd.is_null() {
                return;
            }
            // SAFETY: modifies our own icon; Shell_NotifyIconW is thread-safe.
            unsafe {
                let mut d = base_data(hwnd);
                d.uFlags = NIF_INFO;
                d.dwInfoFlags = NIIF_INFO;
                copy_into(&mut d.szInfoTitle, title);
                copy_into(&mut d.szInfo, body);
                Shell_NotifyIconW(NIM_MODIFY, &d);
            }
        }
    }

    impl Drop for WinTray {
        fn drop(&mut self) {
            let hwnd = HWND_VALUE.swap(0, Ordering::SeqCst) as HWND;
            if !hwnd.is_null() {
                // SAFETY: ask the tray thread to remove the icon and end its loop.
                unsafe {
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                }
            }
        }
    }

    pub fn set_run_key(enable: bool, command: &str) -> Result<(), String> {
        use windows_sys::Win32::System::Registry::*;
        let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
        let name = wide("BearDL");
        // SAFETY: registry calls with valid, NUL-terminated wide strings.
        let r = unsafe {
            if enable {
                let value = wide(command);
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    name.as_ptr(),
                    REG_SZ,
                    value.as_ptr() as *const _,
                    (value.len() * 2) as u32,
                )
            } else {
                let r = RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr());
                // Already absent is fine.
                if r == 2 { 0 } else { r }
            }
        };
        if r == 0 {
            Ok(())
        } else {
            Err(format!("registry error {r}"))
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_has_shape() {
        let px = super::app_icon(32);
        assert_eq!(px.len(), 32 * 32 * 4);
        // Corner is transparent, centre column is the white arrow, edge middle is accent.
        assert_eq!(px[3], 0);
        let at = |x: usize, y: usize| &px[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4];
        assert!(at(16, 10)[0] > 240);
        assert!(at(3, 16)[2] > 150 && at(3, 16)[0] < 100);
    }
}
