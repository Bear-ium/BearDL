use super::App;
use crate::db::Status;
use crate::engine::{Cmd, MoveTo, NewItem, QueueItem};
use crate::util::{format_bytes, format_duration};
use crate::ytdlp::{
    AUDIO_FORMATS, Analysis, CONTAINERS, DownloadOptions, Entry, Format, FormatKind, Mode,
};
use eframe::egui;

#[derive(Default)]
pub struct DownloadsUi {
    url_input: String,
    preset: String,
    opts: DownloadOptions,
    video_fmt: Option<String>,
    audio_fmt: Option<String>,
    selected: Vec<bool>,
    entry_filter: String,
    /// URL whose analysis the editing state above belongs to.
    bound_url: String,
    /// Entry indices matching the filter, cached per (filter, entry count).
    visible: Option<(String, usize, Vec<usize>)>,
}

const HEIGHTS: [u32; 8] = [0, 4320, 2160, 1440, 1080, 720, 480, 360];

fn height_label(h: u32) -> String {
    if h == 0 {
        "Any".into()
    } else {
        format!("{h}p")
    }
}

impl App {
    fn preset_options(&self, name: &str) -> DownloadOptions {
        self.engine
            .state()
            .presets
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.options.clone())
            .unwrap_or_default()
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    if app.dl.preset.is_empty() {
        app.dl.preset = app.settings.default_preset.clone();
    }
    input_row(app, ui);
    ui.add_space(6.0);
    analysis_panel(app, ui);
    ui.add_space(6.0);
    queue(app, ui);
}

fn preset_combo(app: &mut App, ui: &mut egui::Ui) -> bool {
    let names: Vec<String> = app
        .engine
        .state()
        .presets
        .iter()
        .map(|p| p.name.clone())
        .collect();
    let mut changed = false;
    egui::ComboBox::from_id_salt("preset")
        .selected_text(app.dl.preset.clone())
        .width(170.0)
        .show_ui(ui, |ui| {
            for n in names {
                changed |= ui
                    .selectable_value(&mut app.dl.preset, n.clone(), n)
                    .changed();
            }
        });
    changed
}

fn input_row(app: &mut App, ui: &mut egui::Ui) {
    let mut analyse = false;
    let mut quick = false;
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 420.0).max(200.0);
        let r = ui.add(
            egui::TextEdit::singleline(&mut app.dl.url_input)
                .hint_text("Paste a video, playlist or channel URL (several URLs: one per space)")
                .desired_width(w),
        );
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            analyse = true;
        }
        if preset_combo(app, ui) && app.dl.bound_url.is_empty() {
            app.dl.opts = app.preset_options(&app.dl.preset);
        }
        let has_url = !app.dl.url_input.trim().is_empty();
        if ui
            .add_enabled(has_url, egui::Button::new("Analyse"))
            .on_hover_text("List formats, or the items of a playlist or channel")
            .clicked()
        {
            analyse = true;
        }
        if ui
            .add_enabled(has_url, egui::Button::new("Download"))
            .on_hover_text("Queue with the selected preset")
            .clicked()
        {
            quick = true;
        }
    });
    let urls: Vec<String> = app
        .dl
        .url_input
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if analyse && let Some(first) = urls.first() {
        if urls.len() > 1 {
            quick = true;
        } else {
            app.engine.send(Cmd::Analyse(first.clone()));
        }
    }
    if quick && !urls.is_empty() {
        let template = NewItem {
            preset: app.dl.preset.clone(),
            options: app.preset_options(&app.dl.preset),
            ..Default::default()
        };
        app.engine.send(Cmd::QuickAdd { urls, template });
        app.dl.url_input.clear();
    }
    let expanding = app.engine.state().expanding;
    if expanding > 0 {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.weak(format!("Reading {expanding} URL(s)…"));
        });
    }
}

