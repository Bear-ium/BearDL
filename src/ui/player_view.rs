//! Player page: embedded libmpv video with controls and a metadata panel.
use super::App;
use crate::db::Download;
use crate::engine::{Cmd, LibmpvJob, ToolsView};
use crate::player::{GetProcAddress, Player};
use crate::util::{format_bytes, format_duration, format_upload_date};
use eframe::egui;
use std::sync::Arc;

const SPEEDS: [f64; 8] = [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 3.0];

#[derive(Default)]
pub(crate) struct TrimUi {
    open: bool,
    start: String,
    end: String,
    accurate: bool,
    previewing: bool,
}

#[derive(Default)]
pub struct PlayerUi {
    pub player: Option<Player>,
    pub error: Option<String>,
    pub current: Option<Arc<Download>>,
    pub pending: Option<(Arc<Download>, Option<f64>)>,
    pub gl_proc: Option<GetProcAddress>,
    pub show_meta: bool,
    pub fullscreen: bool,
    pub(crate) trim: TrimUi,
    /// Chapters of the current file from its library metadata, parsed once.
    pub(crate) chapters: Vec<crate::ytdlp::Chapter>,
    pub(crate) seek_drag: Option<f64>,
    pub(crate) last_size: [i32; 2],
    /// Last position saved for resuming, and when.
    pub(crate) saved: (f64, f64),
    /// Drop the player and look for libmpv again (it was installed, or its path changed).
    pub(crate) reload: bool,
    pub(crate) libmpv_rev: u64,
}

impl App {
    /// Remembers where playback is, so the file resumes there next time.
    pub(crate) fn save_resume(&mut self) {
        let Some(p) = self.player_ui.player.as_ref() else {
            return;
        };
        let (Some(d), Some(pos)) = (self.player_ui.current.as_ref(), p.position()) else {
            return;
        };
        let dur = p.duration().unwrap_or(0.0);
        // Near the end counts as finished: start from the beginning next time.
        let pos = if dur > 0.0 && pos > dur - 10.0 {
            0.0
        } else {
            pos
        };
        if (pos - self.player_ui.saved.0).abs() > 1.0 {
            self.player_ui.saved.0 = pos;
            self.engine.send(crate::engine::Cmd::SaveResume(d.id, pos));
        }
    }

    /// Starts any requested playback, polls mpv and renders the next frame. Called from
    /// `App::logic`, which also runs while the window is minimised or covered: mpv waits for
    /// each frame to be rendered, so it must keep being fed even when nothing is shown.
    pub(crate) fn drive_player(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if std::mem::take(&mut self.player_ui.reload) {
            self.save_resume();
            if let (Some(p), Some(gl)) = (self.player_ui.player.as_mut(), frame.gl()) {
                p.free_gl(gl);
            }
            self.player_ui.player = None;
            self.player_ui.current = None;
            self.player_ui.error = None;
            crate::player::forget_libmpv();
        }
        if let Some((d, start)) = self.player_ui.pending.take() {
            self.start_playback(ctx, d, start);
        }
        let Some(p) = self.player_ui.player.as_mut() else {
            return;
        };
        p.poll_events();
        let shown = ctx.input(|i| i.viewport().visible().unwrap_or(true));
        if (self.page != super::Page::Player || !shown)
            && let Some(gl) = frame.gl().cloned()
        {
            let size = self.player_ui.last_size;
            let _ = p.render(&gl, frame, if size[0] > 0 { size } else { [16, 16] });
        }
        let playing = p.loaded && !p.paused();
        if playing {
            // Keeps the clock moving for audio-only files, which produce no frames.
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
            let now = ctx.input(|i| i.time);
            if now - self.player_ui.saved.1 > 15.0 {
                self.player_ui.saved.1 = now;
                self.save_resume();
            }
        }
    }

