//! Thumbnail decoding: each format goes to whichever decoder is faster on that OS
//! (measured with `bench_thumbs` on the GitHub Windows and macOS runners, 2026-11-22).
//!
//! - JPEG and BMP: the OS codecs (WIC on Windows, ImageIO on macOS), 2-5x faster than the
//!   `image` crate because they decode JPEGs at reduced size. GIF and ICO too on Windows.
//! - PNG, WebP, TIFF (and GIF on macOS): the `image` crate, which is as fast or faster
//!   there. Its PNG decoder is in the binary anyway for the window icon.
//!
//! Leaving the JPEG, GIF, BMP and ICO decoders out of the `image` crate on Windows and
//! macOS keeps the binary smaller. If the preferred decoder can't read a file (say a
//! JPEG named .png), the other one gets a try. Other platforms use only the `image` crate.
//!
//! The `rust-decoders` feature builds every `image` crate decoder, for `bench_thumbs`.

#[cfg(target_os = "macos")]
mod imageio;
#[cfg(windows)]
mod pdf_win;
#[cfg(windows)]
mod wic;

use eframe::egui::ColorImage;
use std::path::Path;

/// Render only the first PDF page, with a bounded output size.
pub fn pdf_preview(path: &Path, max: u32) -> Option<ColorImage> {
    #[cfg(target_os = "macos")]
    return imageio::pdf_preview(path, max);
    #[cfg(windows)]
    return pdf_win::render(path, max);
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (path, max);
        None
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn pdf_size(width: f64, height: f64, max: u32) -> Option<(u32, u32)> {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 && max > 0) {
        return None;
    }
    let long_edge = width.max(height);
    let dimension = |n: f64| ((n / long_edge * f64::from(max)).round() as u32).max(1);
    Some((dimension(width), dimension(height)))
}

/// A thumbnail at most `max` pixels on the long side (never enlarged).
pub fn thumbnail(path: &Path, max: u32) -> Option<ColorImage> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if rust_first(&ext) {
        thumbnail_rust(path, max).or_else(|| thumbnail_os(path, max))
    } else {
        thumbnail_os(path, max).or_else(|| thumbnail_rust(path, max))
    }
}

pub fn preview_image(path: &Path, max: u32) -> Option<ColorImage> {
    // Try downsampling with the platform codec before decoding a full-size image in Rust.
    #[cfg(windows)]
    let raster = wic::preview(path, max);
    #[cfg(not(windows))]
    let raster = thumbnail_os(path, max);
    raster.or_else(|| {
        let mut reader = image_reader(path)?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let img = reader.decode().ok()?;
        let (w, h) = fit(img.width(), img.height(), max);
        let img = if (w, h) == (img.width(), img.height()) { img } else { img.thumbnail_exact(w, h) };
        Some(color_image(img.into_rgba8()))
    })
}

/// Formats the `image` crate decodes faster than the OS (see the module docs).
fn rust_first(ext: &str) -> bool {
    if cfg!(windows) {
        matches!(ext, "png" | "webp" | "tif" | "tiff")
    } else if cfg!(target_os = "macos") {
        matches!(ext, "png" | "webp" | "tif" | "tiff" | "gif")
    } else {
        true
    }
}

#[cfg(windows)]
fn thumbnail_os(path: &Path, max: u32) -> Option<ColorImage> {
    wic::thumbnail(path, max)
}
#[cfg(target_os = "macos")]
fn thumbnail_os(path: &Path, max: u32) -> Option<ColorImage> {
    imageio::thumbnail(path, max)
}
#[cfg(not(any(windows, target_os = "macos")))]
fn thumbnail_os(_path: &Path, _max: u32) -> Option<ColorImage> {
    None
}

/// Decodes with the `image` crate (only the formats built into it on this platform).
fn thumbnail_rust(path: &Path, max: u32) -> Option<ColorImage> {
    let img = image_reader(path)?.decode().ok()?;
    Some(color_image(img.thumbnail(max, max).into_rgba8()))
}

fn image_reader(path: &Path) -> Option<image::ImageReader<std::io::BufReader<std::fs::File>>> {
    image::ImageReader::open(path).ok()?.with_guessed_format().ok()
}

fn color_image(image: image::RgbaImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw())
}