fn analysis_panel(app: &mut App, ui: &mut egui::Ui) {
    let Some(view) = app.engine.state().analysis.clone() else {
        app.dl.bound_url.clear();
        return;
    };
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if view.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Analysing {}", view.url));
                if ui.button("Cancel").clicked() {
                    app.engine.send(Cmd::ClearAnalysis);
                }
            });
            return;
        }
        match view.result {
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                if ui.button("Close").clicked() {
                    app.engine.send(Cmd::ClearAnalysis);
                }
            }
            Some(Ok(a)) => {
                if app.dl.bound_url != view.url {
                    app.dl.bound_url = view.url.clone();
                    app.dl.opts = app.preset_options(&app.dl.preset);
                    app.dl.video_fmt = None;
                    app.dl.audio_fmt = None;
                    app.dl.entry_filter.clear();
                    app.dl.visible = None;
                    app.dl.selected = match a.as_ref() {
                        Analysis::Playlist { entries, .. } => {
                            entries.iter().map(|e| !e.is_playlist).collect()
                        }
                        _ => Vec::new(),
                    };
                }
                match a.as_ref() {
                    Analysis::Video { meta, formats } => {
                        video_panel(app, ui, &view.url, meta, formats)
                    }
                    Analysis::Playlist {
                        title,
                        channel,
                        entries,
                    } => playlist_panel(app, ui, title, channel, entries),
                }
            }
            None => {}
        }
    });
}

fn format_label(f: &Format) -> String {
    match f.size {
        Some(s) => format!(
            "{} · {}{}",
            f.describe(),
            if f.approx { "~" } else { "" },
            format_bytes(s)
        ),
        None => f.describe(),
    }
}

fn options_row(ui: &mut egui::Ui, o: &mut DownloadOptions, explicit_video: bool) {
    ui.horizontal_wrapped(|ui| {
        for m in Mode::ALL {
            ui.selectable_value(&mut o.mode, m, m.label());
        }
        ui.separator();
        if o.mode != Mode::AudioOnly {
            ui.add_enabled_ui(!explicit_video, |ui| {
                ui.label("Max quality");
                egui::ComboBox::from_id_salt("maxh")
                    .selected_text(height_label(o.max_height))
                    .show_ui(ui, |ui| {
                        for h in HEIGHTS {
                            ui.selectable_value(&mut o.max_height, h, height_label(h));
                        }
                    });
            });
            ui.label("Container");
            egui::ComboBox::from_id_salt("container")
                .selected_text(if o.container.is_empty() {
                    "Auto"
                } else {
                    &o.container
                })
                .show_ui(ui, |ui| {
                    for c in CONTAINERS {
                        ui.selectable_value(
                            &mut o.container,
                            c.to_owned(),
                            if c.is_empty() { "Auto" } else { c },
                        );
                    }
                });
        } else {
            ui.label("Convert to");
            egui::ComboBox::from_id_salt("afmt")
                .selected_text(if o.audio_format.is_empty() {
                    "Original"
                } else {
                    &o.audio_format
                })
                .show_ui(ui, |ui| {
                    for c in AUDIO_FORMATS {
                        ui.selectable_value(
                            &mut o.audio_format,
                            c.to_owned(),
                            if c.is_empty() { "Original" } else { c },
                        );
                    }
                });
        }
        ui.separator();
        ui.checkbox(&mut o.subtitles, "Subtitles");
        ui.checkbox(&mut o.auto_subtitles, "Auto captions");
    });
}

/// Builds `-f` from the chosen streams; `None` keeps the preset's automatic selection.
fn explicit_format(mode: Mode, video: Option<&Format>, audio: Option<&Format>) -> Option<String> {
    match (mode, video, audio) {
        (Mode::AudioOnly, _, Some(a)) => Some(a.id.clone()),
        (Mode::AudioOnly, _, None) => None,
        (Mode::VideoOnly, Some(v), _) => Some(v.id.clone()),
        (Mode::VideoAudio, Some(v), _) if v.kind == FormatKind::Muxed => Some(v.id.clone()),
        (Mode::VideoAudio, Some(v), Some(a)) => Some(format!("{}+{}", v.id, a.id)),
        (Mode::VideoAudio, Some(v), None) => Some(format!("{}+ba/{}", v.id, v.id)),
        (Mode::VideoAudio, None, Some(a)) => Some(format!("bv*+{}", a.id)),
        _ => None,
    }
}