    fn start_playback(&mut self, ctx: &egui::Context, d: Arc<Download>, start: Option<f64>) {
        let file = std::path::PathBuf::from(&d.file_path);
        if !file.is_file() {
            self.push_toast(crate::engine::Level::Error, "The file is missing".into());
            return;
        }
        // The saved position may be newer than the copy the caller has.
        let d = self
            .lib
            .db()
            .and_then(|db| db.get_download(d.id).ok().flatten())
            .map(Arc::new)
            .unwrap_or(d);
        let start = start.or_else(|| {
            let r = d.resume_pos;
            let dur = d.duration.unwrap_or(f64::MAX);
            (r > 5.0 && r < dur - 10.0).then_some(r)
        });
        self.save_resume();
        if self.player_ui.player.is_none() && self.player_ui.error.is_none() {
            match self.player_ui.gl_proc.clone() {
                Some(proc_address) => {
                    match Player::new(ctx, proc_address, &self.saved_settings.libmpv_path) {
                        Ok(p) => self.player_ui.player = Some(p),
                        Err(e) => self.player_ui.error = Some(e),
                    }
                }
                None => self.player_ui.error = Some("no OpenGL context".into()),
            }
        }
        match self.player_ui.player.as_mut() {
            Some(p) => {
                p.download_id = Some(d.id);
                if let Err(e) = p.open(&file, start) {
                    self.push_toast(crate::engine::Level::Error, format!("Could not play: {e}"));
                    return;
                }
                self.player_ui.saved = (start.unwrap_or(0.0), ctx.input(|i| i.time));
                self.player_ui.chapters = crate::ytdlp::parse_chapters(&d.chapters_json);
                // A new file starts with a fresh trim selection.
                self.player_ui.trim = TrimUi {
                    open: self.player_ui.trim.open,
                    ..Default::default()
                };
                self.player_ui.current = Some(d);
                self.page = super::Page::Player;
            }
            None => {
                // No libmpv: hand the file to an external mpv, or the system player.
                let why = self
                    .player_ui
                    .error
                    .clone()
                    .unwrap_or_else(|| "libmpv is not available".into());
                let msg = match crate::player::open_external(&self.settings.mpv_path, &file, start)
                {
                    Ok(m) => format!("{m} (embedded player unavailable: {why})"),
                    Err(_) => {
                        let opened = crate::util::open_path(&file);
                        let hint =
                            if crate::player::libmpv(&self.saved_settings.libmpv_path).is_ok() {
                                String::new()
                            } else {
                                format!(" {}", crate::player::install_hint())
                            };
                        match opened {
                            Ok(()) => format!(
                                "Opened in the default player (embedded player unavailable: {why}).{hint}"
                            ),
                            Err(e) => format!("Could not open the file: {e}.{hint}"),
                        }
                    }
                };
                self.push_toast(crate::engine::Level::Info, msg);
            }
        }
    }
}