/// Size of a `w`×`h` image scaled to fit in `max`×`max`, keeping the aspect ratio.
fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max && h <= max {
        return (w, h);
    }
    let s = max as f64 / w.max(h) as f64;
    (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect_and_never_enlarges() {
        assert_eq!(fit(100, 50, 256), (100, 50));
        assert_eq!(fit(4000, 3000, 256), (256, 192));
        assert_eq!(fit(3000, 4000, 256), (192, 256));
        assert_eq!(fit(10000, 1, 256), (256, 1));
    }

    /// The OS decoder against the image crate, on generated files. Run with
    /// `cargo test --release --features rust-decoders -- --ignored bench_thumbs --nocapture`.
    #[cfg(feature = "rust-decoders")]
    #[test]
    #[ignore]
    fn bench_thumbs() {
        use std::time::Instant;
        let dir = std::env::temp_dir().join("8823-bench-thumbs");
        std::fs::create_dir_all(&dir).unwrap();
        let files = sample_files(&dir);
        println!("{:<24} {:>8} {:>9} {:>9} {:>9}", "file", "size KB", "image ms", "OS ms", "8823 ms");
        let (mut tr, mut to, mut tc) = (0.0, 0.0, 0.0);
        for path in &files {
            let kb = std::fs::metadata(path).unwrap().len() / 1024;
            let name = path.file_name().unwrap().to_string_lossy();
            let r = thumbnail_rust(path, 256).expect("image crate decode");
            let Some(o) = thumbnail_os(path, 256) else {
                println!("{name:<24} {kb:>8}  OS decoder cannot read this file (codec not installed?)");
                continue;
            };
            assert_eq!(r.size, o.size, "{}", path.display());
            let diff = mean(&r) - mean(&o);
            assert!(diff.abs() < 8.0, "{}: mean colour differs by {diff}", path.display());
            let time = |f: &dyn Fn() -> Option<ColorImage>| {
                let mut v: Vec<f64> = (0..9)
                    .map(|_| {
                        let t = Instant::now();
                        std::hint::black_box(f());
                        t.elapsed().as_secs_f64() * 1000.0
                    })
                    .collect();
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                v[4]
            };
            let ms_r = time(&|| thumbnail_rust(path, 256));
            let ms_o = time(&|| thumbnail_os(path, 256));
            let ms_c = time(&|| thumbnail(path, 256));
            tr += ms_r;
            to += ms_o;
            tc += ms_c;
            println!("{name:<24} {kb:>8} {ms_r:>9.2} {ms_o:>9.2} {ms_c:>9.2}");
        }
        println!("{:<24} {:>8} {tr:>9.2} {to:>9.2} {tc:>9.2}", "total", "");
    }

    #[cfg(feature = "rust-decoders")]
    fn mean(img: &ColorImage) -> f64 {
        let s: u64 = img.pixels.iter().map(|c| c.r() as u64 + c.g() as u64 + c.b() as u64).sum();
        s as f64 / (img.pixels.len() as f64 * 3.0)
    }

    /// Photo-like (smooth gradients with fine noise) and screenshot-like test images.
    #[cfg(feature = "rust-decoders")]
    fn sample_files(dir: &Path) -> Vec<std::path::PathBuf> {
        use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
        let noise = |x: u32, y: u32, c: u32| {
            let mut n = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77) ^ c.wrapping_mul(0xC2B2_AE3D);
            n ^= n >> 15;
            n = n.wrapping_mul(0x2C1B_3C6D);
            n ^= n >> 12;
            (n % 17) as i32 - 8
        };
        let photo = |w: u32, h: u32| {
            RgbImage::from_fn(w, h, |x, y| {
                let fx = x as f32 / w as f32;
                let fy = y as f32 / h as f32;
                let v = |c: u32, base: f32| ((base * 255.0) as i32 + noise(x, y, c)).clamp(0, 255) as u8;
                Rgb([
                    v(0, 0.5 + 0.4 * (fx * 7.0).sin() * fy),
                    v(1, 0.3 + 0.5 * fy * (1.0 - fx)),
                    v(2, 0.6 + 0.3 * ((fx + fy) * 11.0).cos()),
                ])
            })
        };
        let shot = |w: u32, h: u32| {
            RgbaImage::from_fn(w, h, |x, y| {
                let band = (y / 24) % 2 == 0;
                let glyph = (x / 3 + y / 2) % 7 < 2 && x % 400 < 300 && y % 24 > 6 && y % 24 < 18;
                if glyph {
                    Rgba([30, 30, 35, 255])
                } else if band {
                    Rgba([250, 250, 252, 255])
                } else {
                    Rgba([236, 240, 246, 255])
                }
            })
        };
        let mut out = Vec::new();
        let mut save = |name: &str, f: &dyn Fn(&Path)| {
            let p = dir.join(name);
            if !p.exists() {
                f(&p);
            }
            out.push(p);
        };
        let big = photo(4000, 3000);
        let mid = photo(1920, 1080);
        save("photo-12mp.jpg", &|p| big.save_with_format(p, ImageFormat::Jpeg).unwrap());
        save("photo-2mp.jpg", &|p| mid.save_with_format(p, ImageFormat::Jpeg).unwrap());
        save("screenshot-4mp.png", &|p| shot(2560, 1440).save_with_format(p, ImageFormat::Png).unwrap());
        save("photo-2mp.png", &|p| mid.save_with_format(p, ImageFormat::Png).unwrap());
        save("photo-2mp-lossless.webp", &|p| mid.save_with_format(p, ImageFormat::WebP).unwrap());
        save("photo-1mp.gif", &|p| photo(1200, 800).save_with_format(p, ImageFormat::Gif).unwrap());
        save("photo-3mp.tif", &|p| photo(2000, 1500).save_with_format(p, ImageFormat::Tiff).unwrap());
        save("photo-2mp.bmp", &|p| mid.save_with_format(p, ImageFormat::Bmp).unwrap());
        out
    }
}