fn video_panel(
    app: &mut App,
    ui: &mut egui::Ui,
    url: &str,
    meta: &crate::ytdlp::Meta,
    formats: &[Format],
) {
    ui.horizontal(|ui| {
        ui.strong(&meta.title);
        if let Some(d) = meta.duration {
            ui.weak(format_duration(d));
        }
        if !meta.channel.is_empty() {
            ui.weak(&meta.channel);
        }
    });
    let dl = &mut app.dl;
    options_row(ui, &mut dl.opts, dl.video_fmt.is_some());
    let video: Vec<&Format> = formats
        .iter()
        .filter(|f| f.kind != FormatKind::AudioOnly)
        .collect();
    let audio: Vec<&Format> = formats
        .iter()
        .filter(|f| f.kind == FormatKind::AudioOnly)
        .collect();
    ui.horizontal_wrapped(|ui| {
        if dl.opts.mode != Mode::AudioOnly {
            ui.label("Video");
            let current = dl
                .video_fmt
                .as_ref()
                .and_then(|id| video.iter().find(|f| &f.id == id))
                .map_or("Best (automatic)".to_owned(), |f| format_label(f));
            egui::ComboBox::from_id_salt("vfmt")
                .selected_text(current)
                .width(330.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut dl.video_fmt, None, "Best (automatic)");
                    for f in &video {
                        ui.selectable_value(&mut dl.video_fmt, Some(f.id.clone()), format_label(f));
                    }
                });
        }
        if dl.opts.mode != Mode::VideoOnly {
            ui.label("Audio");
            let current = dl
                .audio_fmt
                .as_ref()
                .and_then(|id| audio.iter().find(|f| &f.id == id))
                .map_or("Best (automatic)".to_owned(), |f| format_label(f));
            egui::ComboBox::from_id_salt("afmtsel")
                .selected_text(current)
                .width(300.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut dl.audio_fmt, None, "Best (automatic)");
                    for f in &audio {
                        ui.selectable_value(&mut dl.audio_fmt, Some(f.id.clone()), format_label(f));
                    }
                });
        }
    });
    ui.weak(format!("{} formats available", formats.len()));
    ui.horizontal(|ui| {
        if ui.button("Add to queue").clicked() {
            let v = dl
                .video_fmt
                .as_ref()
                .and_then(|id| formats.iter().find(|f| &f.id == id));
            let a = dl
                .audio_fmt
                .as_ref()
                .and_then(|id| formats.iter().find(|f| &f.id == id));
            let mut options = dl.opts.clone();
            if let Some(f) = explicit_format(options.mode, v, a) {
                options.format = f;
            }
            let item = NewItem {
                url: url.to_owned(),
                title: meta.title.clone(),
                channel: meta.channel.clone(),
                duration: meta.duration,
                thumbnail: meta.thumbnail.clone(),
                video_id: meta.id.clone(),
                preset: dl.preset.clone(),
                options,
                subscription_id: None,
            };
            app.engine.send(Cmd::Enqueue(vec![item]));
            app.engine.send(Cmd::ClearAnalysis);
            dl.url_input.clear();
        }
        if ui.button("Close").clicked() {
            app.engine.send(Cmd::ClearAnalysis);
        }
    });
}

fn playlist_panel(app: &mut App, ui: &mut egui::Ui, title: &str, channel: &str, entries: &[Entry]) {
    let dl = &mut app.dl;
    if dl.selected.len() != entries.len() {
        dl.selected = vec![true; entries.len()];
    }
    ui.horizontal(|ui| {
        ui.strong(if title.is_empty() { "Playlist" } else { title });
        if !channel.is_empty() {
            ui.weak(channel);
        }
        ui.weak(format!("{} items", entries.len()));
    });
    options_row(ui, &mut dl.opts, false);
    let filter = dl.entry_filter.to_lowercase();
    let stale = dl
        .visible
        .as_ref()
        .is_none_or(|(f, n, _)| *f != filter || *n != entries.len());
    if stale {
        let v: Vec<usize> = (0..entries.len())
            .filter(|&i| filter.is_empty() || entries[i].title.to_lowercase().contains(&filter))
            .collect();
        dl.visible = Some((filter.clone(), entries.len(), v));
    }
    let visible = dl.visible.as_ref().map(|v| v.2.clone()).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut dl.entry_filter)
                .hint_text("Filter by title")
                .desired_width(220.0),
        );
        if ui.button("All").clicked() {
            for &i in &visible {
                dl.selected[i] = !entries[i].is_playlist;
            }
        }
        if ui.button("None").clicked() {
            for &i in &visible {
                dl.selected[i] = false;
            }
        }
        if ui.button("Invert").clicked() {
            for &i in &visible {
                dl.selected[i] = !dl.selected[i] && !entries[i].is_playlist;
            }
        }
    });
    let mut open = None;
    let row_h = ui.spacing().interact_size.y + 2.0;
    egui::ScrollArea::vertical()
        .id_salt("entries")
        .max_height(240.0)
        .auto_shrink([false, true])
        .show_rows(ui, row_h, visible.len(), |ui, range| {
            for &i in &visible[range] {
                let e = &entries[i];
                ui.horizontal(|ui| {
                    if e.is_playlist {
                        if ui.button("Open").clicked() {
                            open = Some(e.url.clone());
                        }
                        ui.label(format!("▸ {}", e.title));
                    } else {
                        ui.checkbox(&mut dl.selected[i], "");
                        ui.label(&e.title);
                        if let Some(d) = e.duration {
                            ui.weak(format_duration(d));
                        }
                    }
                });
            }
        });
    let n = dl.selected.iter().filter(|s| **s).count();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(n > 0, egui::Button::new(format!("Add {n} to queue")))
            .clicked()
        {
            let items = entries
                .iter()
                .zip(&dl.selected)
                .filter(|(e, s)| **s && !e.is_playlist)
                .map(|(e, _)| NewItem {
                    url: e.url.clone(),
                    title: e.title.clone(),
                    channel: if e.channel.is_empty() {
                        channel.to_owned()
                    } else {
                        e.channel.clone()
                    },
                    duration: e.duration,
                    video_id: e.id.clone(),
                    preset: dl.preset.clone(),
                    options: dl.opts.clone(),
                    ..Default::default()
                })
                .collect();
            app.engine.send(Cmd::Enqueue(items));
            app.engine.send(Cmd::ClearAnalysis);
            dl.url_input.clear();
        }
        if ui.button("Close").clicked() {
            app.engine.send(Cmd::ClearAnalysis);
        }
    });
    if let Some(u) = open {
        app.engine.send(Cmd::Analyse(u));
    }
}

