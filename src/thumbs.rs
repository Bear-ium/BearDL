//! Thumbnails: downscaled copies cached on disk, decoded lazily off the UI thread.
use crate::util::command;
use eframe::egui;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// Width of the cached list thumbnail.
pub const SMALL_WIDTH: u32 = 192;

pub fn small_path(thumb: &Path) -> PathBuf {
    let stem = thumb
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("thumb");
    thumb.with_file_name(format!("{stem}.small.jpg"))
}

/// Writes a small JPEG next to `src` with ffmpeg. Returns its path.
pub fn make_small(ffmpeg: &Path, src: &Path) -> crate::error::Result<PathBuf> {
    let dst = small_path(src);
    let out = command(ffmpeg)
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(src)
        .args(["-vf", &format!("scale={SMALL_WIDTH}:-2"), "-q:v", "5"])
        .arg(&dst)
        .output()?;
    if !out.status.success() || !dst.is_file() {
        crate::bail!(
            "ffmpeg could not scale the thumbnail: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(dst)
}

/// Decodes a JPEG at roughly `max_w` wide, using the decoder's DCT scaling so large
/// images are never fully decoded.
pub fn decode(path: &Path, max_w: u16, max_h: u16) -> Option<egui::ColorImage> {
    let file = std::fs::File::open(path).ok()?;
    let mut dec = jpeg_decoder::Decoder::new(std::io::BufReader::new(file));
    dec.scale(max_w, max_h).ok()?;
    let pixels = dec.decode().ok()?;
    let info = dec.info()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgba: Vec<u8> = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        jpeg_decoder::PixelFormat::L8 => pixels.iter().flat_map(|&l| [l, l, l, 255]).collect(),
        jpeg_decoder::PixelFormat::CMYK32 => pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let k = p[3] as u32;
                let c = |v: u8| (v as u32 * k / 255) as u8;
                [c(p[0]), c(p[1]), c(p[2]), 255]
            })
            .collect(),
        jpeg_decoder::PixelFormat::L16 => pixels
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], 255])
            .collect(),
    };
    if rgba.len() != w * h * 4 {
        return None;
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}

type Key = (PathBuf, u16);

/// Loads and caches thumbnail textures. A worker thread decodes on request; the UI asks
/// every frame and gets `None` until the texture is ready.
pub struct ThumbCache {
    ctx: egui::Context,
    tx: Option<mpsc::Sender<Key>>,
    rx: Option<mpsc::Receiver<(Key, Option<egui::ColorImage>)>>,
    textures: HashMap<Key, Option<egui::TextureHandle>>,
    order: VecDeque<Key>,
    pending: HashSet<Key>,
    capacity: usize,
}

impl ThumbCache {
    pub fn new(ctx: egui::Context, capacity: usize) -> Self {
        Self {
            ctx,
            tx: None,
            rx: None,
            textures: HashMap::new(),
            order: VecDeque::new(),
            pending: HashSet::new(),
            capacity,
        }
    }

    fn worker(&mut self) -> &mpsc::Sender<Key> {
        if self.tx.is_none() {
            let (tx, rx) = mpsc::channel::<Key>();
            let (dtx, drx) = mpsc::channel();
            let ctx = self.ctx.clone();
            let _ = std::thread::Builder::new()
                .name("thumbs".into())
                .spawn(move || {
                    // Blocks on recv while idle: no CPU use.
                    while let Ok(key) = rx.recv() {
                        let (path, w) = &key;
                        let img = decode(path, *w, *w);
                        if dtx.send((key, img)).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                });
            self.tx = Some(tx);
            self.rx = Some(drx);
        }
        self.tx.as_ref().expect("worker started")
    }

    fn receive(&mut self) {
        let Some(rx) = &self.rx else { return };
        let done: Vec<_> = rx.try_iter().collect();
        for (key, img) in done {
            self.pending.remove(&key);
            let tex = img.map(|i| {
                self.ctx
                    .load_texture(key.0.to_string_lossy(), i, egui::TextureOptions::LINEAR)
            });
            self.textures.insert(key.clone(), tex);
            self.order.push_back(key);
            while self.order.len() > self.capacity {
                if let Some(old) = self.order.pop_front() {
                    self.textures.remove(&old);
                }
            }
        }
    }

    /// Texture for `path` decoded at about `width` pixels wide, if ready.
    pub fn get(&mut self, path: &Path, width: u16) -> Option<egui::TextureHandle> {
        if path.as_os_str().is_empty() {
            return None;
        }
        self.receive();
        let key = (path.to_path_buf(), width);
        if let Some(t) = self.textures.get(&key) {
            return t.clone();
        }
        if self.pending.insert(key.clone()) {
            let _ = self.worker().send(key);
        }
        None
    }

    /// Drops cached textures (after a delete or re-download).
    pub fn forget(&mut self, path: &Path) {
        self.textures.retain(|k, _| k.0 != path);
        self.order.retain(|k| k.0 != path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_paths() {
        assert_eq!(
            small_path(Path::new("/t/Youtube-abc.jpg")),
            PathBuf::from("/t/Youtube-abc.small.jpg")
        );
    }

    #[test]
    fn decode_missing_is_none() {
        assert!(decode(Path::new("/no/such.jpg"), 100, 100).is_none());
    }
}
