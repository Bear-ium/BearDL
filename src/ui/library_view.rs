//! Library and history: virtualised list over paged SQL queries, plus the quick view panel.
use super::App;
use crate::db::{Db, Download, LibraryQuery, LibraryRow, SortKey, Status};
use crate::engine::Cmd;
use crate::thumbs::{ThumbCache, small_path};
use crate::util::{format_bytes, format_duration, format_timestamp, format_upload_date};
use eframe::egui;
use std::collections::HashMap;
use std::path::Path;

const PAGE: usize = 100;
const ROW_H: f32 = 44.0;

#[derive(Default)]
pub struct LibraryUi {
    db: Option<Db>,
    db_error: Option<String>,
    query: LibraryQuery,
    count: usize,
    pages: HashMap<usize, Vec<LibraryRow>>,
    seen_rev: Option<u64>,
    dirty: bool,
    selected: Option<(usize, i64)>,
    scroll_to: Option<usize>,
    pub quick_view: bool,
    detail: Option<Download>,
    confirm_delete: Option<i64>,
    thumbs: Option<ThumbCache>,
    scanned: bool,
    scroll_offset: f32,
}

impl LibraryUi {
    /// The UI keeps its own read connection, opened on first use to keep start-up fast.
    fn db(&mut self) -> Option<&Db> {
        if self.db.is_none() && self.db_error.is_none() {
            match Db::open(&crate::paths::db_path()) {
                Ok(db) => self.db = Some(db),
                Err(e) => self.db_error = Some(e.to_string()),
            }
        }
        self.db.as_ref()
    }

    fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn refresh(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.pages.clear();
        let q = self.query.clone();
        self.count = self
            .db()
            .and_then(|db| db.library_count(&q).ok())
            .unwrap_or(0);
        if let Some((_, id)) = self.selected {
            self.detail = self.db().and_then(|db| db.get_download(id).ok().flatten());
            if self.detail.is_none() {
                self.selected = None;
            }
        }
    }

    fn row(&mut self, i: usize) -> Option<&LibraryRow> {
        let page = i / PAGE;
        if !self.pages.contains_key(&page) {
            let q = self.query.clone();
            let rows = self
                .db()
                .and_then(|db| db.library_page(&q, page * PAGE, PAGE).ok())
                .unwrap_or_default();
            // Keep memory bounded when scrolling through a huge library.
            if self.pages.len() > 20 {
                self.pages.clear();
            }
            self.pages.insert(page, rows);
        }
        self.pages.get(&page)?.get(i % PAGE)
    }

    fn select(&mut self, i: usize) {
        let Some(id) = self.row(i).map(|r| r.id) else {
            return;
        };
        self.selected = Some((i, id));
        self.detail = self.db().and_then(|db| db.get_download(id).ok().flatten());
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let rev = app.engine.state().library_rev;
    let lib = &mut app.lib;
    if lib.thumbs.is_none() {
        lib.thumbs = Some(ThumbCache::new(ui.ctx().clone(), 400));
    }
    if lib.seen_rev != Some(rev) {
        lib.seen_rev = Some(rev);
        lib.invalidate();
    }
    if !lib.scanned {
        lib.scanned = true;
        app.engine.send(Cmd::ScanFiles);
    }
    if let Some(e) = &lib.db_error {
        ui.colored_label(
            ui.visuals().error_fg_color,
            format!("Library unavailable: {e}"),
        );
        return;
    }
    toolbar(app, ui);
    app.lib.refresh();
    keyboard(app, ui);
    if app.lib.quick_view && app.lib.detail.is_some() {
        egui::Panel::right("quick_view")
            .resizable(true)
            .default_size(360.0)
            .min_size(260.0)
            .show(ui, |ui| quick_view(app, ui));
    }
    list(app, ui);
    delete_dialog(app, ui);
}

fn toolbar(app: &mut App, ui: &mut egui::Ui) {
    let lib = &mut app.lib;
    let before = lib.query.clone();
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut lib.query.text)
                .hint_text("Search title, channel, description")
                .desired_width(260.0),
        );
        egui::ComboBox::from_id_salt("lib_status")
            .selected_text(lib.query.status.map_or("Any status", |s| s.label()))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut lib.query.status, None, "Any status");
                for s in [
                    Status::Done,
                    Status::Failed,
                    Status::Cancelled,
                    Status::Skipped,
                ] {
                    ui.selectable_value(&mut lib.query.status, Some(s), s.label());
                }
            });
        ui.checkbox(&mut lib.query.missing_only, "Missing files");
        ui.separator();
        ui.label("Sort");
        egui::ComboBox::from_id_salt("lib_sort")
            .selected_text(sort_label(lib.query.sort))
            .show_ui(ui, |ui| {
                for k in [
                    SortKey::Date,
                    SortKey::Title,
                    SortKey::Channel,
                    SortKey::Duration,
                    SortKey::Size,
                ] {
                    ui.selectable_value(&mut lib.query.sort, k, sort_label(k));
                }
            });
        let arrow = if lib.query.descending { "↓" } else { "↑" };
        if ui.button(arrow).on_hover_text("Reverse order").clicked() {
            lib.query.descending = !lib.query.descending;
        }
        if !lib.query.channel.is_empty()
            && ui
                .button(format!("Channel: {} ✕", lib.query.channel))
                .clicked()
        {
            lib.query.channel.clear();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("Check files")
                .on_hover_text("Find files that were moved or deleted")
                .clicked()
            {
                app.engine.send(Cmd::ScanFiles);
            }
            ui.weak(format!("{} items", lib.count));
        });
    });
    if lib.query != before {
        lib.selected = None;
        lib.detail = None;
        lib.invalidate();
    }
    ui.separator();
}

