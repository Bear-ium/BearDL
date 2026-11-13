//! Search by spoken words: full-text search over every indexed subtitle cue. A click opens
//! the player at that moment.
use super::App;
use crate::db::{CueHit, Db};
use crate::util::format_duration;
use eframe::egui;

const DELAY: f64 = 0.25;
const LIMIT: usize = 500;

/// (query generation, hits or error) from the search thread.
type SearchResult = (u64, Result<Vec<CueHit>, String>);

#[derive(Default)]
pub struct SearchUi {
    db: Option<Db>,
    error: Option<String>,
    input: String,
    edited: Option<f64>,
    query: String,
    /// Hits grouped by video, in rank order of each video's best hit.
    groups: Vec<(i64, String, String, Vec<CueHit>)>,
    stats: Option<(i64, i64)>,
    seen_rev: Option<u64>,
    generation: u64,
    pending: Option<std::sync::mpsc::Receiver<SearchResult>>,
}

impl SearchUi {
    /// Development screenshots: start with a query.
    #[cfg(feature = "dev-shots")]
    pub fn dev_set(&mut self, q: &str) {
        self.input = q.to_owned();
        self.edited = Some(0.0);
    }

    fn db(&mut self) -> Option<&Db> {
        if self.db.is_none() && self.error.is_none() {
            match Db::open(&crate::paths::db_path()) {
                Ok(db) => self.db = Some(db),
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        self.db.as_ref()
    }

    /// Starts the query on a worker thread with its own connection, so a slow search over
    /// millions of cues never blocks the UI. Results arrive in `poll`.
    fn run(&mut self, ctx: &egui::Context) {
        self.generation += 1;
        let Some(fts) = crate::subtitles::cue_query(&self.query) else {
            self.groups.clear();
            self.pending = None;
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let generation = self.generation;
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("search".into())
            .spawn(move || {
                let res = Db::open(&crate::paths::db_path())
                    .and_then(|db| db.search_cues(&fts, LIMIT))
                    .map_err(|e| e.to_string());
                let _ = tx.send((generation, res));
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.pending = Some(rx);
        }
    }

    fn poll(&mut self) {
        let Some(rx) = &self.pending else { return };
        let Ok((generation, res)) = rx.try_recv() else {
            return;
        };
        self.pending = None;
        if generation != self.generation {
            return;
        }
        let hits = match res {
            Ok(h) => h,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        self.error = None;
        self.groups.clear();
        for h in hits {
            match self.groups.iter_mut().find(|g| g.0 == h.download_id) {
                Some(g) => g.3.push(h),
                None => {
                    self.groups
                        .push((h.download_id, h.title.clone(), h.channel.clone(), vec![h]))
                }
            }
        }
        for g in &mut self.groups {
            g.3.sort_by(|a, b| a.start.total_cmp(&b.start));
        }
    }
}

/// Lays out a snippet with matches (between \u{2} and \u{3}) in bold accent colour.
fn snippet_job(s: &str, ui: &egui::Ui) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let body = egui::TextStyle::Body.resolve(ui.style());
    let normal = egui::TextFormat::simple(body.clone(), ui.visuals().text_color());
    let strong = egui::TextFormat {
        color: ui.visuals().hyperlink_color,
        underline: egui::Stroke::new(1.0, ui.visuals().hyperlink_color),
        ..egui::TextFormat::simple(body, ui.visuals().strong_text_color())
    };
    let mut rest = s;
    while let Some(i) = rest.find('\u{2}') {
        job.append(&rest[..i], 0.0, normal.clone());
        let after = &rest[i + 1..];
        let end = after.find('\u{3}').unwrap_or(after.len());
        job.append(&after[..end], 0.0, strong.clone());
        rest = after.get(end + 1..).unwrap_or("");
    }
    job.append(rest, 0.0, normal);
    job.wrap.max_width = ui.available_width();
    job
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let rev = app.engine.state().library_rev;
    let s = &mut app.search;
    s.poll();
    if s.seen_rev != Some(rev) {
        s.seen_rev = Some(rev);
        s.stats = s.db().and_then(|db| db.cue_stats().ok());
        if !s.query.is_empty() {
            s.run(ui.ctx());
        }
    }
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut s.input)
                .hint_text("Search what was said, e.g. elephants or \"exact phrase\"")
                .desired_width(420.0),
        );
        if r.changed() {
            s.edited = Some(ui.input(|i| i.time));
        }
        if let Some((videos, tracks)) = s.stats {
            ui.weak(format!(
                "{} in {} indexed",
                crate::util::plural(tracks as u64, "subtitle track", "subtitle tracks"),
                crate::util::plural(videos as u64, "video", "videos")
            ));
        }
    });
    if let Some(t) = s.edited {
        let now = ui.input(|i| i.time);
        if now - t >= DELAY {
            s.edited = None;
            if s.input.trim() != s.query {
                s.query = s.input.trim().to_owned();
                s.run(ui.ctx());
            }
        } else {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(DELAY - (now - t)));
        }
    }
    if let Some(e) = &s.error {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
    ui.separator();
    if s.query.is_empty() {
        ui.weak(
            "Subtitles and automatic captions are downloaded with each video (see presets) \
             and every line is indexed here. Click a result to play from that moment.",
        );
        return;
    }
    if s.groups.is_empty() {
        ui.weak("No matches.");
        return;
    }
    let total: usize = s.groups.iter().map(|g| g.3.len()).sum();
    ui.weak(format!(
        "{} in {}{}",
        crate::util::plural(total as u64, "match", "matches"),
        crate::util::plural(s.groups.len() as u64, "video", "videos"),
        if total >= LIMIT {
            " (showing the best)"
        } else {
            ""
        }
    ));
    let mut play: Option<(i64, f64)> = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (id, title, channel, hits) in &s.groups {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Label::new(egui::RichText::new(title).strong())
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_text("Play from the first match")
                        .clicked()
                    {
                        play = Some((*id, hits[0].start));
                    }
                    if !channel.is_empty() {
                        ui.weak(channel);
                    }
                    ui.weak(crate::util::plural(hits.len() as u64, "match", "matches"));
                });
                for h in hits.iter().take(50) {
                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::Button::new(format_duration(h.start)).small())
                            .on_hover_text("Play from here")
                            .clicked()
                        {
                            play = Some((*id, h.start));
                        }
                        let job = snippet_job(&h.snippet, ui);
                        if ui
                            .add(egui::Label::new(job).sense(egui::Sense::click()))
                            .clicked()
                        {
                            play = Some((*id, h.start));
                        }
                    });
                }
                if hits.len() > 50 {
                    ui.weak(format!("and {} more", hits.len() - 50));
                }
            }
        });
    if let Some((id, start)) = play {
        let d = s.db().and_then(|db| db.get_download(id).ok().flatten());
        match d {
            // Start a moment early so the words are heard from their beginning.
            Some(d) => app.play(&d, Some((start - 1.0).max(0.0))),
            None => app.push_toast(
                crate::engine::Level::Error,
                "That video is no longer in the library".into(),
            ),
        }
    }
}