impl PlayerUi {
    /// Development screenshots: open the trim panel with a selection.
    #[cfg(feature = "dev-shots")]
    pub fn dev_open_trim(&mut self) {
        self.trim.open = true;
        self.trim.start = "0:02".into();
        self.trim.end = "0:05".into();
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
    let has_file = app
        .player_ui
        .player
        .as_ref()
        .is_some_and(|p| !p.file.as_os_str().is_empty());
    if !has_file {
        empty(app, ui);
        return;
    }
    keyboard(app, ui);
    if app.player_ui.show_meta && !app.player_ui.fullscreen {
        egui::Panel::right("player_meta")
            .resizable(true)
            .default_size(320.0)
            .min_size(220.0)
            .show(ui, |ui| meta_panel(app, ui));
    }
    let mut controls_h = if ui.available_width() < 900.0 {
        92.0
    } else {
        64.0
    };
    if app.player_ui.trim.open {
        controls_h += 80.0;
    }
    let avail = ui.available_size();
    let video = egui::vec2(avail.x, (avail.y - controls_h).max(60.0));
    let ppp = ui.ctx().pixels_per_point();
    let px = [(video.x * ppp) as i32, (video.y * ppp) as i32];
    app.player_ui.last_size = px;
    let tex = match (frame.gl().cloned(), app.player_ui.player.as_mut()) {
        (Some(gl), Some(p)) => p.render(&gl, frame, px),
        _ => None,
    };
    let (rect, resp) = ui.allocate_exact_size(video, egui::Sense::click());
    ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
    if let Some(t) = tex {
        egui::Image::new(egui::load::SizedTexture::new(t, video)).paint_at(ui, rect);
    }
    if resp.clicked()
        && let Some(p) = &app.player_ui.player
    {
        p.toggle_pause();
    }
    if resp.double_clicked() {
        // The first click of the double-click paused; undo that.
        if let Some(p) = &app.player_ui.player {
            p.toggle_pause();
        }
        toggle_fullscreen(app, ui.ctx());
    }
    controls(app, ui);
    if app.player_ui.trim.open {
        trim_panel(app, ui);
    }
}

fn trim_panel(app: &mut App, ui: &mut egui::Ui) {
    let Some(p) = app.player_ui.player.as_ref() else {
        return;
    };
    let Some(d) = app.player_ui.current.clone() else {
        return;
    };
    let pos = p.position().unwrap_or(0.0);
    let dur = p.duration().or(d.duration);
    let t = &mut app.player_ui.trim;
    let mut cmd = None;
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.label("Start");
        ui.add(
            egui::TextEdit::singleline(&mut t.start)
                .hint_text("0:00")
                .desired_width(70.0),
        );
        if ui
            .small_button("Set")
            .on_hover_text("Use the current position")
            .clicked()
        {
            t.start = format_duration(pos);
        }
        ui.label("End");
        ui.add(
            egui::TextEdit::singleline(&mut t.end)
                .hint_text(dur.map(format_duration).unwrap_or_default())
                .desired_width(70.0),
        );
        if ui
            .small_button("Set")
            .on_hover_text("Use the current position")
            .clicked()
        {
            t.end = format_duration(pos);
        }
        let chapters = app.player_ui.chapters.clone();
        let marks = &p.chapters;
        if !chapters.is_empty() || !marks.is_empty() {
            // Chapters from the library metadata, or those mpv found in the file.
            let list: Vec<crate::ytdlp::Chapter> = if chapters.is_empty() {
                marks
                    .iter()
                    .map(|m| crate::ytdlp::Chapter {
                        start: m.time,
                        end: f64::NAN,
                        title: m.title.clone(),
                    })
                    .collect()
            } else {
                chapters
            };
            egui::ComboBox::from_id_salt("trim_chapter")
                .selected_text("Chapter")
                .width(140.0)
                .show_ui(ui, |ui| {
                    for (i, c) in list.iter().enumerate() {
                        if ui
                            .selectable_label(
                                false,
                                format!("{}  {}", format_duration(c.start), c.title),
                            )
                            .clicked()
                            && let Some((a, b)) = crate::trim::chapter_range(&list, i, dur)
                        {
                            t.start = format_duration(a);
                            t.end = format_duration(b);
                        }
                    }
                });
        }
        ui.separator();
        ui.radio_value(&mut t.accurate, false, "Fast")
            .on_hover_text("Copies the streams: instant, but cuts at the nearest keyframes");
        ui.radio_value(&mut t.accurate, true, "Accurate")
            .on_hover_text("Re-encodes for exact cuts: slower; keeps video and audio only");
    });
    // Empty fields mean the start or end of the file; anything else must parse.
    let parse = |s: &str, default: Option<f64>| -> Result<f64, String> {
        if s.trim().is_empty() {
            default.ok_or_else(|| "set an end time".to_owned())
        } else {
            crate::util::parse_duration(s).ok_or_else(|| format!("“{s}” is not a time like 1:23"))
        }
    };
    let parsed = parse(&t.start, Some(0.0)).and_then(|a| Ok((a, parse(&t.end, dur)?)));
    let (start, end) = parsed.clone().unwrap_or((0.0, 0.0));
    let valid = parsed.and_then(|(a, b)| crate::trim::validate(a, b, dur));
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(valid.is_ok(), egui::Button::new("Preview"))
            .on_hover_text("Loop the selection in the player")
            .clicked()
        {
            // A loop ending at the very end would stop at end-of-file instead of looping.
            let b = dur.map_or(end, |d| end.min(d - 0.05));
            p.set_loop(Some((start, b)));
            p.seek(start);
            if p.paused() {
                p.toggle_pause();
            }
            t.previewing = true;
        }
        if t.previewing && ui.button("Stop preview").clicked() {
            p.set_loop(None);
            t.previewing = false;
        }
        if ui
            .add_enabled(valid.is_ok(), egui::Button::new("Save clip"))
            .on_hover_text("Write the selection to a new file next to this one")
            .clicked()
        {
            cmd = Some(Cmd::Trim {
                id: d.id,
                start,
                end,
                accurate: t.accurate,
            });
        }
        if let Err(e) = &valid {
            if !t.start.is_empty() || !t.end.is_empty() {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
        } else {
            ui.weak(format!("{} selected", format_duration(end - start)));
        }
    });
    let jobs = app.engine.state().trims.clone();
    for j in jobs.iter().rev().take(3) {
        ui.horizontal(|ui| match (&j.error, j.done) {
            (Some(e), _) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("{}: {e}", j.label));
            }
            (None, true) => {
                ui.weak(format!("Saved: {}", j.label));
            }
            (None, false) => {
                ui.add(
                    egui::ProgressBar::new(j.fraction.unwrap_or(0.0))
                        .desired_width(160.0)
                        .show_percentage(),
                );
                ui.label(&j.label);
            }
        });
    }
    if let Some(c) = cmd {
        app.engine.send(c);
    }
}