fn sort_label(k: SortKey) -> &'static str {
    match k {
        SortKey::Date => "Date added",
        SortKey::Title => "Title",
        SortKey::Channel => "Channel",
        SortKey::Duration => "Duration",
        SortKey::Size => "Size",
    }
}

fn keyboard(app: &mut App, ui: &mut egui::Ui) {
    if ui.ctx().memory(|m| m.focused().is_some()) || app.lib.confirm_delete.is_some() {
        return;
    }
    let (down, up, space, del, enter, pgdn, pgup) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::Space),
            i.key_pressed(egui::Key::Delete),
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::PageDown),
            i.key_pressed(egui::Key::PageUp),
        )
    });
    let lib = &mut app.lib;
    if lib.count == 0 {
        return;
    }
    let cur = lib.selected.map(|(i, _)| i);
    let step = |d: isize| -> usize {
        let base = cur.map_or(-1, |c| c as isize);
        (base + d).clamp(0, lib.count as isize - 1) as usize
    };
    let target = if down {
        Some(step(1))
    } else if up {
        Some(step(-1))
    } else if pgdn {
        Some(step(10))
    } else if pgup {
        Some(step(-10))
    } else {
        None
    };
    if let Some(t) = target {
        lib.select(t);
        lib.scroll_to = Some(t);
    }
    if space && lib.selected.is_some() {
        lib.quick_view = !lib.quick_view;
    }
    if del && let Some((_, id)) = lib.selected {
        lib.confirm_delete = Some(id);
    }
    if enter && let Some(d) = lib.detail.clone() {
        app.play(&d, None);
    }
}

fn list(app: &mut App, ui: &mut egui::Ui) {
    let lib = &mut app.lib;
    if lib.count == 0 {
        ui.weak(if lib.query.text.is_empty() && lib.query.status.is_none() {
            "Your downloads will appear here."
        } else {
            "Nothing matches."
        });
        return;
    }
    let mut area = egui::ScrollArea::vertical()
        .id_salt("library")
        .auto_shrink([false, false]);
    if let Some(i) = lib.scroll_to.take() {
        // Keep the keyboard selection in view.
        let spacing = ui.spacing().item_spacing.y;
        let y = i as f32 * (ROW_H + spacing);
        let view_h = ui.available_height();
        let current = lib.scroll_offset;
        if y < current || y + ROW_H > current + view_h {
            area = area.vertical_scroll_offset((y - view_h / 2.0).max(0.0));
        }
    }
    let mut clicked = None;
    let mut double = None;
    let mut channel_filter = None;
    let out = area.show_rows(ui, ROW_H, lib.count, |ui, range| {
        for i in range {
            let Some(row) = lib.row(i).cloned() else {
                continue;
            };
            let selected = lib.selected.is_some_and(|(_, id)| id == row.id);
            let thumb = lib
                .thumbs
                .as_mut()
                .and_then(|t| list_thumb(t, &row.thumb_path));
            let r = library_row(ui, &row, selected, thumb);
            if r.clicked() {
                clicked = Some(i);
            }
            if r.double_clicked() {
                double = Some(i);
            }
            r.context_menu(|ui| {
                if ui.button("Show only this channel").clicked() {
                    channel_filter = Some(row.channel.clone());
                    ui.close();
                }
            });
        }
    });
    lib.scroll_offset = out.state.clamped_offset().y;
    if let Some(i) = clicked {
        lib.select(i);
    }
    if let Some(c) = channel_filter.filter(|c| !c.is_empty()) {
        lib.query.channel = c;
        lib.invalidate();
    }
    if let Some(i) = double {
        lib.select(i);
        if let Some(d) = lib.detail.clone() {
            app.play(&d, None);
        }
    }
}

