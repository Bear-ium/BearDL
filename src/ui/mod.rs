pub mod fonts;
mod settings_view;

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
    toasts: Vec<Toast>,
    bench_exit: bool,
    frames: u32,
}

const TOAST_SECS: f64 = 6.0;

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, bench_exit: bool) -> Self {
        crate::startup::mark("creator");
        let _ = crate::paths::ensure_dirs();
        let settings = Settings::load();
        cc.egui_ctx.set_fonts(fonts::system_fonts(None));
        cc.egui_ctx.set_zoom_factor(settings.ui_scale);
        crate::startup::mark("fonts set");
        let ctx = cc.egui_ctx.clone();
        let engine = Engine::start(
            settings.clone(),
            crate::paths::tools_dir(),
            std::sync::Arc::new(move || ctx.request_repaint()),
        );
        Self {
            engine,
            saved_settings: settings.clone(),
            settings,
            page: Page::Downloads,
            toasts: Vec::new(),
            bench_exit,
            frames: 0,
        }
    }

    fn sync_settings(&mut self, ctx: &egui::Context) {
        // Wait until text editing ends, so partial paths are never applied.
        if self.settings == self.saved_settings || ctx.memory(|m| m.focused().is_some()) {
            return;
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
                    ui.spinner();
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(170.0)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.heading("BearDL");
                ui.add_space(8.0);
                for p in Page::ALL {
                    let r = ui.add_sized(
                        [ui.available_width(), 28.0],
                        egui::Button::selectable(self.page == p, p.label()),
                    );
                    if r.clicked() {
                        self.page = p;
                    }
                }
            });
        egui::Panel::bottom("status")
            .exact_size(26.0)
            .show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default_margins().show(ui, |ui| match self.page {
            Page::Settings => settings_view::show(self, ui),
            other => {
                ui.heading(other.label());
                ui.weak("Coming in a later milestone.");
            }
        });
        self.show_toasts(&ctx);
        self.sync_settings(&ctx);

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
