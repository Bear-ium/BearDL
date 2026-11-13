mod downloads_view;
pub mod fonts;
mod library_view;
mod player_view;
mod presets_view;
mod search_view;
mod settings_view;
#[cfg(feature = "dev-shots")]
mod shots;
mod subscriptions_view;

use crate::engine::{Engine, Level, Notice};
use crate::settings::Settings;
use eframe::egui;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Downloads,
    Library,
    Search,
    Subscriptions,
    Player,
    Settings,
}

impl Page {
    const ALL: [Page; 6] = [
        Page::Downloads,
        Page::Library,
        Page::Search,
        Page::Subscriptions,
        Page::Player,
        Page::Settings,
    ];

    pub fn from_name(s: &str) -> Option<Page> {
        Page::ALL
            .into_iter()
            .find(|p| format!("{p:?}").eq_ignore_ascii_case(s))
    }

    fn label(self) -> &'static str {
        match self {
            Page::Downloads => "⬇  Downloads",
            Page::Library => "▤  Library",
            Page::Search => "🔍  Search",
            Page::Subscriptions => "↻  Subscriptions",
            Page::Player => "▶  Player",
            Page::Settings => "⚙  Settings",
        }
    }
}

struct Toast {
    notice: Notice,
    until: f64,
}

pub struct App {
    pub(crate) engine: Engine,
    pub(crate) settings: Settings,
    saved_settings: Settings,
    page: Page,
    dl: downloads_view::DownloadsUi,
    lib: library_view::LibraryUi,
    player_ui: player_view::PlayerUi,
    search: search_view::SearchUi,
    subs: subscriptions_view::SubsUi,
    tray: Option<crate::platform::Tray>,
    quitting: bool,
    /// Started with `--minimised`: frames left before hiding in the tray. eframe shows the
    /// window after its first frame whatever the builder says, so the hide comes after that.
    hide_at_start: u8,
    presets: presets_view::PresetsUi,
    toasts: Vec<Toast>,
    bench_exit: bool,
    frames: u32,
    pub(crate) user_themes: Vec<crate::theme::UserTheme>,
    applied_theme: Option<String>,
    applied_font: Option<Option<std::path::PathBuf>>,
    applied_scale: f32,
    themes_rev: u64,
    pub(crate) export_path: String,
    pub(crate) confirm_replace: bool,
    #[cfg(feature = "dev-shots")]
    shots: Option<shots::Shots>,
}