fn list_thumb(cache: &mut ThumbCache, thumb: &str) -> Option<egui::TextureHandle> {
    if thumb.is_empty() {
        return None;
    }
    let small = small_path(Path::new(thumb));
    if small.is_file() {
        cache.get(&small, 96)
    } else {
        cache.get(Path::new(thumb), 96)
    }
}

fn library_row(
    ui: &mut egui::Ui,
    row: &LibraryRow,
    selected: bool,
    thumb: Option<egui::TextureHandle>,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_H),
        egui::Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let v = ui.visuals();
    if selected {
        ui.painter().rect_filled(rect, 4.0, v.selection.bg_fill);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, v.widgets.hovered.weak_bg_fill);
    }
    let thumb_rect = egui::Rect::from_min_size(
        rect.min + egui::vec2(4.0, 3.0),
        egui::vec2(68.0, ROW_H - 6.0),
    );
    match thumb {
        Some(t) => {
            egui::Image::new(&t)
                .fit_to_exact_size(thumb_rect.size())
                .corner_radius(3.0)
                .paint_at(ui, thumb_rect);
        }
        None => {
            ui.painter()
                .rect_filled(thumb_rect, 3.0, v.extreme_bg_color);
        }
    }
    let text_col = if selected {
        v.selection.stroke.color
    } else {
        v.text_color()
    };
    let weak = v.weak_text_color();
    let x = thumb_rect.right() + 10.0;
    let right_w = 260.0;
    let title_w = (rect.right() - right_w - x).max(80.0);
    let painter = ui.painter_at(rect);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let mut title_job = egui::text::LayoutJob::simple_singleline(row.title.clone(), font, text_col);
    title_job.wrap = egui::text::TextWrapping::truncate_at_width(title_w);
    let title_galley = ui.fonts_mut(|f| f.layout_job(title_job));
    painter.galley(egui::pos2(x, rect.top() + 4.0), title_galley, text_col);
    let mut sub = row.channel.clone();
    if row.missing {
        sub = format!("File missing · {sub}");
    }
    if let Some(s) = row.status.filter(|s| *s != Status::Done) {
        sub = format!("{} · {sub}", s.label());
    }
    let sub_col = if row.missing || row.status == Some(Status::Failed) {
        v.error_fg_color
    } else {
        weak
    };
    let mut job = egui::text::LayoutJob::simple_singleline(sub, small.clone(), sub_col);
    job.wrap = egui::text::TextWrapping::truncate_at_width(title_w);
    let g = ui.fonts_mut(|f| f.layout_job(job));
    painter.galley(egui::pos2(x, rect.top() + 24.0), g, sub_col);
    let cols = [
        (row.duration.map(format_duration).unwrap_or_default(), 60.0),
        (
            row.file_size
                .map(|s| format_bytes(s as u64))
                .unwrap_or_default(),
            80.0,
        ),
        (
            format_timestamp(row.created_at)
                .split(' ')
                .next()
                .unwrap_or("")
                .to_owned(),
            110.0,
        ),
    ];
    let mut cx = rect.right() - 8.0;
    for (text, w) in cols.iter().rev() {
        painter.text(
            egui::pos2(cx, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            text,
            small.clone(),
            weak,
        );
        cx -= w;
    }
    resp
}

