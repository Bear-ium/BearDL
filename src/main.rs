#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;

fn main() -> eframe::Result {
    beardl::startup::begin();
    let bench_exit = std::env::args().any(|a| a == "--bench-exit");
    // Started with the OS: begin hidden in the tray.
    let minimised = std::env::args().any(|a| a == "--minimised");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("BearDL")
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(beardl::platform::window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "BearDL",
        options,
        Box::new(move |cc| Ok(Box::new(beardl::ui::App::new(cc, bench_exit, minimised)))),
    )
}