const TOAST_SECS: f64 = 6.0;

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, bench_exit: bool, minimised: bool) -> Self {
        crate::startup::mark("creator");
        let _ = crate::paths::ensure_dirs();
        let settings = Settings::load();
        crate::theme::write_example(&crate::paths::themes_dir());
        let user_themes = crate::theme::load_user_themes(&crate::paths::themes_dir());
        let ctx = cc.egui_ctx.clone();
        let engine = Engine::start(
            crate::engine::EngineConfig {
                settings: settings.clone(),
                tools_dir: crate::paths::tools_dir(),
                db_path: crate::paths::db_path(),
                thumbs_dir: crate::paths::thumbs_dir(),
                subs_dir: crate::paths::subs_dir(),
                themes_dir: crate::paths::themes_dir(),
            },
            std::sync::Arc::new(move |d: std::time::Duration| {
                if d.is_zero() {
                    ctx.request_repaint();
                } else {
                    ctx.request_repaint_after(d);
                }
            }),
        );
        Self {
            engine,
            saved_settings: settings.clone(),
            settings,
            page: Page::Downloads,
            dl: Default::default(),
            lib: Default::default(),
            search: Default::default(),
            subs: Default::default(),
            tray: None,
            quitting: false,
            hide_at_start: 0,
            player_ui: player_view::PlayerUi {
                gl_proc: cc.get_proc_address.clone(),
                ..Default::default()
            },
            presets: Default::default(),
            toasts: Vec::new(),
            bench_exit,
            frames: 0,
            user_themes,
            applied_theme: None,
            applied_font: None,
            applied_scale: 0.0,
            themes_rev: 0,
            export_path: crate::paths::home()
                .join("BearDL-settings.json")
                .display()
                .to_string(),
            confirm_replace: false,
            #[cfg(feature = "dev-shots")]
            shots: shots::Shots::from_env(),
        }
        .with_dev_setup()
        .with_theme(&cc.egui_ctx)
        .with_tray(&cc.egui_ctx, bench_exit, minimised)
    }

    fn with_tray(mut self, ctx: &egui::Context, bench_exit: bool, minimised: bool) -> Self {
        if !bench_exit {
            match crate::platform::Tray::new(ctx) {
                Ok(t) => self.tray = Some(t),
                Err(e) => self.push_toast(Level::Error, format!("No tray icon: {e}")),
            }
        }
        if minimised {
            if self.tray.is_some() {
                self.hide_at_start = 2;
            } else {
                // No tray to bring it back: start minimised instead.
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }
        // Keep the start-up entry pointing at this program if it has moved.
        if self.settings.start_with_os && !bench_exit {
            let _ = crate::platform::set_autostart(true);
        }
        self
    }

    /// Tray menu, close-to-tray and desktop notifications. Runs in `logic`, which eframe also
    /// calls while the window is hidden.
    fn handle_os(&mut self, ctx: &egui::Context) {
        let events = self.tray.as_ref().map(|t| t.events()).unwrap_or_default();
        for ev in events {
            match ev {
                crate::platform::TrayEvent::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                crate::platform::TrayEvent::CheckSubscriptions => {
                    self.engine.send(crate::engine::Cmd::CheckAllSubscriptions);
                }
                crate::platform::TrayEvent::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        let close = ctx.input(|i| i.viewport().close_requested());
        if close && !self.quitting && self.settings.close_to_tray && self.tray.is_some() {
            // Keep running in the tray so subscriptions keep checking.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            if let Some(p) = &self.player_ui.player
                && !p.paused()
            {
                p.toggle_pause();
            }
        }
        let notices = std::mem::take(&mut self.engine.state().desktop_notices);
        for (title, body) in notices {
            match &self.tray {
                Some(t) => t.notify(&title, &body),
                None => crate::platform::notify(&title, &body),
            }
        }
    }

    fn with_theme(mut self, ctx: &egui::Context) -> Self {
        self.apply_theme(ctx);
        crate::startup::mark("theme and fonts set");
        self
    }

    pub(crate) fn reload_themes(&mut self) {
        self.user_themes = crate::theme::load_user_themes(&crate::paths::themes_dir());
        self.applied_theme = None;
        self.applied_font = None;
    }

    /// Applies the selected theme (colours, spacing, font sizes, optional font file) and the
    /// UI scale. Runs only when something changed.
    fn apply_theme(&mut self, ctx: &egui::Context) {
        if self.applied_scale != self.settings.ui_scale {
            self.applied_scale = self.settings.ui_scale;
            ctx.set_zoom_factor(self.settings.ui_scale);
        }
        if self.applied_theme.as_deref() == Some(self.settings.theme.as_str()) {
            return;
        }
        let theme = crate::theme::find(&self.settings.theme, &self.user_themes);
        ctx.all_styles_mut(|s| theme.apply(s));
        ctx.set_theme(if theme.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        let font = theme.font_path();
        if self.applied_font.as_ref() != Some(&font) {
            ctx.set_fonts(fonts::system_fonts(font.as_deref()));
            self.applied_font = Some(font);
        }
        self.applied_theme = Some(self.settings.theme.clone());
    }

    fn with_dev_setup(#[allow(unused_mut)] mut self) -> Self {
        #[cfg(feature = "dev-shots")]
        if let Some(s) = &self.shots {
            if let Some(p) = s.page.as_deref().and_then(Page::from_name) {
                self.page = p;
            }
            if s.trim {
                self.player_ui.dev_open_trim();
            }
            if let Some(q) = s.search.clone() {
                self.search.dev_set(&q);
            }
            if s.play_first {
                self.lib.dev_play_first = true;
                self.lib.dev_select_first = true;
            }
            if s.select_first {
                self.lib.dev_select_first = true;
                self.lib.quick_view = true;
            }
            if let Some(u) = s.url.clone() {
                self.engine.send(crate::engine::Cmd::Analyse(u));
            }
        }
        self
    }

    fn sync_settings(&mut self, ctx: &egui::Context) {
        // Wait until text editing ends, so partial paths are never applied.
        if self.settings == self.saved_settings || ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        if self.settings.start_with_os != self.saved_settings.start_with_os
            && let Err(e) = crate::platform::set_autostart(self.settings.start_with_os)
        {
            self.push_toast(
                Level::Error,
                format!("Could not change start-up setting: {e}"),
            );
            self.settings.start_with_os = self.saved_settings.start_with_os;
        }
        self.settings = self.settings.clone().sanitised();
        if let Err(e) = self.settings.save() {
            self.push_toast(Level::Error, format!("Could not save settings: {e}"));
        }
        self.saved_settings = self.settings.clone();
        self.engine.send(crate::engine::Cmd::ApplySettings(Box::new(
            self.settings.clone(),
        )));
    }

    /// Plays a library entry, optionally from a position in seconds.
    pub(crate) fn play(&mut self, d: &crate::db::Download, start: Option<f64>) {
        if std::path::Path::new(&d.file_path).is_file() {
            self.player_ui.pending = Some((std::sync::Arc::new(d.clone()), start));
        } else {
            self.push_toast(Level::Error, "The file is missing".into());
        }
    }

    pub(crate) fn push_toast(&mut self, level: Level, text: String) {
        self.toasts.push(Toast {
            notice: Notice { level, text },
            until: 0.0,
        });
    }

    fn show_toasts(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let incoming = std::mem::take(&mut self.engine.state().notices);
        for n in incoming {
            self.toasts.push(Toast {
                notice: n,
                until: 0.0,
            });
        }
        for t in &mut self.toasts {
            if t.until == 0.0 {
                t.until = now + TOAST_SECS;
            }
        }
        self.toasts.retain(|t| t.until > now);
        if self.toasts.len() > 5 {
            self.toasts.drain(..self.toasts.len() - 5);
        }
        let Some(next) = self.toasts.iter().map(|t| t.until).reduce(f64::min) else {
            return;
        };
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((next - now).max(0.05)));
        egui::Area::new(egui::Id::unique("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -36.0])
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                let mut dismiss = None;
                for (i, t) in self.toasts.iter().enumerate() {
                    let colour = match t.notice.level {
                        Level::Info => ui.visuals().widgets.noninteractive.fg_stroke.color,
                        Level::Error => ui.visuals().error_fg_color,
                    };
                    let r = egui::Frame::popup(ui.style())
                        .show(ui, |ui| {
                            ui.set_max_width(380.0);
                            ui.colored_label(colour, &t.notice.text);
                        })
                        .response;
                    if r.interact(egui::Sense::click()).clicked() {
                        dismiss = Some(i);
                    }
                }
                if let Some(i) = dismiss {
                    self.toasts.remove(i);
                }
            });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let st = self.engine.state();
            match &st.tools.busy {
                Some(job) => {
                    // Static, not a spinner: background checks must not repaint every frame.
                    ui.label("↻");
                    let text = match job.total {
                        Some(t) if t > 0 => format!(
                            "{} {} / {}",
                            job.label,
                            crate::util::format_bytes(job.done),
                            crate::util::format_bytes(t)
                        ),
                        _ if job.done > 0 => {
                            format!("{} {}", job.label, crate::util::format_bytes(job.done))
                        }
                        _ => job.label.clone(),
                    };
                    ui.label(text);
                }
                None => match &st.tools.ytdlp_version {
                    Some(v) => {
                        ui.weak(format!("yt-dlp {v}"));
                    }
                    None => {
                        ui.colored_label(ui.visuals().warn_fg_color, "yt-dlp is not installed");
                    }
                },
            }
            if st.paths.ffmpeg.is_none() {
                ui.separator();
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "ffmpeg not found: merging and conversion are unavailable",
                );
            }
        });
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.handle_os(ctx);
        self.drive_player(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let immersive = self.player_ui.fullscreen && self.page == Page::Player;
        if !immersive {
            self.side_panels(ui);
        }
        egui::CentralPanel::default_margins().show(ui, |ui| match self.page {
            Page::Settings => settings_view::show(self, ui),
            Page::Downloads => downloads_view::show(self, ui),
            Page::Library => library_view::show(self, ui),
            Page::Player => player_view::show(self, ui, frame),
            Page::Search => search_view::show(self, ui),
            Page::Subscriptions => subscriptions_view::show(self, ui),
        });
        self.after_frame(&ctx);
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        self.save_resume();
        if let (Some(p), Some(gl)) = (self.player_ui.player.as_mut(), gl) {
            p.free_gl(gl);
        }
        self.player_ui.player = None;
    }
}

