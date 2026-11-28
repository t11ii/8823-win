//! macOS ImageIO thumbnails (raw FFI to ImageIO, CoreGraphics and CoreFoundation).
//! ImageIO decodes PNG, JPEG, GIF, BMP, ICO, TIFF, WebP and HEIC.

use eframe::egui::ColorImage;
use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

type Ref = *const c_void;

#[derive(Clone, Copy)]
#[repr(C)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[derive(Clone, Copy)]
#[repr(C)]
struct CGSize {
    width: f64,
    height: f64,
}
#[derive(Clone, Copy)]
#[repr(C)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}
#[repr(C)]
struct CGAffineTransform {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
}

const K_CF_NUMBER_SINT32: isize = 3;
const K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;
const K_CG_BITMAP_BYTE_ORDER_32_BIG: u32 = 4 << 12;
const PDF_CROP_BOX: i32 = 1;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: Ref;
    static kCFBooleanFalse: Ref;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFURLCreateFromFileSystemRepresentation(alloc: Ref, buf: *const u8, len: isize, is_dir: u8) -> Ref;
    fn CFNumberCreate(alloc: Ref, kind: isize, value: *const c_void) -> Ref;
    fn CFDictionaryCreate(alloc: Ref, keys: *const Ref, values: *const Ref, n: isize, kcb: *const c_void, vcb: *const c_void) -> Ref;
    fn CFRelease(p: Ref);
}

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    static kCGImageSourceCreateThumbnailFromImageAlways: Ref;
    static kCGImageSourceThumbnailMaxPixelSize: Ref;
    static kCGImageSourceCreateThumbnailWithTransform: Ref;
    static kCGImageSourceShouldCache: Ref;
    fn CGImageSourceCreateWithURL(url: Ref, options: Ref) -> Ref;
    fn CGImageSourceCreateThumbnailAtIndex(src: Ref, index: usize, options: Ref) -> Ref;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGImageGetWidth(img: Ref) -> usize;
    fn CGImageGetHeight(img: Ref) -> usize;
    fn CGColorSpaceCreateDeviceRGB() -> Ref;
    fn CGBitmapContextCreate(data: *mut c_void, w: usize, h: usize, bpc: usize, bpr: usize, space: Ref, info: u32) -> Ref;
    fn CGContextDrawImage(ctx: Ref, rect: CGRect, img: Ref);
    fn CGPDFDocumentCreateWithURL(url: Ref) -> Ref;
    fn CGPDFDocumentIsUnlocked(doc: Ref) -> bool;
    fn CGPDFDocumentGetPage(doc: Ref, page: usize) -> Ref;
    fn CGPDFPageGetBoxRect(page: Ref, box_type: i32) -> CGRect;
    fn CGPDFPageGetRotationAngle(page: Ref) -> i32;
    fn CGPDFPageGetDrawingTransform(page: Ref, box_type: i32, rect: CGRect, rotate: i32, preserve_aspect_ratio: bool) -> CGAffineTransform;
    fn CGContextSetRGBFillColor(ctx: Ref, red: f64, green: f64, blue: f64, alpha: f64);
    fn CGContextFillRect(ctx: Ref, rect: CGRect);
    fn CGContextConcatCTM(ctx: Ref, transform: CGAffineTransform);
    fn CGContextClipToRect(ctx: Ref, rect: CGRect);
    fn CGContextDrawPDFPage(ctx: Ref, page: Ref);
}

/// A CoreFoundation object, released on drop.
struct Cf(Ref);

impl Cf {
    fn new(p: Ref) -> Option<Cf> {
        (!p.is_null()).then_some(Cf(p))
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

fn file_url(path: &Path) -> Option<Cf> {
    let bytes = path.as_os_str().as_bytes();
    unsafe {
        Cf::new(CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize, 0))
    }
}

pub fn thumbnail(path: &Path, max: u32) -> Option<ColorImage> {
    unsafe {
        let url = file_url(path)?;
        let no_cache = [kCGImageSourceShouldCache];
        let no = [kCFBooleanFalse];
        let src_opts = Cf::new(CFDictionaryCreate(
            std::ptr::null(),
            no_cache.as_ptr(),
            no.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        ))?;
        let src = Cf::new(CGImageSourceCreateWithURL(url.0, src_opts.0))?;

        let max_px = max as i32;
        let max_num = Cf::new(CFNumberCreate(std::ptr::null(), K_CF_NUMBER_SINT32, &max_px as *const i32 as *const c_void))?;
        // Always decode the real image (an embedded EXIF thumbnail is too small and
        // blurry), at most `max` pixels on the long side, without EXIF rotation (same as
        // the other platforms).
        let keys = [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceThumbnailMaxPixelSize,
            kCGImageSourceCreateThumbnailWithTransform,
            kCGImageSourceShouldCache,
        ];
        let values = [kCFBooleanTrue, max_num.0, kCFBooleanFalse, kCFBooleanFalse];
        let opts = Cf::new(CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            keys.len() as isize,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        ))?;
        let img = Cf::new(CGImageSourceCreateThumbnailAtIndex(src.0, 0, opts.0))?;

        let (w, h) = (CGImageGetWidth(img.0), CGImageGetHeight(img.0));
        if w == 0 || h == 0 {
            return None;
        }
        // ImageIO never enlarges, but keep the same bound as the other platforms.
        let (w, h) = super::fit(w as u32, h as u32, max);
        bitmap(w, h, |ctx, rect| CGContextDrawImage(ctx, rect, img.0))
    }
}

pub fn pdf_preview(path: &Path, max: u32) -> Option<ColorImage> {
    unsafe {
        let url = file_url(path)?;
        let doc = Cf::new(CGPDFDocumentCreateWithURL(url.0))?;
        if !CGPDFDocumentIsUnlocked(doc.0) {
            return None;
        }
        let page = CGPDFDocumentGetPage(doc.0, 1);
        if page.is_null() {
            return None;
        }
        let bounds = CGPDFPageGetBoxRect(page, PDF_CROP_BOX);
        let (mut width, mut height) = (bounds.size.width, bounds.size.height);
        if CGPDFPageGetRotationAngle(page).rem_euclid(180) != 0 {
            std::mem::swap(&mut width, &mut height);
        }
        let (w, h) = super::pdf_size(width, height, max)?;
        bitmap(w, h, |ctx, rect| {
            CGContextSetRGBFillColor(ctx, 1.0, 1.0, 1.0, 1.0);
            CGContextFillRect(ctx, rect);
            let transform = CGPDFPageGetDrawingTransform(page, PDF_CROP_BOX, rect, 0, true);
            CGContextConcatCTM(ctx, transform);
            CGContextClipToRect(ctx, bounds);
            CGContextDrawPDFPage(ctx, page);
        })
    }
}

fn bitmap(w: u32, h: u32, draw: impl FnOnce(Ref, CGRect)) -> Option<ColorImage> {
    let (w, h) = (w as usize, h as usize);
    unsafe {
        let mut buf = vec![0u8; w * h * 4];
        let space = Cf::new(CGColorSpaceCreateDeviceRGB())?;
        let ctx = Cf::new(CGBitmapContextCreate(
            buf.as_mut_ptr() as *mut c_void,
            w,
            h,
            8,
            w * 4,
            space.0,
            K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST | K_CG_BITMAP_BYTE_ORDER_32_BIG,
        ))?;
        let rect = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: w as f64, height: h as f64 } };
        draw(ctx.0, rect);
        drop(ctx);
        Some(ColorImage::from_rgba_premultiplied([w, h], &buf))
    }
}
