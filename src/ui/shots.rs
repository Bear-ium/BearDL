//! Development aid (feature `dev-shots`): saves a screenshot after a delay and exits.
//! `BEARDL_SHOT=<file.png>`, `BEARDL_SHOT_DELAY=<secs>`, `BEARDL_SHOT_PAGE=<page>`,
//! `BEARDL_SHOT_URL=<url to analyse>`.
use eframe::egui;
use std::io::Write;

pub struct Shots {
    path: String,
    due: f64,
    requested: bool,
    pub page: Option<String>,
    pub url: Option<String>,
    pub select_first: bool,
}

impl Shots {
    pub fn from_env() -> Option<Self> {
        let path = std::env::var("BEARDL_SHOT").ok()?;
        let due = std::env::var("BEARDL_SHOT_DELAY")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3.0);
        Some(Self {
            path,
            due,
            requested: false,
            page: std::env::var("BEARDL_SHOT_PAGE").ok(),
            url: std::env::var("BEARDL_SHOT_URL").ok(),
            select_first: std::env::var("BEARDL_SHOT_SELECT").is_ok(),
        })
    }

    pub fn tick(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if !self.requested && now >= self.due {
            self.requested = true;
            let path = self.path.clone();
            let cb = egui::ScreenshotCallback::new(move |image| {
                let _ = write_png(&path, image.size[0], image.size[1], image.as_raw());
                std::process::exit(0);
            });
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(cb));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// Uncompressed (stored-deflate) RGBA PNG.
fn write_png(path: &str, w: usize, h: usize, rgba: &[u8]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity((w * 4 + 1) * h);
    for row in rgba.chunks(w * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65_535).enumerate() {
        let last = (i + 1) * 65_535 >= raw.len();
        z.push(last as u8);
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::File::create(path)?.write_all(&out)
}
