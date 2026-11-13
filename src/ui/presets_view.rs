use super::App;
use crate::engine::Cmd;
use crate::ytdlp::{AUDIO_FORMATS, CONTAINERS, DownloadOptions, Mode, Preset};
use eframe::egui;

#[derive(Default)]
pub struct PresetsUi {
    /// Name of the preset being edited, as stored (None for a new one).
    original: Option<String>,
    draft: Option<Preset>,
}

fn combo(ui: &mut egui::Ui, id: &str, value: &mut String, choices: &[&str], empty: &str) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(if value.is_empty() {
            empty
        } else {
            value.as_str()
        })
        .show_ui(ui, |ui| {
            for c in choices {
                ui.selectable_value(value, (*c).to_owned(), if c.is_empty() { empty } else { c });
            }
        });
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.strong("Presets");
    ui.weak("Named yt-dlp option sets. The default preset is used for quick downloads.");
    let presets = app.engine.state().presets.clone();
    let mut cmds = Vec::new();
    ui.horizontal_wrapped(|ui| {
        for p in &presets {
            let editing = app.presets.original.as_deref() == Some(p.name.as_str());
            let label = if p.name == app.settings.default_preset {
                format!("{} ★", p.name)
            } else {
                p.name.clone()
            };
            if ui.selectable_label(editing, label).clicked() {
                app.presets.original = Some(p.name.clone());
                app.presets.draft = Some(p.clone());
            }
        }
        if ui.button("New").clicked() {
            app.presets.original = None;
            app.presets.draft = Some(Preset {
                name: "New preset".into(),
                options: DownloadOptions::default(),
            });
        }
    });
    let Some(draft) = app.presets.draft.as_mut() else {
        return;
    };
    let validation = app.engine.state().validation.clone();
    let mut close = false;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        let o = &mut draft.options;
        egui::Grid::new("preset")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut draft.name);
                ui.end_row();
                ui.label("Streams");
                ui.horizontal(|ui| {
                    for m in Mode::ALL {
                        ui.selectable_value(&mut o.mode, m, m.label());
                    }
                });
                ui.end_row();
                ui.label("Max height");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut o.max_height)
                            .range(0..=8640)
                            .suffix(" px"),
                    );
                    ui.weak("0 = best available");
                });
                ui.end_row();
                ui.label("Format selector");
                let auto = DownloadOptions {
                    format: String::new(),
                    ..o.clone()
                }
                .format_spec();
                ui.add(
                    egui::TextEdit::singleline(&mut o.format)
                        .hint_text(auto)
                        .desired_width(300.0),
                );
                ui.end_row();
                ui.label("Container");
                combo(
                    ui,
                    "p_container",
                    &mut o.container,
                    &CONTAINERS,
                    "Automatic",
                );
                ui.end_row();
                ui.label("Audio conversion");
                combo(
                    ui,
                    "p_audio",
                    &mut o.audio_format,
                    &AUDIO_FORMATS,
                    "Keep original",
                );
                ui.end_row();
                ui.label("Embed");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut o.embed_metadata, "Metadata");
                    ui.checkbox(&mut o.embed_thumbnail, "Thumbnail");
                    ui.checkbox(&mut o.embed_chapters, "Chapters");
                    ui.checkbox(&mut o.embed_subs, "Subtitles");
                });
                ui.end_row();
                ui.label("Subtitles");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut o.subtitles, "Manual");
                    ui.checkbox(&mut o.auto_subtitles, "Automatic");
                    ui.add(
                        egui::TextEdit::singleline(&mut o.sub_langs)
                            .hint_text("languages (default from settings)")
                            .desired_width(200.0),
                    );
                });
                ui.end_row();
                ui.label("Folder");
                ui.add(
                    egui::TextEdit::singleline(&mut o.folder)
                        .hint_text("sub-folder or absolute path")
                        .desired_width(300.0),
                );
                ui.end_row();
                ui.label("File name template");
                ui.add(
                    egui::TextEdit::singleline(&mut o.output_template)
                        .hint_text("default from settings")
                        .desired_width(300.0),
                );
                ui.end_row();
                ui.label("Uploaded after");
                ui.add(
                    egui::TextEdit::singleline(&mut o.date_after)
                        .hint_text("YYYYMMDD or now-7days")
                        .desired_width(160.0),
                );
                ui.end_row();
                ui.label("Custom arguments");
                ui.vertical(|ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut o.custom_args)
                            .hint_text("e.g. --sponsorblock-remove all -N 4")
                            .desired_rows(2)
                            .desired_width(400.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Validate").clicked() {
                            cmds.push(Cmd::ValidateArgs(o.custom_args.clone()));
                        }
                        match &validation {
                            Some((args, None)) if *args == o.custom_args => {
                                ui.spinner();
                            }
                            Some((args, Some(Ok(())))) if *args == o.custom_args => {
                                ui.colored_label(egui::Color32::from_rgb(80, 180, 100), "Valid");
                            }
                            Some((args, Some(Err(e)))) if *args == o.custom_args => {
                                ui.colored_label(ui.visuals().error_fg_color, e);
                            }
                            _ => {
                                if let Err(e) = crate::ytdlp::validate_custom_args(&o.custom_args) {
                                    ui.colored_label(ui.visuals().error_fg_color, e);
                                }
                            }
                        }
                    });
                });
                ui.end_row();
            });
        ui.horizontal(|ui| {
            let args_ok = crate::ytdlp::validate_custom_args(&draft.options.custom_args).is_ok();
            if ui
                .add_enabled(
                    args_ok && !draft.name.trim().is_empty(),
                    egui::Button::new("Save"),
                )
                .clicked()
            {
                cmds.push(Cmd::SavePreset {
                    old_name: app.presets.original.clone(),
                    preset: draft.clone(),
                });
                if app.settings.default_preset == app.presets.original.clone().unwrap_or_default() {
                    app.settings.default_preset = draft.name.trim().to_owned();
                }
                app.presets.original = Some(draft.name.trim().to_owned());
            }
            if let Some(orig) = app.presets.original.clone() {
                if ui.button("Make default").clicked() {
                    app.settings.default_preset = orig.clone();
                }
                if ui.button("Duplicate").clicked() {
                    draft.name = format!("{orig} copy");
                    app.presets.original = None;
                }
                if presets.len() > 1 && ui.button("Delete").clicked() {
                    cmds.push(Cmd::DeletePreset(orig.clone()));
                    if app.settings.default_preset == orig {
                        app.settings.default_preset = presets
                            .iter()
                            .find(|p| p.name != orig)
                            .map(|p| p.name.clone())
                            .unwrap_or_default();
                    }
                    close = true;
                }
            }
            if ui.button("Close").clicked() {
                close = true;
            }
        });
    });
    if close {
        app.presets.original = None;
        app.presets.draft = None;
    }
    for c in cmds {
        app.engine.send(c);
    }
}
