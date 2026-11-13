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
        appearance(app, ui);
        ui.add_space(12.0);
        system(app, ui);
        ui.add_space(12.0);
        super::presets_view::show(app, ui);
        ui.add_space(12.0);
        tools(app, ui);
        ui.add_space(12.0);
        import_export(app, ui);
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
                    .hint_text(crate::ytdlp::DEFAULT_SUB_LANGS)
                    .desired_width(200.0),
            );
            ui.end_row();
        });
}

fn appearance(app: &mut App, ui: &mut egui::Ui) {
    ui.strong("Appearance");
    let mut names: Vec<String> = crate::theme::builtins()
        .into_iter()
        .map(|t| t.name)
        .collect();
    for u in &app.user_themes {
        if let Ok(t) = &u.theme
            && !names.contains(&t.name)
        {
            names.push(t.name.clone());
        }
    }
    egui::Grid::new("appearance")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Theme");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("theme")
                    .selected_text(app.settings.theme.clone())
                    .show_ui(ui, |ui| {
                        for n in &names {
                            ui.selectable_value(&mut app.settings.theme, n.clone(), n);
                        }
                    });
                if ui
                    .button("Reload themes")
                    .on_hover_text("Read the themes folder again after editing a file")
                    .clicked()
                {
                    app.reload_themes();
                }
                if ui.button("Open themes folder").clicked() {
                    let dir = crate::paths::themes_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = crate::util::open_path(&dir);
                }
            });
            ui.end_row();
            ui.label("Interface scale");
            ui.horizontal(|ui| {
                ui.spacing_mut().slider_width = 200.0;
                let mut scale = app.scale_draft.unwrap_or(app.settings.ui_scale);
                // Zooming moves the slider under the pointer, so a drag only applies on release.
                let r = ui.add(
                    egui::Slider::new(&mut scale, 0.75..=2.0)
                        .step_by(0.05)
                        .show_value(false),
                );
                scale = (scale * 20.0).round() / 20.0;
                if r.dragged() {
                    app.scale_draft = Some(scale);
                } else if app.scale_draft.take().is_some() || r.changed() {
                    app.settings.ui_scale = scale;
                }
                ui.label(format!("{:.0}%", scale * 100.0));
                if app.scale_draft.is_some() {
                    ui.weak("applies when you let go");
                } else if (app.settings.ui_scale - 1.0).abs() > 0.001
                    && ui.button("Reset").on_hover_text("Back to 100%").clicked()
                {
                    app.settings.ui_scale = 1.0;
                }
            });
            ui.end_row();
        });
    for u in &app.user_themes {
        if let Err(e) = &u.theme {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!(
                    "{}: {e}",
                    u.path.file_name().unwrap_or_default().to_string_lossy()
                ),
            );
        }
    }
}

fn system(app: &mut App, ui: &mut egui::Ui) {
    let s = &mut app.settings;
    ui.strong("System");
    ui.checkbox(&mut s.start_with_os, "Start with the system (in the tray)");
    ui.checkbox(
        &mut s.close_to_tray,
        "Closing the window keeps BearDL running in the tray, so subscriptions keep checking",
    );
    ui.checkbox(
        &mut s.notifications,
        "Notify when a subscription downloads something new",
    );
}

fn import_export(app: &mut App, ui: &mut egui::Ui) {
    use crate::export::ImportMode;
    ui.strong("Import and export");
    ui.weak("One file with settings, presets, subscriptions and your themes.");
    ui.horizontal(|ui| {
        ui.label("File");
        ui.add(egui::TextEdit::singleline(&mut app.export_path).desired_width(380.0));
    });
    let path = std::path::PathBuf::from(app.export_path.trim().trim_matches('"'));
    let has_path = !app.export_path.trim().is_empty();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(has_path, egui::Button::new("Export"))
            .clicked()
        {
            app.engine.send(Cmd::Export {
                path: path.clone(),
                settings: Box::new(app.settings.clone()),
            });
        }
        let exists = path.is_file();
        if ui
            .add_enabled(exists, egui::Button::new("Import and merge"))
            .on_hover_text("Add and update presets, subscriptions and themes; keep your own")
            .clicked()
        {
            app.engine.send(Cmd::Import {
                path: path.clone(),
                mode: ImportMode::Merge,
                current: Box::new(app.settings.clone()),
            });
        }
        if ui
            .add_enabled(exists, egui::Button::new("Import and replace…"))
            .on_hover_text("Make presets, subscriptions and themes match the file")
            .clicked()
        {
            app.confirm_replace = true;
        }
    });
    if app.confirm_replace {
        ui.horizontal(|ui| {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Replace removes presets, subscriptions and themes that are not in the file.",
            );
            if ui.button("Replace").clicked() {
                app.engine.send(Cmd::Import {
                    path: path.clone(),
                    mode: ImportMode::Replace,
                    current: Box::new(app.settings.clone()),
                });
                app.confirm_replace = false;
            }
            if ui.button("Cancel").clicked() {
                app.confirm_replace = false;
            }
        });
    }
}

fn tools(app: &mut App, ui: &mut egui::Ui) {
    ui.strong("Tools");
    let t = app.engine.state().tools.clone();
    let paths = app.engine.state().paths.clone();
    let busy = t.busy.is_some();
    let libmpv = crate::player::libmpv(&app.saved_settings.libmpv_path);
    let custom_libmpv = !app.saved_settings.libmpv_path.is_empty()
        || std::env::var_os("BEARDL_LIBMPV").is_some_and(|v| !v.is_empty());
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

            ui.label("libmpv");
            ui.vertical(|ui| match &libmpv {
                Ok(m) => {
                    let managed = crate::paths::tools_dir()
                        .join("mpv")
                        .join(crate::libmpv::DLL);
                    let src = match &t.libmpv {
                        _ if custom_libmpv => "custom".to_owned(),
                        Some(i) if m.path == managed => format!("downloaded, build {}", i.version),
                        _ => "found on this system".to_owned(),
                    };
                    ui.label(format!("Built-in player ready ({src})"));
                    ui.weak(m.path.display().to_string());
                }
                Err(e) => {
                    ui.colored_label(ui.visuals().warn_fg_color, "not found");
                    ui.weak(e);
                    if let Some(c) = super::player_view::libmpv_setup(&t, ui) {
                        cmds.push(c);
                    }
                }
            });
            ui.end_row();

            ui.label("Custom libmpv");
            ui.add(
                egui::TextEdit::singleline(&mut s.libmpv_path)
                    .hint_text("path to the library; empty searches the usual places")
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
