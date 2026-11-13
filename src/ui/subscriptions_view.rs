//! Subscriptions page: follow channels, playlists or any URL; options, status and logs.
use super::App;
use crate::engine::{Cmd, SubView};
use crate::subs::{FirstCheck, SubConfig, SubtitleChoice};
use crate::util::{format_timestamp, unix_now};
use eframe::egui;

#[derive(Default)]
pub struct SubsUi {
    new_url: String,
    new_name: String,
    new_config: Option<SubConfig>,
    /// The subscription being edited: (id, name, url, config).
    editing: Option<(i64, String, String, SubConfig)>,
    confirm_delete: Option<i64>,
}

fn when(ts: i64) -> String {
    if ts <= 0 {
        return "never".into();
    }
    let d = ts - unix_now();
    let (a, past) = (d.abs(), d < 0);
    let t = if a < 60 {
        "under a minute".to_owned()
    } else if a < 3600 {
        format!("{} min", a / 60)
    } else if a < 86_400 {
        format!("{} h {} min", a / 3600, a % 3600 / 60)
    } else {
        return format_timestamp(ts);
    };
    if past {
        format!("{t} ago")
    } else {
        format!("in {t}")
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let subs = app.engine.state().subscriptions.clone();
    let mut cmds = Vec::new();
    ui.horizontal(|ui| {
        ui.heading("Subscriptions");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(!subs.is_empty(), egui::Button::new("Check all now"))
                .clicked()
            {
                cmds.push(Cmd::CheckAllSubscriptions);
            }
        });
    });
    ui.horizontal(|ui| {
        let s = &mut app.subs;
        ui.add(
            egui::TextEdit::singleline(&mut s.new_url)
                .hint_text("Channel, playlist or any URL yt-dlp can list")
                .desired_width(360.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut s.new_name)
                .hint_text("Name (optional)")
                .desired_width(160.0),
        );
        if ui
            .add_enabled(!s.new_url.trim().is_empty(), egui::Button::new("Subscribe"))
            .clicked()
        {
            cmds.push(Cmd::AddSubscription {
                name: std::mem::take(&mut s.new_name),
                url: std::mem::take(&mut s.new_url),
                config: s.new_config.take().unwrap_or_default(),
            });
        }
    });
    // The options that matter before the first check, set up front.
    let presets: Vec<String> = app
        .engine
        .state()
        .presets
        .iter()
        .map(|p| p.name.clone())
        .collect();
    let cfg = app.subs.new_config.get_or_insert_with(SubConfig::default);
    ui.horizontal_wrapped(|ui| {
        ui.label("Preset");
        egui::ComboBox::from_id_salt("new_sub_preset")
            .selected_text(if cfg.preset.is_empty() {
                "Default".to_owned()
            } else {
                cfg.preset.clone()
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut cfg.preset, String::new(), "Default");
                for p in &presets {
                    ui.selectable_value(&mut cfg.preset, p.clone(), p);
                }
            });
        ui.label("First check");
        ui.selectable_value(&mut cfg.first_check, FirstCheck::Latest, "newest items");
        ui.selectable_value(
            &mut cfg.first_check,
            FirstCheck::OnlyNew,
            "only future uploads",
        );
        ui.label("Items");
        ui.add(egui::DragValue::new(&mut cfg.max_items).range(1..=500));
    });
    ui.weak("Edit a subscription for its folder, quality, filters and how often it checks.");
    ui.separator();
    if app.subs.editing.is_some() {
        egui::Panel::right("sub_editor")
            .resizable(true)
            .default_size(380.0)
            .min_size(300.0)
            .show(ui, |ui| editor(app, ui, &subs, &mut cmds));
    }
    if subs.is_empty() {
        ui.weak("No subscriptions yet.");
    }
    egui::ScrollArea::vertical()
        .id_salt("subs")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for v in &subs {
                row(app, ui, v, &mut cmds);
                ui.separator();
            }
        });
    if let Some(id) = app.subs.confirm_delete {
        let name = subs
            .iter()
            .find(|v| v.sub.id == id)
            .map(|v| v.sub.name.clone())
            .unwrap_or_default();
        egui::Window::new("Delete subscription")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "Stop following “{name}”? Downloaded files and library entries stay."
                ));
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        cmds.push(Cmd::DeleteSubscription(id));
                        app.subs.confirm_delete = None;
                        if app.subs.editing.as_ref().is_some_and(|e| e.0 == id) {
                            app.subs.editing = None;
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        app.subs.confirm_delete = None;
                    }
                });
            });
    }
    for c in cmds {
        app.engine.send(c);
    }
}