impl App {
    fn side_panels(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(170.0)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.heading("BearDL");
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                    ui.spacing_mut().button_padding = egui::vec2(10.0, 6.0);
                    for p in Page::ALL {
                        if ui.selectable_label(self.page == p, p.label()).clicked() {
                            self.page = p;
                        }
                    }
                });
            });
        egui::Panel::bottom("status")
            .exact_size(26.0)
            .show(ui, |ui| self.status_bar(ui));
    }

    fn after_frame(&mut self, ctx: &egui::Context) {
        if self.hide_at_start > 0 {
            self.hide_at_start -= 1;
            if self.hide_at_start == 0 {
                crate::startup::mark("hiding in tray");
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                ctx.request_repaint();
            }
        }
        let ctx = ctx.clone();
        self.show_toasts(&ctx);
        let (imported, themes_rev) = {
            let mut st = self.engine.state();
            (st.imported_settings.take(), st.themes_rev)
        };
        if let Some(s) = imported {
            self.settings = s;
        }
        if themes_rev != self.themes_rev {
            self.themes_rev = themes_rev;
            self.reload_themes();
        }
        self.sync_settings(&ctx);
        self.apply_theme(&ctx);

        #[cfg(feature = "dev-shots")]
        if let Some(s) = self.shots.as_mut() {
            s.tick(&ctx);
        }
        self.frames += 1;
        if self.frames <= 2 {
            crate::startup::mark("frame");
        }
        if self.bench_exit {
            if self.frames >= 2 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ctx.request_repaint();
            }
        }
    }
}