fn empty(app: &mut App, ui: &mut egui::Ui) {
    ui.heading("Player");
    ui.weak("Nothing playing. Double-click an item in the Library, or press Enter on it.");
    ui.add_space(8.0);
    match crate::player::libmpv(&app.saved_settings.libmpv_path) {
        Ok(m) => {
            ui.weak(format!("libmpv: {}", m.path.display()));
        }
        Err(e) => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "The built-in player needs libmpv, which was not found.",
            );
            ui.weak(e);
            ui.add_space(4.0);
            let tools = app.engine.state().tools.clone();
            if let Some(cmd) = libmpv_setup(&tools, ui) {
                app.engine.send(cmd);
            }
            ui.add_space(4.0);
            ui.weak("Until then, files open in an external mpv or the system player.");
        }
    }
    if let Some(e) = &app.player_ui.error {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
}

/// How to get libmpv: on Windows the download button, its progress and any error; elsewhere
/// the package manager command. Returns the command for a button that was pressed.
pub(super) fn libmpv_setup(t: &ToolsView, ui: &mut egui::Ui) -> Option<Cmd> {
    if !crate::libmpv::downloadable() {
        let i = crate::libmpv::instructions();
        ui.add(egui::Label::new(i.text).wrap());
        if let Some(command) = i.command {
            ui.horizontal(|ui| {
                ui.code(command);
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(command.to_owned());
                }
            });
        }
        return None;
    }
    let mut cmd = None;
    match t.libmpv_job {
        LibmpvJob::Idle => {
            if ui
                .button("Download libmpv")
                .on_hover_text(format!(
                    "Downloads about 30 MB from {} and keeps only {}",
                    crate::libmpv::RELEASES,
                    crate::libmpv::DLL
                ))
                .clicked()
            {
                cmd = Some(Cmd::InstallLibmpv);
            }
        }
        LibmpvJob::Waiting => {
            ui.horizontal(|ui| {
                ui.label("Waiting for another download to finish");
                if ui.button("Cancel").clicked() {
                    cmd = Some(Cmd::CancelLibmpv);
                }
            });
        }
        LibmpvJob::Running => {
            ui.horizontal(|ui| {
                let job = t.busy.clone().unwrap_or_default();
                match job.total.filter(|t| *t > 0) {
                    Some(total) => {
                        ui.add(
                            egui::ProgressBar::new(job.done as f32 / total as f32)
                                .desired_width(180.0)
                                .show_percentage(),
                        );
                        ui.label(format!(
                            "{} {} / {}",
                            job.label,
                            format_bytes(job.done),
                            format_bytes(total)
                        ));
                    }
                    None => {
                        ui.label(format!("{}…", job.label));
                    }
                }
                if ui.button("Cancel").clicked() {
                    cmd = Some(Cmd::CancelLibmpv);
                }
            });
        }
    }
    if let Some(e) = &t.libmpv_error {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("The download failed: {e}"))
                    .color(ui.visuals().error_fg_color),
            )
            .wrap(),
        );
    }
    cmd
}

