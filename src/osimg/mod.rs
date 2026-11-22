//! Thumbnail decoding. Windows and macOS use the OS image codecs (WIC, ImageIO), which
//! keeps the JPEG, WebP, GIF, TIFF... decoders out of the binary (about 0.9 MB) and lets
//! the OS decode JPEGs at reduced size. Other platforms use the `image` crate.
//!
//! The `rust-decoders` feature also builds the `image` crate path on Windows/macOS, for
//! the speed comparison in the tests (`bench_thumbs`).

#[cfg(target_os = "macos")]
mod imageio;
#[cfg(windows)]
mod wic;

use eframe::egui::ColorImage;
use std::path::Path;

/// A thumbnail at most `max` pixels on the long side (never enlarged).
pub fn thumbnail(path: &Path, max: u32) -> Option<ColorImage> {
    #[cfg(windows)]
    return wic::thumbnail(path, max);
    #[cfg(target_os = "macos")]
    return imageio::thumbnail(path, max);
    #[cfg(not(any(windows, target_os = "macos")))]
    return thumbnail_rust(path, max);
}

/// Decodes with the `image` crate.
#[cfg(any(feature = "rust-decoders", not(any(windows, target_os = "macos"))))]
pub fn thumbnail_rust(path: &Path, max: u32) -> Option<ColorImage> {
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let t = img.thumbnail(max, max).to_rgba8();
    let size = [t.width() as usize, t.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, t.as_raw()))
}

/// Size of a `w`×`h` image scaled to fit in `max`×`max`, keeping the aspect ratio.
#[allow(dead_code)]
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
        println!("{:<22} {:>9} {:>10} {:>10} {:>7}", "file", "size KB", "image ms", "OS ms", "ratio");
        let (mut tr, mut to) = (0.0, 0.0);
        for path in &files {
            let kb = std::fs::metadata(path).unwrap().len() / 1024;
            let name = path.file_name().unwrap().to_string_lossy();
            let r = thumbnail_rust(path, 256).expect("image crate decode");
            let Some(o) = thumbnail(path, 256) else {
                println!("{name:<22} {kb:>9}  OS decoder cannot read this file (codec not installed?)");
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
            let ms_o = time(&|| thumbnail(path, 256));
            tr += ms_r;
            to += ms_o;
            println!("{name:<22} {kb:>9} {ms_r:>10.2} {ms_o:>10.2} {:>6.2}x", ms_r / ms_o);
        }
        println!("{:<22} {:>9} {tr:>10.2} {to:>10.2} {:>6.2}x", "total", "", tr / to);
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
