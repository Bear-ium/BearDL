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
            // Resized to a share of the screen on the first frame (see `App::fit_window`).
            .with_inner_size([1100.0, 700.0])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_icon(beardl::platform::window_icon()),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "BearDL",
        options,
        Box::new(move |cc| Ok(Box::new(beardl::ui::App::new(cc, bench_exit, minimised)))),
    )
}