fn toggle_fullscreen(app: &mut App, ctx: &egui::Context) {
    app.player_ui.fullscreen = !app.player_ui.fullscreen;
    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(app.player_ui.fullscreen));
}

fn keyboard(app: &mut App, ui: &mut egui::Ui) {
    if ui.ctx().memory(|m| m.focused().is_some()) {
        return;
    }
    let keys = ui.input(|i| {
        [
            i.key_pressed(egui::Key::Space),
            i.key_pressed(egui::Key::ArrowLeft),
            i.key_pressed(egui::Key::ArrowRight),
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::F),
            i.key_pressed(egui::Key::M),
            i.key_pressed(egui::Key::Escape),
        ]
    });
    let Some(p) = &app.player_ui.player else {
        return;
    };
    let [space, left, right, up, down, f, m, esc] = keys;
    if space {
        p.toggle_pause();
    }
    if left {
        p.seek_relative(-5.0);
    }
    if right {
        p.seek_relative(5.0);
    }
    if up || down {
        let v = p.get_f64("volume").unwrap_or(100.0) + if up { 5.0 } else { -5.0 };
        p.set_f64("volume", v.clamp(0.0, 130.0));
    }
    if m {
        let muted = p.get_flag("mute").unwrap_or(false);
        p.set("mute", if muted { "no" } else { "yes" });
    }
    let popup_open = egui::Popup::is_any_open(ui.ctx());
    if f || (esc && app.player_ui.fullscreen && !popup_open) {
        toggle_fullscreen(app, ui.ctx());
    }
}

fn controls(app: &mut App, ui: &mut egui::Ui) {
    let Some(p) = app.player_ui.player.as_mut() else {
        return;
    };
    let pos = p.position().unwrap_or(0.0);
    let dur = p.duration().unwrap_or(0.0).max(pos);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - 120.0).max(100.0);
        let mut v = app.player_ui.seek_drag.unwrap_or(pos);
        let r = ui.add(
            egui::Slider::new(&mut v, 0.0..=dur.max(0.1))
                .show_value(false)
                .trailing_fill(true),
        );
        if r.dragged() {
            app.player_ui.seek_drag = Some(v);
        }
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            p.seek(v);
            app.player_ui.seek_drag = None;
        }
        ui.label(format!("{} / {}", format_duration(v), format_duration(dur)));
    });
    ui.horizontal_wrapped(|ui| {
        let paused = p.paused();
        if ui
            .button(if paused { "▶ Play" } else { "❚❚ Pause" })
            .clicked()
        {
            p.toggle_pause();
        }
        if ui.button("−10 s").clicked() {
            p.seek_relative(-10.0);
        }
        if ui.button("+10 s").clicked() {
            p.seek_relative(10.0);
        }
        ui.separator();
        let mut vol = p.get_f64("volume").unwrap_or(100.0);
        ui.label("Volume");
        if ui
            .add(egui::Slider::new(&mut vol, 0.0..=130.0).show_value(false))
            .changed()
        {
            p.set_f64("volume", vol);
        }
        let speed = p.get_f64("speed").unwrap_or(1.0);
        egui::ComboBox::from_id_salt("speed")
            .selected_text(format!("{speed}×"))
            .width(52.0)
            .show_ui(ui, |ui| {
                for s in SPEEDS {
                    if ui.selectable_label(speed == s, format!("{s}×")).clicked() {
                        p.set_f64("speed", s);
                    }
                }
            });
        track_combo(ui, p, "audio", "Audio");
        track_combo(ui, p, "sub", "Subtitles");
        if !p.chapters.is_empty() {
            let current = p.get_i64("chapter").unwrap_or(-1);
            let name = usize::try_from(current)
                .ok()
                .and_then(|i| p.chapters.get(i))
                .map_or("Chapters".to_owned(), |c| c.title.clone());
            let chapters = p.chapters.clone();
            egui::ComboBox::from_id_salt("chapter")
                .selected_text(name)
                .width(130.0)
                .show_ui(ui, |ui| {
                    for (i, c) in chapters.iter().enumerate() {
                        let label = format!("{}  {}", format_duration(c.time), c.title);
                        if ui.selectable_label(current == i as i64, label).clicked() {
                            p.seek(c.time);
                        }
                    }
                });
        }
        ui.separator();
        if ui
            .toggle_value(&mut app.player_ui.trim.open, "Trim")
            .changed()
            && !app.player_ui.trim.open
            && app.player_ui.trim.previewing
        {
            p.set_loop(None);
            app.player_ui.trim.previewing = false;
        }
        ui.toggle_value(&mut app.player_ui.show_meta, "Details");
        if ui.button("Fullscreen").clicked() {
            app.player_ui.fullscreen = !app.player_ui.fullscreen;
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Fullscreen(app.player_ui.fullscreen));
        }
    });
}