fn row(app: &mut App, ui: &mut egui::Ui, v: &SubView, cmds: &mut Vec<Cmd>) {
    let s = &v.sub;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.strong(&s.name);
                if s.paused {
                    ui.colored_label(ui.visuals().warn_fg_color, "paused");
                }
                if v.checking {
                    ui.spinner();
                    ui.weak("checking");
                }
            });
            ui.add(egui::Label::new(egui::RichText::new(&s.url).small().weak()).truncate());
            let status = format!(
                "Last check {} · next {} · every {} min · {} seen",
                when(s.last_check),
                if s.paused {
                    "paused".to_owned()
                } else {
                    when(s.next_check.max(unix_now()))
                },
                s.config.interval_minutes,
                v.seen
            );
            ui.weak(status);
            if let Some(l) = v.logs.first() {
                let col = if l.is_error {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.colored_label(col, format!("{}: {}", when(l.ts), l.message));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Delete").clicked() {
                app.subs.confirm_delete = Some(s.id);
            }
            if ui.button("Edit").clicked() {
                app.subs.editing = Some((s.id, s.name.clone(), s.url.clone(), s.config.clone()));
            }
            let label = if s.paused { "Resume" } else { "Pause" };
            if ui.button(label).clicked() {
                cmds.push(Cmd::PauseSubscription(s.id, !s.paused));
            }
            if ui
                .add_enabled(!v.checking, egui::Button::new("Check now"))
                .clicked()
            {
                cmds.push(Cmd::CheckSubscription(s.id));
            }
        });
    });
}

fn editor(app: &mut App, ui: &mut egui::Ui, subs: &[SubView], cmds: &mut Vec<Cmd>) {
    let presets: Vec<String> = app
        .engine
        .state()
        .presets
        .iter()
        .map(|p| p.name.clone())
        .collect();
    let default_preset = app.settings.default_preset.clone();
    let Some((id, name, url, cfg)) = app.subs.editing.as_mut() else {
        return;
    };
    let id = *id;
    let mut close = false;
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("Edit subscription");
        egui::Grid::new("sub_edit")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label("Name");
                ui.text_edit_singleline(name);
                ui.end_row();
                ui.label("URL");
                ui.text_edit_singleline(url);
                ui.end_row();
                ui.label("Preset");
                egui::ComboBox::from_id_salt("sub_preset")
                    .selected_text(if cfg.preset.is_empty() {
                        format!("Default ({default_preset})")
                    } else {
                        cfg.preset.clone()
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut cfg.preset, String::new(), "Default");
                        for p in &presets {
                            ui.selectable_value(&mut cfg.preset, p.clone(), p);
                        }
                    });
                ui.end_row();
                ui.label("Folder");
                ui.add(
                    egui::TextEdit::singleline(&mut cfg.folder)
                        .hint_text("sub-folder or absolute path"),
                );
                ui.end_row();
                ui.label("Max height");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut cfg.max_height).range(0..=8640));
                    ui.weak("0 = preset");
                });
                ui.end_row();
                ui.label("Uploaded after");
                ui.add(egui::TextEdit::singleline(&mut cfg.date_after).hint_text("YYYYMMDD"));
                ui.end_row();
                ui.label("Title must contain");
                ui.add(
                    egui::TextEdit::singleline(&mut cfg.include)
                        .hint_text("any of: word, other word"),
                );
                ui.end_row();
                ui.label("Skip titles with");
                ui.add(egui::TextEdit::singleline(&mut cfg.exclude).hint_text("#shorts, live"));
                ui.end_row();
                ui.label("Items per check");
                ui.add(egui::DragValue::new(&mut cfg.max_items).range(1..=500));
                ui.end_row();
                ui.label("Check every");
                ui.add(
                    egui::DragValue::new(&mut cfg.interval_minutes)
                        .range(5..=10_080)
                        .suffix(" min"),
                );
                ui.end_row();
                ui.label("Subtitles");
                ui.horizontal(|ui| {
                    for (v, l) in [
                        (SubtitleChoice::Preset, "Preset"),
                        (SubtitleChoice::On, "On"),
                        (SubtitleChoice::Off, "Off"),
                    ] {
                        ui.selectable_value(&mut cfg.subtitles, v, l);
                    }
                });
                ui.end_row();
                ui.label("First check");
                ui.vertical(|ui| {
                    ui.radio_value(
                        &mut cfg.first_check,
                        FirstCheck::Latest,
                        "Download the newest items",
                    );
                    ui.radio_value(
                        &mut cfg.first_check,
                        FirstCheck::OnlyNew,
                        "Only future uploads",
                    );
                });
                ui.end_row();
            });
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                cmds.push(Cmd::UpdateSubscription {
                    id,
                    name: name.clone(),
                    url: url.clone(),
                    config: cfg.clone(),
                });
                close = true;
            }
            if ui.button("Close").clicked() {
                close = true;
            }
        });
        if let Some(v) = subs.iter().find(|v| v.sub.id == id) {
            ui.add_space(8.0);
            ui.strong("Recent checks");
            if v.logs.is_empty() {
                ui.weak("None yet.");
            }
            for l in &v.logs {
                let col = if l.is_error {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().text_color()
                };
                ui.horizontal_wrapped(|ui| {
                    ui.weak(format_timestamp(l.ts));
                    ui.colored_label(col, &l.message);
                });
            }
        }
    });
    if close {
        app.subs.editing = None;
    }
}
