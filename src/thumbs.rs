//! Image thumbnails for the gallery view, decoded on a small pool of worker threads.

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

pub const THUMB_PX: u32 = 256;
const MAX_FILE_BYTES: u64 = 80 * 1024 * 1024;
const MAX_CACHED: usize = 1500;

enum Slot {
    Pending,
    Ready(TextureHandle),
    Failed,
}

pub struct Thumbs {
    cache: HashMap<PathBuf, Slot>,
    req_tx: Sender<PathBuf>,
    res_rx: Receiver<(PathBuf, Option<ColorImage>)>,
}

pub fn is_image(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff")
}

impl Thumbs {
    pub fn new(ctx: &egui::Context) -> Self {
        let (req_tx, req_rx) = channel::<PathBuf>();
        let (res_tx, res_rx) = channel();
        let req_rx = Arc::new(Mutex::new(req_rx));
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(2, 6);
        for _ in 0..workers {
            let rx = req_rx.clone();
            let tx = res_tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                loop {
                    let next = rx.lock().ok().and_then(|r| r.recv().ok());
                    let Some(path) = next else { return };
                    let img = decode(&path);
                    if tx.send((path, img)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        }
        Thumbs { cache: HashMap::new(), req_tx, res_rx }
    }

    /// Upload finished decodes as textures. Call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((path, img)) = self.res_rx.try_recv() {
            let slot = match img {
                Some(img) => Slot::Ready(ctx.load_texture(path.to_string_lossy(), img, TextureOptions::LINEAR)),
                None => Slot::Failed,
            };
            self.cache.insert(path, slot);
        }
    }

    /// The thumbnail for `path`, requesting it in the background if it isn't loaded yet.
    pub fn get(&mut self, path: &Path) -> Option<&TextureHandle> {
        if !self.cache.contains_key(path) {
            if self.cache.len() >= MAX_CACHED {
                // Simple bound: drop finished entries; anything on screen is re-requested.
                self.cache.retain(|_, s| matches!(s, Slot::Pending));
            }
            self.cache.insert(path.to_path_buf(), Slot::Pending);
            let _ = self.req_tx.send(path.to_path_buf());
            return None;
        }
        match self.cache.get(path) {
            Some(Slot::Ready(t)) => Some(t),
            _ => None,
        }
    }

    /// Forget cached thumbnails (e.g. after a refresh, files may have changed).
    pub fn invalidate(&mut self) {
        self.cache.retain(|_, s| matches!(s, Slot::Pending));
    }
}

fn decode(path: &Path) -> Option<ColorImage> {
    let len = std::fs::metadata(path).ok()?.len();
    if len > MAX_FILE_BYTES {
        return None;
    }
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let t = img.thumbnail(THUMB_PX, THUMB_PX).to_rgba8();
    let size = [t.width() as usize, t.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, t.as_raw()))
}