fn track_combo(ui: &mut egui::Ui, p: &mut Player, kind: &str, label: &str) {
    let tracks: Vec<_> = p
        .tracks
        .iter()
        .filter(|t| t.kind == kind)
        .cloned()
        .collect();
    if tracks.is_empty() {
        return;
    }
    let selected = tracks.iter().find(|t| t.selected);
    let text = selected.map_or(format!("{label}: off"), |t| {
        format!("{label}: {}", t.label())
    });
    let mut choice: Option<Option<i64>> = None;
    egui::ComboBox::from_id_salt(kind)
        .selected_text(text)
        .width(120.0)
        .show_ui(ui, |ui| {
            if kind == "sub" && ui.selectable_label(selected.is_none(), "Off").clicked() {
                choice = Some(None);
            }
            for t in &tracks {
                if ui.selectable_label(t.selected, t.label()).clicked() {
                    choice = Some(Some(t.id));
                }
            }
        });
    if let Some(c) = choice {
        p.select_track(kind, c);
    }
}

fn meta_panel(app: &mut App, ui: &mut egui::Ui) {
    let Some(d) = app.player_ui.current.clone() else {
        return;
    };
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add(egui::Label::new(egui::RichText::new(&d.title).heading()).wrap());
        if !d.channel.is_empty() {
            ui.label(&d.channel);
        }
        egui::Grid::new("pmeta")
            .num_columns(2)
            .spacing([10.0, 3.0])
            .show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    if !v.is_empty() {
                        ui.weak(k);
                        ui.add(egui::Label::new(v).wrap());
                        ui.end_row();
                    }
                };
                row("Uploaded", format_upload_date(&d.upload_date));
                row(
                    "Duration",
                    d.duration.map(format_duration).unwrap_or_default(),
                );
                row(
                    "Size",
                    d.file_size
                        .map(|s| format_bytes(s as u64))
                        .unwrap_or_default(),
                );
                row("Format", d.format_note.clone());
                row("File", d.file_path.clone());
                row("Source", d.url.clone());
            });
        let chapters = app
            .player_ui
            .player
            .as_ref()
            .map(|p| p.chapters.clone())
            .unwrap_or_default();
        if !chapters.is_empty() {
            ui.add_space(6.0);
            ui.strong("Chapters");
            for c in &chapters {
                if ui
                    .add(
                        egui::Button::new(format!("{}  {}", format_duration(c.time), c.title))
                            .frame(false),
                    )
                    .clicked()
                    && let Some(p) = &app.player_ui.player
                {
                    p.seek(c.time);
                }
            }
        }
        if !d.description.is_empty() {
            ui.add_space(6.0);
            ui.strong("Description");
            ui.add(egui::Label::new(&d.description).wrap().selectable(true));
        }
    });
}