fn progress_text(q: &QueueItem) -> String {
    let p = &q.progress;
    let mut parts = Vec::new();
    if !p.stage.is_empty() {
        parts.push(p.stage.clone());
    }
    if let Some(f) = p.fraction {
        parts.push(format!("{:.0}%", f * 100.0));
    }
    if let Some(s) = p.speed {
        parts.push(format!("{}/s", format_bytes(s as u64)));
    }
    if let Some(e) = p.eta {
        parts.push(format!("ETA {}", format_duration(e)));
    }
    parts.join(" · ")
}

fn queue(app: &mut App, ui: &mut egui::Ui) {
    let (len, active, waiting) = {
        let st = app.engine.state();
        let count = |s: Status| st.queue.iter().filter(|q| q.status == s).count();
        (
            st.queue.len(),
            count(Status::Running),
            count(Status::Queued),
        )
    };
    let mut cmds = Vec::new();
    ui.horizontal(|ui| {
        ui.heading("Queue");
        ui.weak(format!("{active} downloading · {waiting} waiting"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Clear finished").clicked() {
                cmds.push(Cmd::ClearFinished);
            }
            if ui.button("Resume all").clicked() {
                cmds.push(Cmd::ResumeAll);
            }
            if ui.button("Pause all").clicked() {
                cmds.push(Cmd::PauseAll);
            }
        });
    });
    ui.separator();
    if len == 0 {
        ui.weak("Nothing queued. Paste a URL above to start.");
    }
    let row_h = 52.0;
    egui::ScrollArea::vertical()
        .id_salt("queue")
        .auto_shrink([false, false])
        .show_rows(ui, row_h, len, |ui, range| {
            // Copy only the visible rows out of the shared state.
            let rows: Vec<QueueItem> = {
                let st = app.engine.state();
                let end = range.end.min(st.queue.len());
                st.queue[range.start.min(end)..end].to_vec()
            };
            for q in &rows {
                queue_row(ui, q, row_h, &mut cmds);
            }
        });
    for c in cmds {
        app.engine.send(c);
    }
}