fn quick_view(app: &mut App, ui: &mut egui::Ui) {
    let Some(d) = app.lib.detail.clone() else {
        return;
    };
    let mut cmds = Vec::new();
    egui::ScrollArea::vertical()
        .id_salt("quick")
        .show(ui, |ui| {
            if let Some(cache) = app.lib.thumbs.as_mut() {
                let w = ui.available_width();
                match cache.get(Path::new(&d.thumb_path), 480) {
                    Some(t) => {
                        let size = t.size_vec2();
                        let h = w * size.y / size.x.max(1.0);
                        ui.add(
                            egui::Image::new(&t)
                                .fit_to_exact_size(egui::vec2(w, h))
                                .corner_radius(4.0),
                        );
                    }
                    None => {
                        let (r, _) = ui.allocate_exact_size(
                            egui::vec2(w, w * 9.0 / 16.0),
                            egui::Sense::hover(),
                        );
                        ui.painter()
                            .rect_filled(r, 4.0, ui.visuals().extreme_bg_color);
                    }
                }
            }
            ui.add_space(6.0);
            ui.add(egui::Label::new(egui::RichText::new(&d.title).heading()).wrap());
            if !d.channel.is_empty() {
                ui.label(&d.channel);
            }
            ui.add_space(4.0);
            egui::Grid::new("qv_meta")
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
                    row("Added", format_timestamp(d.created_at));
                    row(
                        "Status",
                        d.status.map(|s| s.label().to_owned()).unwrap_or_default(),
                    );
                    if d.missing {
                        row("File", "missing (moved or deleted)".into());
                    }
                    row("Path", d.file_path.clone());
                    row("Source", d.url.clone());
                    if !d.error.is_empty() {
                        row("Error", d.error.clone());
                    }
                });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                let exists = !d.file_path.is_empty() && Path::new(&d.file_path).is_file();
                if ui.add_enabled(exists, egui::Button::new("Play")).clicked() {
                    app.play(&d, None);
                }
                if ui.add_enabled(exists, egui::Button::new("Open")).clicked() {
                    let _ = crate::util::open_path(Path::new(&d.file_path));
                }
                if ui
                    .add_enabled(exists, egui::Button::new("Show in folder"))
                    .clicked()
                {
                    let _ = crate::util::reveal_path(Path::new(&d.file_path));
                }
                if ui.button("Re-download").clicked() {
                    cmds.push(Cmd::Redownload(d.id));
                }
                if ui.button("Copy URL").clicked() {
                    ui.ctx().copy_text(d.url.clone());
                }
                if ui.button("Delete…").clicked() {
                    app.lib.confirm_delete = Some(d.id);
                }
            });
            let chapters = crate::ytdlp::parse_chapters(&d.chapters_json);
            if !chapters.is_empty() {
                ui.add_space(8.0);
                ui.strong("Chapters");
                for c in &chapters {
                    let label = format!("{}  {}", format_duration(c.start), c.title);
                    if ui
                        .add(egui::Button::new(label).frame(false))
                        .on_hover_text("Play from here")
                        .clicked()
                    {
                        app.play(&d, Some(c.start));
                    }
                }
            }
            if !d.description.is_empty() {
                ui.add_space(8.0);
                ui.strong("Description");
                ui.add(egui::Label::new(&d.description).wrap().selectable(true));
            }
        });
    for c in cmds {
        app.engine.send(c);
    }
}

fn delete_dialog(app: &mut App, ui: &mut egui::Ui) {
    let Some(id) = app.lib.confirm_delete else {
        return;
    };
    let title = app
        .lib
        .detail
        .as_ref()
        .filter(|d| d.id == id)
        .map(|d| d.title.clone())
        .unwrap_or_default();
    let mut close = false;
    egui::Window::new("Delete")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            ui.label(format!("Delete “{title}”?"));
            ui.horizontal(|ui| {
                if ui.button("Remove from library").clicked() {
                    app.engine.send(Cmd::DeleteEntries {
                        ids: vec![id],
                        delete_files: false,
                    });
                    close = true;
                }
                if ui.button("Delete file too").clicked() {
                    app.engine.send(Cmd::DeleteEntries {
                        ids: vec![id],
                        delete_files: true,
                    });
                    close = true;
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
        });
    if close {
        app.lib.confirm_delete = None;
        if app.lib.selected.is_some_and(|(_, s)| s == id) {
            app.lib.selected = None;
            app.lib.detail = None;
        }
    }
}
