use super::App;
use crate::engine::Cmd;
use crate::tools::Channel;
use crate::util::format_timestamp;
use eframe::egui;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("Settings");
        ui.add_space(6.0);
        general(app, ui);
        ui.add_space(12.0);
        super::presets_view::show(app, ui);
        ui.add_space(12.0);
        tools(app, ui);
    });
}

fn general(app: &mut App, ui: &mut egui::Ui) {
    let s = &mut app.settings;
    ui.strong("Downloads");
    egui::Grid::new("general")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Download folder");
            let mut dir = s.download_dir.display().to_string();
            if ui
                .add(egui::TextEdit::singleline(&mut dir).desired_width(360.0))
                .changed()
            {
                s.download_dir = dir.into();
            }
            ui.end_row();
            ui.label("File name template");
            ui.add(egui::TextEdit::singleline(&mut s.output_template).desired_width(360.0));
            ui.end_row();
            ui.label("Concurrent downloads");
            ui.add(egui::DragValue::new(&mut s.concurrency).range(1..=64));
            ui.end_row();
            ui.label("Global rate limit");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut s.rate_limit)
                        .hint_text("none, e.g. 5M or 800K")
                        .desired_width(170.0),
                );
                if !s.rate_limit.trim().is_empty()
                    && crate::util::parse_rate(&s.rate_limit).is_none()
                {
                    ui.colored_label(ui.visuals().error_fg_color, "not a rate");
                } else {
                    ui.weak("split evenly across concurrent downloads");
                }
            });
            ui.end_row();
            ui.label("Subtitle languages");
            ui.add(
                egui::TextEdit::singleline(&mut s.subtitle_langs)
                    .hint_text("en,en-US,en-GB")
                    .desired_width(200.0),
            );
            ui.end_row();
        });
}

fn tools(app: &mut App, ui: &mut egui::Ui) {
    ui.strong("yt-dlp and ffmpeg");
    let t = app.engine.state().tools.clone();
    let paths = app.engine.state().paths.clone();
    let busy = t.busy.is_some();
    let mut cmds = Vec::new();
    let s = &mut app.settings;
    egui::Grid::new("tools")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("yt-dlp");
            ui.vertical(|ui| {
                match (&t.ytdlp_version, &paths.ytdlp) {
                    (Some(v), Some(p)) => {
                        let src = if t.ytdlp_managed {
                            t.ytdlp_channel.map_or("managed", |c| c.label())
                        } else {
                            "external"
                        };
                        ui.label(format!("{v} ({src})"));
                        ui.weak(p.display().to_string());
                    }
                    (None, Some(p)) => {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!("{} does not run", p.display()),
                        );
                    }
                    _ => {
                        ui.colored_label(ui.visuals().warn_fg_color, "not installed");
                    }
                }
                if let Some(l) = &t.ytdlp_latest {
                    if l.available {
                        ui.colored_label(
                            ui.visuals().hyperlink_color,
                            format!("Update available: {}", l.latest),
                        );
                    } else {
                        ui.weak(format!("Latest: {}", l.latest));
                    }
                }
                if t.last_check > 0 {
                    ui.weak(format!(
                        "Last checked {} UTC",
                        format_timestamp(t.last_check)
                    ));
                }
            });
            ui.end_row();

            ui.label("Update channel");
            ui.horizontal(|ui| {
                for c in Channel::ALL {
                    ui.selectable_value(&mut s.ytdlp_channel, c, c.label());
                }
            });
            ui.end_row();

            ui.label("");
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!busy && s.ytdlp_path.trim().is_empty(), |ui| {
                    if ui.button("Check for updates").clicked() {
                        cmds.push(Cmd::CheckTools { manual: true });
                    }
                    let label = if paths.ytdlp.is_some() {
                        "Update now"
                    } else {
                        "Install"
                    };
                    if ui.button(label).clicked() {
                        cmds.push(Cmd::UpdateYtdlp);
                    }
                    if let Some(prev) = &t.ytdlp_prev
                        && ui
                            .button("Roll back")
                            .on_hover_text(format!("Restore {prev}"))
                            .clicked()
                    {
                        cmds.push(Cmd::RollbackYtdlp);
                    }
                });
            });
            ui.end_row();

            ui.label("Automatic updates");
            ui.horizontal(|ui| {
                ui.checkbox(&mut s.auto_update, "Check on startup and every");
                ui.add(
                    egui::DragValue::new(&mut s.update_interval_hours)
                        .range(1..=720)
                        .suffix(" h"),
                );
            });
            ui.end_row();

            ui.label("Custom yt-dlp");
            ui.add(
                egui::TextEdit::singleline(&mut s.ytdlp_path)
                    .hint_text("path; empty uses the managed copy")
                    .desired_width(360.0),
            );
            ui.end_row();

            ui.label("ffmpeg");
            ui.vertical(|ui| {
                match (&t.ffmpeg_version, &paths.ffmpeg) {
                    (Some(v), Some(p)) => {
                        let src = if t.ffmpeg_managed {
                            "managed"
                        } else {
                            "system"
                        };
                        ui.label(format!("{v} ({src})"));
                        ui.weak(p.display().to_string());
                    }
                    _ => {
                        ui.colored_label(ui.visuals().warn_fg_color, "not found");
                    }
                }
                if let Some(l) = t.ffmpeg_latest.as_ref().filter(|l| l.available) {
                    ui.colored_label(
                        ui.visuals().hyperlink_color,
                        format!("New build available from {}", l.latest),
                    );
                }
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        let label = if t.ffmpeg_managed {
                            "Update ffmpeg"
                        } else {
                            "Download ffmpeg"
                        };
                        if ui.button(label).clicked() {
                            cmds.push(Cmd::InstallFfmpeg);
                        }
                        if let Some(prev) = &t.ffmpeg_prev
                            && ui
                                .button("Roll back")
                                .on_hover_text(format!("Restore {prev}"))
                                .clicked()
                        {
                            cmds.push(Cmd::RollbackFfmpeg);
                        }
                    });
                });
            });
            ui.end_row();

            ui.label("Custom ffmpeg");
            ui.add(
                egui::TextEdit::singleline(&mut s.ffmpeg_path)
                    .hint_text("path to ffmpeg; empty auto-detects")
                    .desired_width(360.0),
            );
            ui.end_row();
        });
    if let Some(e) = &t.error {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
    for c in cmds {
        app.engine.send(c);
    }
}