fn queue_row(ui: &mut egui::Ui, q: &QueueItem, row_h: f32, cmds: &mut Vec<Cmd>) {
    ui.allocate_ui(egui::vec2(ui.available_width(), row_h), |ui| {
        ui.horizontal(|ui| {
            let text_w = (ui.available_width() - 520.0).max(160.0);
            ui.vertical(|ui| {
                ui.set_width(text_w);
                ui.add(egui::Label::new(egui::RichText::new(&q.title).strong()).truncate());
                let status = match q.status {
                    Status::Failed | Status::Skipped if !q.error.is_empty() => {
                        format!(
                            "{}: {}",
                            q.status.label(),
                            q.error.lines().next().unwrap_or("")
                        )
                    }
                    s => s.label().to_owned(),
                };
                let colour = match q.status {
                    Status::Failed => ui.visuals().error_fg_color,
                    Status::Done => ui.visuals().widgets.inactive.fg_stroke.color,
                    _ => ui.visuals().weak_text_color(),
                };
                let meta = [q.channel.as_str(), q.preset.as_str()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" · ");
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{status}   {meta}"))
                            .color(colour)
                            .small(),
                    )
                    .truncate(),
                )
                .on_hover_text(&q.error);
            });
            if matches!(q.status, Status::Running | Status::Done)
                || q.progress.fraction.is_some_and(|f| f > 0.0)
            {
                let frac = q.progress.fraction.unwrap_or(0.0);
                let mut text = progress_text(q);
                if q.progress.fraction.is_none() && q.progress.downloaded > 0 {
                    // Live streams and HLS without a size: show bytes, no endless animation.
                    text = format!("{text} · {}", format_bytes(q.progress.downloaded));
                }
                ui.add(egui::ProgressBar::new(frac).desired_width(200.0).text(text));
            } else {
                ui.add_sized([200.0, 18.0], egui::Label::new(""));
            }
            match q.status {
                Status::Running | Status::Queued => {
                    if ui.small_button("Pause").clicked() {
                        cmds.push(Cmd::Pause(q.id));
                    }
                    if ui.small_button("Cancel").clicked() {
                        cmds.push(Cmd::Cancel(q.id));
                    }
                }
                Status::Paused => {
                    if ui.small_button("Resume").clicked() {
                        cmds.push(Cmd::Resume(q.id));
                    }
                    if ui.small_button("Cancel").clicked() {
                        cmds.push(Cmd::Cancel(q.id));
                    }
                }
                Status::Failed | Status::Cancelled | Status::Skipped => {
                    if ui.small_button("Retry").clicked() {
                        cmds.push(Cmd::Retry(q.id));
                    }
                }
                Status::Done => {
                    if ui.small_button("Show").clicked() && !q.file_path.is_empty() {
                        let _ = crate::util::reveal_path(std::path::Path::new(&q.file_path));
                    }
                }
            }
            if ui.small_button("▲").on_hover_text("Move up").clicked() {
                cmds.push(Cmd::Move(q.id, MoveTo::Up));
            }
            if ui.small_button("▼").on_hover_text("Move down").clicked() {
                cmds.push(Cmd::Move(q.id, MoveTo::Down));
            }
            if ui
                .small_button("Top")
                .on_hover_text("Download next")
                .clicked()
            {
                cmds.push(Cmd::Move(q.id, MoveTo::Top));
            }
            if ui
                .small_button("Remove")
                .on_hover_text("Remove from the queue (finished items stay in the library)")
                .clicked()
            {
                cmds.push(Cmd::Remove(q.id));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(id: &str, kind: FormatKind) -> Format {
        Format {
            id: id.into(),
            ext: String::new(),
            kind,
            height: None,
            fps: None,
            vcodec: String::new(),
            acodec: String::new(),
            size: None,
            approx: false,
            tbr: None,
            note: String::new(),
            resolution: String::new(),
            language: String::new(),
        }
    }

    #[test]
    fn explicit_formats() {
        let v = f("137", FormatKind::VideoOnly);
        let m = f("18", FormatKind::Muxed);
        let a = f("140", FormatKind::AudioOnly);
        assert_eq!(
            explicit_format(Mode::VideoAudio, Some(&v), Some(&a)).as_deref(),
            Some("137+140")
        );
        assert_eq!(
            explicit_format(Mode::VideoAudio, Some(&m), Some(&a)).as_deref(),
            Some("18")
        );
        assert_eq!(
            explicit_format(Mode::VideoAudio, None, Some(&a)).as_deref(),
            Some("bv*+140")
        );
        assert_eq!(
            explicit_format(Mode::VideoAudio, Some(&v), None).as_deref(),
            Some("137+ba/137")
        );
        assert_eq!(explicit_format(Mode::VideoAudio, None, None), None);
        assert_eq!(
            explicit_format(Mode::AudioOnly, Some(&v), Some(&a)).as_deref(),
            Some("140")
        );
        assert_eq!(
            explicit_format(Mode::VideoOnly, Some(&v), Some(&a)).as_deref(),
            Some("137")
        );
    }
}
