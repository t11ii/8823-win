//! First-page rendering through the Windows PDF API.

use eframe::egui::ColorImage;
use std::path::Path;
use windows::Data::Pdf::{PdfDocument, PdfPage, PdfPageRenderOptions};
use windows::Graphics::Imaging::BitmapEncoder;
use windows::Storage::FileAccessMode;
use windows::Storage::Streams::{DataReader, IRandomAccessStream, InMemoryRandomAccessStream};
use windows::Win32::System::WinRT::{
    CreateRandomAccessStreamOnFile, RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
};
use windows::core::{HSTRING, PCWSTR};

struct WinRt;

impl Drop for WinRt {
    fn drop(&mut self) {
        unsafe { RoUninitialize() }
    }
}

pub fn render(path: &Path, max: u32) -> Option<ColorImage> {
    // The caller runs on a dedicated worker. Join WinRT operations in its MTA.
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.ok()?;
    let _runtime = WinRt;
    let path = std::path::absolute(path).ok()?;
    let path = HSTRING::from(path.as_os_str());
    // A read-only file stream also works for files with the Windows Hidden attribute.
    let input: IRandomAccessStream = unsafe {
        CreateRandomAccessStreamOnFile(PCWSTR(path.as_ptr()), FileAccessMode::Read.0 as u32)
    }
    .ok()?;
    let document = PdfDocument::LoadFromStreamAsync(&input).ok()?.join().ok()?;
    let page = document.GetPage(0).ok()?;
    let rendered = render_page(&page, max);
    let _ = page.Close();
    rendered
}

fn render_page(page: &PdfPage, max: u32) -> Option<ColorImage> {
    let size = page.Size().ok()?;
    let (width, height) = super::pdf_size(size.Width.into(), size.Height.into(), max)?;

    let options = PdfPageRenderOptions::new().ok()?;
    options.SetDestinationWidth(width).ok()?;
    options.SetDestinationHeight(height).ok()?;
    options.SetIsIgnoringHighContrast(true).ok()?;
    options
        .SetBitmapEncoderId(BitmapEncoder::PngEncoderId().ok()?)
        .ok()?;

    let stream = InMemoryRandomAccessStream::new().ok()?;
    page.RenderWithOptionsToStreamAsync(&stream, &options)
        .ok()?
        .join()
        .ok()?;
    let byte_count = u32::try_from(stream.Size().ok()?).ok()?;
    // Allow overhead for PNG framing in addition to a full RGBA image.
    if byte_count == 0 || u64::from(byte_count) > u64::from(max).pow(2) * 5 + 1024 * 1024 {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).ok()?).ok()?;
    if reader.LoadAsync(byte_count).ok()?.join().ok()? != byte_count {
        return None;
    }
    let mut png = vec![0; byte_count as usize];
    reader.ReadBytes(&mut png).ok()?;
    let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let (width, height) = image.dimensions();
    if width > max || height > max {
        return None;
    }
    Some(ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        image.as_raw(),
    ))
}
