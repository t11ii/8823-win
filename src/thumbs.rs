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
    workers: Option<Workers>,
}

struct Workers {
    requests: Sender<PathBuf>,
    results: Receiver<(PathBuf, Option<ColorImage>)>,
}

pub fn is_image(ext: &str) -> bool {
    // HEIC comes free with the OS decoders (on Windows when the HEIF codec is installed).
    matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff")
        || (cfg!(any(windows, target_os = "macos")) && matches!(ext, "heic" | "heif"))
}

impl Thumbs {
    pub fn new() -> Self {
        Self { cache: HashMap::new(), workers: None }
    }

    fn spawn_workers(ctx: &egui::Context) -> Workers {
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
        Workers { requests: req_tx, results: res_rx }
    }

    /// Upload finished decodes as textures. Call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let Some(workers) = &self.workers else { return };
        while let Ok((path, img)) = workers.results.try_recv() {
            let slot = match img {
                Some(img) => Slot::Ready(ctx.load_texture(path.to_string_lossy(), img, TextureOptions::LINEAR)),
                None => Slot::Failed,
            };
            self.cache.insert(path, slot);
        }
    }

    /// The thumbnail for `path`, requesting it in the background if it isn't loaded yet.
    pub fn get(&mut self, ctx: &egui::Context, path: &Path) -> Option<&TextureHandle> {
        if !self.cache.contains_key(path) {
            if self.cache.len() >= MAX_CACHED {
                // Simple bound: drop finished entries; anything on screen is re-requested.
                self.cache.retain(|_, s| matches!(s, Slot::Pending));
            }
            self.cache.insert(path.to_path_buf(), Slot::Pending);
            let workers = self.workers.get_or_insert_with(|| Self::spawn_workers(ctx));
            let _ = workers.requests.send(path.to_path_buf());
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
    crate::osimg::thumbnail(path, THUMB_PX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_workers_start_on_the_first_request() {
        let ctx = egui::Context::default();
        let mut thumbs = Thumbs::new();
        thumbs.poll(&ctx);
        assert!(thumbs.workers.is_none());
        let path = Path::new("missingThumbnail.png");
        assert!(thumbs.get(&ctx, path).is_none());
        let workers = thumbs.workers.as_ref().unwrap();
        let (decoded, _) = workers.results.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(decoded, path);
    }
}
