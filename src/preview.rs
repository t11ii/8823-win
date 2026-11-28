//! On-demand previews. File reads and decoding happen on a worker thread.

use crate::fsx::{Bg, Msg};
use eframe::egui::{self, ColorImage, Key, Modifiers, TextureHandle};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

const TEXT_BYTES: usize = 64 * 1024;
const RASTER_EDGE: u32 = 1280;
const MEDIA_BYTES: u64 = 80 * 1024 * 1024;

pub enum Content<T> {
    Raster { image: T, pdf: bool },
    Text { text: String, truncated: bool },
}

pub type LoadResult = Result<Content<ColorImage>, String>;

enum State {
    Waiting,
    Loading,
    Ready(Result<Content<TextureHandle>, String>),
}

pub struct Preview {
    pub req: u64,
    path: PathBuf,
    name: String,
    state: State,
    reset_scroll: bool,
}

impl Preview {
    pub fn new(req: u64, path: PathBuf) -> Self {
        let name = crate::text::display_name(path.file_name().unwrap_or(path.as_os_str()));
        Self {
            req,
            path,
            name,
            state: State::Waiting,
            reset_scroll: true,
        }
    }

    /// The caller starts a waiting request only when the previous worker has finished.
    pub fn start(&mut self, bg: Bg) -> bool {
        if !matches!(self.state, State::Waiting) {
            return false;
        }
        let req = self.req;
        let path = self.path.clone();
        match std::thread::Builder::new()
            .name("preview".into())
            .spawn(move || {
                bg.send(Msg::PreviewReady {
                    req,
                    result: load(&path),
                });
            }) {
            Ok(_) => {
                self.state = State::Loading;
                true
            }
            Err(err) => {
                self.state = State::Ready(Err(format!("Could not start preview. {err}")));
                false
            }
        }
    }

    pub fn finish(&mut self, ctx: &egui::Context, result: LoadResult) {
        let result = result.map(|content| match content {
            Content::Raster { image, pdf } => Content::Raster {
                image: ctx.load_texture("preview", image, egui::TextureOptions::LINEAR),
                pdf,
            },
            Content::Text { text, truncated } => Content::Text { text, truncated },
        });
        self.state = State::Ready(result);
    }

    /// Returns true if the Close button was clicked.
    pub fn show(&mut self, ui: &mut egui::Ui) -> bool {
        let screen = ui.ctx().content_rect().size();
        let width = (screen.x - 48.0).clamp(1.0, 820.0);
        let height = (screen.y - 112.0).clamp(1.0, 650.0);
        ui.set_width(width);
        let (_, close) = egui::Sides::new().shrink_left().show(
            ui,
            |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&self.name).strong()).truncate())
                    .on_hover_text(self.path.display().to_string());
            },
            |ui| ui.button("Close").clicked(),
        );
        ui.separator();
        match &self.state {
            State::Waiting | State::Loading => {
                ui.label("Loading preview…");
            }
            State::Ready(Err(message)) => {
                ui.label(message);
            }
            State::Ready(Ok(Content::Raster { image, pdf })) => {
                if *pdf {
                    ui.weak("Page 1");
                }
                ui.vertical_centered(|ui| {
                    ui.add(
                        egui::Image::new(egui::load::SizedTexture::from_handle(image))
                            .max_size(egui::vec2(width, height)),
                    );
                });
            }
            State::Ready(Ok(Content::Text { text, truncated })) => {
                if *truncated {
                    ui.weak("Showing the first 64 KiB");
                }
                if text.is_empty() {
                    ui.weak("Empty file");
                } else {
                    let mut scroll = egui::ScrollArea::vertical()
                        .id_salt("preview_text")
                        .max_height(height)
                        .auto_shrink([false, true]);
                    if std::mem::take(&mut self.reset_scroll) {
                        scroll = scroll.vertical_scroll_offset(0.0);
                    }
                    scroll.show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(text).monospace())
                                .wrap()
                                .selectable(true),
                        );
                    });
                }
            }
        }
        close
    }
}

pub fn shortcut_modifiers(modifiers: Modifiers) -> bool {
    modifiers.matches_exact(Modifiers::CTRL)
        || (cfg!(target_os = "macos") && modifiers.matches_exact(Modifiers::MAC_CMD))
}

pub fn consume_shortcut(input: &mut egui::InputState) -> bool {
    if !input.key_pressed(Key::I) {
        return false;
    }
    let mut pressed = false;
    input.events.retain(|e| {
        if let egui::Event::Key {
            key: Key::I,
            pressed: true,
            repeat,
            modifiers,
            ..
        } = e
            && shortcut_modifiers(*modifiers)
        {
            pressed |= !repeat;
            false
        } else {
            true
        }
    });
    pressed
}

