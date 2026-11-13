pub mod fonts;

use eframe::egui;

pub struct App {
    bench_exit: bool,
    frames: u32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, bench_exit: bool) -> Self {
        crate::startup::mark("creator");
        cc.egui_ctx.set_fonts(fonts::system_fonts(None));
        crate::startup::mark("fonts set");
        Self {
            bench_exit,
            frames: 0,
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default_margins().show(ui, |ui| ui.heading("BearDL"));
        self.frames += 1;
        if self.frames <= 2 {
            crate::startup::mark("frame");
        }
        if self.bench_exit {
            if self.frames >= 2 {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ui.ctx().request_repaint();
            }
        }
    }
}
