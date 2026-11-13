//! Embedded player: libmpv renders through its OpenGL API into a texture that egui shows.
//! mpv asks for a repaint only when it has a new frame, so a paused player costs nothing.
pub mod ffi;

use eframe::egui;
use eframe::glow::{self, HasContext};
use ffi::{Mpv, cstr};
use std::ffi::{CStr, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

pub type GetProcAddress = Arc<dyn Fn(&CStr) -> *const c_void + Send + Sync>;

static LIBMPV: OnceLock<Result<Arc<Mpv>, String>> = OnceLock::new();

/// libmpv, loaded once on first use.
pub fn libmpv() -> Result<Arc<Mpv>, String> {
    LIBMPV.get_or_init(|| Mpv::load().map(Arc::new)).clone()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub lang: String,
    pub selected: bool,
}

impl Track {
    pub fn label(&self) -> String {
        let mut s = match (self.title.is_empty(), self.lang.is_empty()) {
            (false, false) => format!("{} ({})", self.title, self.lang),
            (false, true) => self.title.clone(),
            (true, false) => self.lang.clone(),
            (true, true) => format!("Track {}", self.id),
        };
        if s.chars().count() > 60 {
            s = s.chars().take(60).collect();
        }
        s
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChapterMark {
    pub time: f64,
    pub title: String,
}

struct Gl {
    texture: glow::Texture,
    fbo: glow::Framebuffer,
    size: [i32; 2],
    id: egui::TextureId,
}

pub struct Player {
    mpv: Arc<Mpv>,
    handle: *mut ffi::mpv_handle,
    render: *mut ffi::mpv_render_context,
    // Kept alive for the callbacks registered with mpv.
    _repaint: Box<egui::Context>,
    _proc: Box<GetProcAddress>,
    gl: Option<Gl>,
    pub file: PathBuf,
    pub download_id: Option<i64>,
    pub loaded: bool,
    pub tracks: Vec<Track>,
    pub chapters: Vec<ChapterMark>,
    pending_start: bool,
}

unsafe extern "C" fn repaint_cb(data: *mut c_void) {
    // SAFETY: `data` is the boxed egui::Context owned by the Player, which outlives mpv.
    let ctx = unsafe { &*(data as *const egui::Context) };
    ctx.request_repaint();
}

unsafe extern "C" fn get_proc(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    // SAFETY: `ctx` is the boxed GetProcAddress owned by the Player; `name` is a C string.
    let f = unsafe { &*(ctx as *const GetProcAddress) };
    let name = unsafe { CStr::from_ptr(name) };
    f(name) as *mut c_void
}

impl Player {
    /// Creates an mpv instance with an OpenGL render context on the current GL context.
    pub fn new(ctx: &egui::Context, proc_address: GetProcAddress) -> Result<Player, String> {
        let mpv = libmpv()?;
        // SAFETY: calls follow libmpv's documented order: create, set options, initialise,
        // then create the render context. Pointers passed in live as long as the Player.
        unsafe {
            let handle = (mpv.create)();
            if handle.is_null() {
                return Err("mpv_create failed".into());
            }
            for (k, v) in [
                ("vo", "libmpv"),
                ("hwdec", "auto-safe"),
                ("keep-open", "yes"),
                ("idle", "yes"),
                ("osc", "no"),
                ("input-default-bindings", "no"),
                ("input-vo-keyboard", "no"),
                ("config", "no"),
                ("terminal", "no"),
                ("ytdl", "no"),
                ("sub-auto", "fuzzy"),
                ("audio-display", "no"),
            ] {
                (mpv.set_option_string)(handle, cstr(k).as_ptr(), cstr(v).as_ptr());
            }
            let r = (mpv.initialize)(handle);
            if r < 0 {
                (mpv.terminate_destroy)(handle);
                return Err(format!("mpv_initialize: {}", mpv.error(r)));
            }
            let repaint = Box::new(ctx.clone());
            let proc_box = Box::new(proc_address);
            let api = cstr("opengl");
            let mut init = ffi::mpv_opengl_init_params {
                get_proc_address: Some(get_proc),
                get_proc_address_ctx: &*proc_box as *const GetProcAddress as *mut c_void,
            };
            let mut params = [
                ffi::mpv_render_param {
                    type_: ffi::MPV_RENDER_PARAM_API_TYPE,
                    data: api.as_ptr() as *mut c_void,
                },
                ffi::mpv_render_param {
                    type_: ffi::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                    data: &mut init as *mut _ as *mut c_void,
                },
                ffi::mpv_render_param {
                    type_: ffi::MPV_RENDER_PARAM_INVALID,
                    data: std::ptr::null_mut(),
                },
            ];
            let mut render = std::ptr::null_mut();
            let r = (mpv.render_context_create)(&mut render, handle, params.as_mut_ptr());
            if r < 0 {
                (mpv.terminate_destroy)(handle);
                return Err(format!("mpv render context: {}", mpv.error(r)));
            }
            let data = &*repaint as *const egui::Context as *mut c_void;
            (mpv.render_context_set_update_callback)(render, Some(repaint_cb), data);
            (mpv.set_wakeup_callback)(handle, Some(repaint_cb), data);
            Ok(Player {
                mpv,
                handle,
                render,
                _repaint: repaint,
                _proc: proc_box,
                gl: None,
                file: PathBuf::new(),
                download_id: None,
                loaded: false,
                tracks: Vec::new(),
                chapters: Vec::new(),
                pending_start: false,
            })
        }
    }

    fn command(&self, args: &[&str]) -> Result<(), String> {
        let owned: Vec<_> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        // SAFETY: a NULL-terminated array of valid C strings.
        let r = unsafe { (self.mpv.command)(self.handle, ptrs.as_mut_ptr()) };
        if r < 0 {
            Err(self.mpv.error(r))
        } else {
            Ok(())
        }
    }

    pub fn set(&self, name: &str, value: &str) {
        // SAFETY: valid handle and C strings.
        unsafe {
            (self.mpv.set_property_string)(self.handle, cstr(name).as_ptr(), cstr(value).as_ptr());
        }
    }

    pub fn get_string(&self, name: &str) -> Option<String> {
        // SAFETY: mpv returns an owned C string or NULL; it is freed with mpv_free.
        unsafe {
            let p = (self.mpv.get_property_string)(self.handle, cstr(name).as_ptr());
            if p.is_null() {
                return None;
            }
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            (self.mpv.free)(p as *mut c_void);
            Some(s)
        }
    }

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        let mut v: f64 = 0.0;
        // SAFETY: MPV_FORMAT_DOUBLE writes one f64 into `v`.
        let r = unsafe {
            (self.mpv.get_property)(
                self.handle,
                cstr(name).as_ptr(),
                ffi::MPV_FORMAT_DOUBLE,
                &mut v as *mut f64 as *mut c_void,
            )
        };
        (r >= 0).then_some(v)
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        let mut v: i64 = 0;
        // SAFETY: MPV_FORMAT_INT64 writes one i64 into `v`.
        let r = unsafe {
            (self.mpv.get_property)(
                self.handle,
                cstr(name).as_ptr(),
                ffi::MPV_FORMAT_INT64,
                &mut v as *mut i64 as *mut c_void,
            )
        };
        (r >= 0).then_some(v)
    }

    pub fn get_flag(&self, name: &str) -> Option<bool> {
        let mut v: c_int = 0;
        // SAFETY: MPV_FORMAT_FLAG writes one int into `v`.
        let r = unsafe {
            (self.mpv.get_property)(
                self.handle,
                cstr(name).as_ptr(),
                ffi::MPV_FORMAT_FLAG,
                &mut v as *mut c_int as *mut c_void,
            )
        };
        (r >= 0).then_some(v != 0)
    }

    pub fn set_f64(&self, name: &str, mut v: f64) {
        // SAFETY: MPV_FORMAT_DOUBLE reads one f64.
        unsafe {
            (self.mpv.set_property)(
                self.handle,
                cstr(name).as_ptr(),
                ffi::MPV_FORMAT_DOUBLE,
                &mut v as *mut f64 as *mut c_void,
            );
        }
    }

    /// Opens a file, starting at `start` seconds if given.
    pub fn open(&mut self, file: &Path, start: Option<f64>) -> Result<(), String> {
        self.loaded = false;
        self.tracks.clear();
        self.chapters.clear();
        self.file = file.to_path_buf();
        // mpv keeps runtime options across files: a trim preview loop must not carry over.
        self.set_loop(None);
        match start {
            Some(s) if s > 0.0 => {
                self.set("start", &format!("{s:.3}"));
                self.pending_start = true;
            }
            _ => self.set("start", "none"),
        }
        self.command(&["loadfile", &file.to_string_lossy(), "replace"])?;
        self.set("pause", "no");
        Ok(())
    }

    pub fn stop(&mut self) {
        let _ = self.command(&["stop"]);
        self.loaded = false;
        self.download_id = None;
    }

    pub fn seek(&self, secs: f64) {
        let _ = self.command(&["seek", &format!("{secs:.3}"), "absolute+exact"]);
    }

    pub fn seek_relative(&self, secs: f64) {
        let _ = self.command(&["seek", &format!("{secs:.3}"), "relative"]);
    }

    pub fn toggle_pause(&self) {
        let _ = self.command(&["cycle", "pause"]);
    }

    pub fn paused(&self) -> bool {
        self.get_flag("pause").unwrap_or(true)
    }

    pub fn position(&self) -> Option<f64> {
        self.get_f64("time-pos")
    }

    pub fn duration(&self) -> Option<f64> {
        self.get_f64("duration")
    }

    /// Handles queued mpv events. Returns true if a file finished loading.
    pub fn poll_events(&mut self) -> bool {
        let mut loaded_now = false;
        loop {
            // SAFETY: timeout 0 never blocks; the event is valid until the next call.
            let ev = unsafe { &*(self.mpv.wait_event)(self.handle, 0.0) };
            match ev.event_id {
                ffi::MPV_EVENT_NONE => break,
                ffi::MPV_EVENT_FILE_LOADED => {
                    self.loaded = true;
                    loaded_now = true;
                    if self.pending_start {
                        self.pending_start = false;
                        self.set("start", "none");
                    }
                    self.refresh_tracks();
                }
                ffi::MPV_EVENT_SHUTDOWN => break,
                _ => {}
            }
        }
        loaded_now
    }

    /// Reads the audio/subtitle tracks and chapters of the loaded file.
    pub fn refresh_tracks(&mut self) {
        let n = self.get_i64("track-list/count").unwrap_or(0);
        self.tracks = (0..n)
            .filter_map(|i| {
                let p = |k: &str| self.get_string(&format!("track-list/{i}/{k}"));
                Some(Track {
                    id: p("id")?.parse().ok()?,
                    kind: p("type")?,
                    title: p("title").unwrap_or_default(),
                    lang: p("lang").unwrap_or_default(),
                    selected: p("selected").as_deref() == Some("yes"),
                })
            })
            .collect();
        let c = self.get_i64("chapter-list/count").unwrap_or(0);
        self.chapters = (0..c)
            .filter_map(|i| {
                Some(ChapterMark {
                    time: self.get_f64(&format!("chapter-list/{i}/time"))?,
                    title: self
                        .get_string(&format!("chapter-list/{i}/title"))
                        .unwrap_or_default(),
                })
            })
            .collect();
    }

    pub fn select_track(&mut self, kind: &str, id: Option<i64>) {
        let prop = match kind {
            "audio" => "aid",
            "sub" => "sid",
            _ => return,
        };
        self.set(prop, &id.map_or("no".into(), |i| i.to_string()));
        for t in self.tracks.iter_mut().filter(|t| t.kind == kind) {
            t.selected = Some(t.id) == id;
        }
    }

    /// Sets an A-B loop (for trim previews); `None` clears it.
    pub fn set_loop(&self, range: Option<(f64, f64)>) {
        match range {
            Some((a, b)) => {
                self.set("ab-loop-a", &format!("{a:.3}"));
                self.set("ab-loop-b", &format!("{b:.3}"));
            }
            None => {
                self.set("ab-loop-a", "no");
                self.set("ab-loop-b", "no");
            }
        }
    }

    /// Renders the current frame into the player's texture at `size` pixels, if mpv has a
    /// new frame or the size changed. Returns the texture to show.
    pub fn render(
        &mut self,
        gl: &glow::Context,
        frame: &mut eframe::Frame,
        size: [i32; 2],
    ) -> Option<egui::TextureId> {
        let size = [size[0].clamp(16, 8192), size[1].clamp(16, 8192)];
        // SAFETY: GL calls on the current context (eframe makes it current during ui()).
        unsafe {
            let resized = match &mut self.gl {
                Some(g) if g.size == size => false,
                Some(g) => {
                    gl.bind_texture(glow::TEXTURE_2D, Some(g.texture));
                    gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        glow::RGBA8 as i32,
                        size[0],
                        size[1],
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(None),
                    );
                    g.size = size;
                    true
                }
                None => {
                    let texture = gl.create_texture().ok()?;
                    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                    for (k, v) in [
                        (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                        (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                        (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                        (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
                    ] {
                        gl.tex_parameter_i32(glow::TEXTURE_2D, k, v as i32);
                    }
                    gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        glow::RGBA8 as i32,
                        size[0],
                        size[1],
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(None),
                    );
                    let fbo = gl.create_framebuffer().ok()?;
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                    gl.framebuffer_texture_2d(
                        glow::FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        glow::TEXTURE_2D,
                        Some(texture),
                        0,
                    );
                    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                    let id = frame.register_native_glow_texture(texture);
                    self.gl = Some(Gl {
                        texture,
                        fbo,
                        size,
                        id,
                    });
                    true
                }
            };
            gl.bind_texture(glow::TEXTURE_2D, None);
            let g = self.gl.as_ref()?;
            let flags = (self.mpv.render_context_update)(self.render);
            if resized || flags & ffi::MPV_RENDER_UPDATE_FRAME != 0 {
                let mut fbo = ffi::mpv_opengl_fbo {
                    fbo: g.fbo.0.get() as c_int,
                    w: size[0],
                    h: size[1],
                    internal_format: 0,
                };
                // The FBO texture is shown by egui with row 0 at the top: no flip.
                let mut flip: c_int = 0;
                let mut params = [
                    ffi::mpv_render_param {
                        type_: ffi::MPV_RENDER_PARAM_OPENGL_FBO,
                        data: &mut fbo as *mut _ as *mut c_void,
                    },
                    ffi::mpv_render_param {
                        type_: ffi::MPV_RENDER_PARAM_FLIP_Y,
                        data: &mut flip as *mut _ as *mut c_void,
                    },
                    ffi::mpv_render_param {
                        type_: ffi::MPV_RENDER_PARAM_INVALID,
                        data: std::ptr::null_mut(),
                    },
                ];
                // mpv expects default GL state; egui leaves blending on.
                gl.disable(glow::BLEND);
                (self.mpv.render_context_render)(self.render, params.as_mut_ptr());
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            }
            Some(g.id)
        }
    }

    /// Frees the framebuffer; call with the GL context current before dropping. The texture
    /// was handed to egui, which deletes it itself.
    pub fn free_gl(&mut self, gl: &glow::Context) {
        if let Some(g) = self.gl.take() {
            // SAFETY: deleting the framebuffer this player created on this context.
            unsafe {
                gl.delete_framebuffer(g.fbo);
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: render context first, then the core, as libmpv requires.
        unsafe {
            (self.mpv.render_context_set_update_callback)(self.render, None, std::ptr::null_mut());
            (self.mpv.set_wakeup_callback)(self.handle, None, std::ptr::null_mut());
            (self.mpv.render_context_free)(self.render);
            (self.mpv.terminate_destroy)(self.handle);
        }
    }
}

/// External mpv for systems without libmpv. Returns how it was opened.
pub fn open_external(mpv_path: &str, file: &Path, start: Option<f64>) -> Result<String, String> {
    let exe = if mpv_path.trim().is_empty() {
        crate::tools::which("mpv").ok_or_else(install_hint)?
    } else {
        PathBuf::from(mpv_path.trim())
    };
    let mut c = std::process::Command::new(&exe);
    if let Some(s) = start.filter(|s| *s > 0.0) {
        c.arg(format!("--start={s:.3}"));
    }
    c.arg("--").arg(file);
    c.spawn()
        .map(|_| format!("Opened in {}", exe.display()))
        .map_err(|e| format!("could not start {}: {e}", exe.display()))
}

pub fn install_hint() -> String {
    if cfg!(windows) {
        "Install mpv to play here: put libmpv-2.dll (from the mpv-dev builds on \
         sourceforge.net/projects/mpv-player-windows) next to BearDL.exe, or install mpv \
         with `winget install mpv` or `scoop install mpv`."
            .into()
    } else if cfg!(target_os = "macos") {
        "Install mpv to play here: `brew install mpv` (provides libmpv).".into()
    } else {
        "Install mpv to play here: your package manager's `libmpv2` (or `mpv-libs`) package, \
         or `mpv` for the external player."
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_labels() {
        let t = |title: &str, lang: &str| Track {
            id: 2,
            kind: "audio".into(),
            title: title.into(),
            lang: lang.into(),
            selected: false,
        };
        assert_eq!(t("Commentary", "en").label(), "Commentary (en)");
        assert_eq!(t("", "de").label(), "de");
        assert_eq!(t("", "").label(), "Track 2");
    }

    #[test]
    fn missing_libmpv_is_an_error_not_a_crash() {
        // With an override that does not exist, loading falls through to the system search;
        // either outcome must be a clean Result.
        let _ = libmpv();
    }
}