fn load(path: &Path) -> LoadResult {
    let metadata = path
        .metadata()
        .map_err(|err| format!("Could not read file. {err}"))?;
    if !metadata.is_file() {
        return Err("Preview is available for regular files.".into());
    }
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let pdf = ext == "pdf";
    if pdf || crate::thumbs::is_image(&ext) {
        if metadata.len() > MEDIA_BYTES {
            return Err("Preview is limited to images and PDFs up to 80 MiB.".into());
        }
        let image = if pdf {
            crate::osimg::pdf_preview(path, RASTER_EDGE).ok_or("Could not preview this PDF.")?
        } else {
            crate::osimg::preview_image(path, RASTER_EDGE).ok_or("Could not preview this image.")?
        };
        return Ok(Content::Raster { image, pdf });
    }
    read_text(path)
        .map_err(|err| format!("Could not read file. {err}"))?
        .ok_or_else(|| "Preview supports images, PDFs, and UTF-8 or UTF-16 text.".into())
}

fn read_text(path: &Path) -> std::io::Result<Option<Content<ColorImage>>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((TEXT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > TEXT_BYTES;
    bytes.truncate(TEXT_BYTES);
    Ok(decode_text(&bytes, truncated).map(|text| Content::Text { text, truncated }))
}

fn decode_text(bytes: &[u8], truncated: bool) -> Option<String> {
    let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let little_endian = bytes[0] == 0xff;
        let units = bytes[2..].chunks_exact(2);
        if !truncated && !units.remainder().is_empty() {
            return None;
        }
        let mut units: Vec<u16> = units
            .map(|b| {
                let pair = [b[0], b[1]];
                if little_endian {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                }
            })
            .collect();
        if truncated && units.last().is_some_and(|u| (0xd800..=0xdbff).contains(u)) {
            units.pop();
        }
        String::from_utf16(&units).ok()?
    } else {
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        match std::str::from_utf8(bytes) {
            Ok(text) => text.to_string(),
            Err(err) if truncated && err.error_len().is_none() => {
                std::str::from_utf8(&bytes[..err.valid_up_to()])
                    .ok()?
                    .to_string()
            }
            Err(_) => return None,
        }
    };
    if text
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t' | '\u{000c}'))
    {
        return None;
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(extension: &str, bytes: &[u8]) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "8823Preview{}-{nonce}.{extension}",
                std::process::id()
            ));
            File::options()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap()
                .write_all(bytes)
                .unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn reads_only_the_prefix_and_keeps_whole_characters() {
        let mut bytes = vec![b'a'; TEXT_BYTES - 1];
        bytes.extend("日本語".as_bytes());
        let file = Fixture::new("txt", &bytes);
        let Content::Text { text, truncated } = load(&file.0).unwrap() else {
            panic!("expected text");
        };
        assert!(truncated);
        assert_eq!(text.len(), TEXT_BYTES - 1);
        assert_eq!(
            decode_text(&[0xff, 0xfe, 0xe5, 0x65], false).as_deref(),
            Some("日")
        );
        assert!(decode_text(b"hello\0binary", false).is_none());
    }

    #[test]
    fn image_preview_is_bounded() {
        let image = image::RgbImage::from_pixel(1600, 800, image::Rgb([40, 60, 80]));
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let file = Fixture::new("PNG", &png.into_inner());
        let Content::Raster { image, pdf: false } = load(&file.0).unwrap() else {
            panic!("expected an image");
        };
        assert_eq!(image.size, [RASTER_EDGE as usize, RASTER_EDGE as usize / 2]);
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn pdf_preview_renders_the_first_page_with_its_rotation() {
        let stream =
            |draw: &str| format!("<< /Length {} >>\nstream\n{draw}\nendstream", draw.len());
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R] >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 200] /Rotate 90 /Resources << >> /Contents 5 0 R >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 200] /Resources << >> /Contents 6 0 R >>".into(),
            stream("1 0 0 rg 0 0 100 200 re f"),
            stream("0 0 1 rg 0 0 100 200 re f"),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            writeln!(&mut pdf, "{} 0 obj\n{object}\nendobj", i + 1).unwrap();
        }
        let xref = pdf.len();
        let count = objects.len() + 1;
        write!(&mut pdf, "xref\n0 {count}\n0000000000 65535 f \n").unwrap();
        for offset in offsets {
            writeln!(&mut pdf, "{offset:010} 00000 n ").unwrap();
        }
        writeln!(
            &mut pdf,
            "trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF"
        )
        .unwrap();

        let file = Fixture::new("pdf", &pdf);
        // Exercise repeated previews from separate worker threads.
        for _ in 0..2 {
            let result = std::thread::scope(|s| s.spawn(|| load(&file.0)).join().unwrap());
            let Content::Raster { image, pdf: true } = result.unwrap() else {
                panic!("expected a PDF page");
            };
            let [w, h] = image.size;
            assert_eq!([w, h], [RASTER_EDGE as usize, RASTER_EDGE as usize / 2]);
            let center = image.pixels[h / 2 * w + w / 2];
            assert!(
                center.r() > 200 && center.g() < 50 && center.b() < 50,
                "center was {center:?}"
            );
        }
    }
}
