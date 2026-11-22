//! Windows Imaging Component (WIC) thumbnails, through raw COM vtables (windows-sys has
//! no WIC bindings). WIC ships with Windows and decodes PNG, JPEG, GIF, BMP, ICO and TIFF;
//! WebP and HEIC work when their Windows codecs are installed (WebP is built into Windows 11).

use super::fit;
use eframe::egui::ColorImage;
use std::cell::RefCell;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows_sys::core::GUID;

type Hr = i32;
type Ptr = *mut c_void;

const CLSID_WIC_IMAGING_FACTORY: GUID = GUID::from_u128(0xcacaf262_9370_4615_a13b_9f5539da4c0a);
const IID_IWIC_IMAGING_FACTORY: GUID = GUID::from_u128(0xec5ec8a9_c395_4314_9c77_54d7a935ff70);
const GUID_PIXEL_FORMAT_32BPP_RGBA: GUID = GUID::from_u128(0xf5c7ad2d_6a8d_43dd_a7a8_a29935261ae9);
const GUID_CONTAINER_FORMAT_ICO: GUID = GUID::from_u128(0xa3a860c4_338f_4c17_919a_fba4b5628f21);
const GENERIC_READ: u32 = 0x8000_0000;
const WIC_DECODE_METADATA_CACHE_ON_DEMAND: i32 = 0;
const WIC_INTERPOLATION_FANT: i32 = 3;
const WIC_DITHER_NONE: i32 = 0;
const WIC_PALETTE_CUSTOM: i32 = 0;

// Vtable slots (IUnknown takes 0..=2).
mod slot {
    pub const RELEASE: usize = 2;
    // IWICImagingFactory
    pub const CREATE_DECODER_FROM_FILENAME: usize = 3;
    pub const CREATE_FORMAT_CONVERTER: usize = 10;
    pub const CREATE_BITMAP_SCALER: usize = 11;
    // IWICBitmapDecoder
    pub const GET_CONTAINER_FORMAT: usize = 5;
    pub const GET_FRAME_COUNT: usize = 12;
    pub const GET_FRAME: usize = 13;
    // IWICBitmapSource (frame, scaler, converter)
    pub const GET_SIZE: usize = 3;
    pub const COPY_PIXELS: usize = 7;
    // IWICBitmapScaler / IWICFormatConverter
    pub const INITIALIZE: usize = 8;
}

/// An owned COM pointer, released on drop.
struct Com(Ptr);

impl Com {
    /// The function in vtable slot `i`, cast to the signature `F`.
    unsafe fn f<F: Copy>(&self, i: usize) -> F {
        unsafe {
            let vtbl = *(self.0 as *const *const *const c_void);
            std::mem::transmute_copy(&*vtbl.add(i))
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { self.f::<unsafe extern "system" fn(Ptr) -> u32>(slot::RELEASE)(self.0) };
        }
    }
}

fn ok(hr: Hr) -> Option<()> {
    (hr >= 0).then_some(())
}

thread_local! {
    /// One factory per worker thread (COM is initialised once per thread, multithreaded).
    static FACTORY: RefCell<Option<Com>> = const { RefCell::new(None) };
}

fn with_factory<R>(f: impl FnOnce(&Com) -> Option<R>) -> Option<R> {
    FACTORY.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            unsafe {
                CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32);
                let mut p: Ptr = null_mut();
                ok(CoCreateInstance(&CLSID_WIC_IMAGING_FACTORY, null_mut(), CLSCTX_INPROC_SERVER, &IID_IWIC_IMAGING_FACTORY, &mut p))?;
                *cell = Some(Com(p));
            }
        }
        f(cell.as_ref()?)
    })
}

pub fn thumbnail(path: &Path, max: u32) -> Option<ColorImage> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    with_factory(|factory| unsafe {
        let mut p: Ptr = null_mut();
        ok(factory.f::<unsafe extern "system" fn(Ptr, *const u16, *const GUID, u32, i32, *mut Ptr) -> Hr>(slot::CREATE_DECODER_FROM_FILENAME)(
            factory.0,
            wide.as_ptr(),
            std::ptr::null(),
            GENERIC_READ,
            WIC_DECODE_METADATA_CACHE_ON_DEMAND,
            &mut p,
        ))?;
        let decoder = Com(p);

        let mut frames = 0u32;
        ok(decoder.f::<unsafe extern "system" fn(Ptr, *mut u32) -> Hr>(slot::GET_FRAME_COUNT)(decoder.0, &mut frames))?;
        if frames == 0 {
            return None;
        }
        let get_frame = |i: u32| -> Option<Com> {
            let mut p: Ptr = null_mut();
            ok(decoder.f::<unsafe extern "system" fn(Ptr, u32, *mut Ptr) -> Hr>(slot::GET_FRAME)(decoder.0, i, &mut p))?;
            Some(Com(p))
        };
        let size = |src: &Com| -> Option<(u32, u32)> {
            let (mut w, mut h) = (0u32, 0u32);
            ok(src.f::<unsafe extern "system" fn(Ptr, *mut u32, *mut u32) -> Hr>(slot::GET_SIZE)(src.0, &mut w, &mut h))?;
            Some((w, h))
        };

        // Icons hold several sizes; use the largest (as the old decoder did). Everything
        // else uses the first frame (the first page of a TIFF, the first frame of a GIF).
        let mut container = GUID::from_u128(0);
        decoder.f::<unsafe extern "system" fn(Ptr, *mut GUID) -> Hr>(slot::GET_CONTAINER_FORMAT)(decoder.0, &mut container);
        let mut frame = get_frame(0)?;
        let (mut w, mut h) = size(&frame)?;
        if frames > 1 && guid_eq(&container, &GUID_CONTAINER_FORMAT_ICO) {
            for i in 1..frames.min(32) {
                if let Some(f) = get_frame(i) {
                    if let Some((fw, fh)) = size(&f) {
                        if fw * fh > w * h {
                            (frame, w, h) = (f, fw, fh);
                        }
                    }
                }
            }
        }
        if w == 0 || h == 0 {
            return None;
        }

        let (tw, th) = fit(w, h, max);
        let mut source = frame;
        if (tw, th) != (w, h) {
            let mut p: Ptr = null_mut();
            ok(factory.f::<unsafe extern "system" fn(Ptr, *mut Ptr) -> Hr>(slot::CREATE_BITMAP_SCALER)(factory.0, &mut p))?;
            let scaler = Com(p);
            ok(scaler.f::<unsafe extern "system" fn(Ptr, Ptr, u32, u32, i32) -> Hr>(slot::INITIALIZE)(
                scaler.0,
                source.0,
                tw,
                th,
                WIC_INTERPOLATION_FANT,
            ))?;
            source = scaler;
        }

        let mut p: Ptr = null_mut();
        ok(factory.f::<unsafe extern "system" fn(Ptr, *mut Ptr) -> Hr>(slot::CREATE_FORMAT_CONVERTER)(factory.0, &mut p))?;
        let converter = Com(p);
        ok(converter.f::<unsafe extern "system" fn(Ptr, Ptr, *const GUID, i32, Ptr, f64, i32) -> Hr>(slot::INITIALIZE)(
            converter.0,
            source.0,
            &GUID_PIXEL_FORMAT_32BPP_RGBA,
            WIC_DITHER_NONE,
            null_mut(),
            0.0,
            WIC_PALETTE_CUSTOM,
        ))?;

        let stride = tw * 4;
        let mut buf = vec![0u8; (stride * th) as usize];
        ok(converter.f::<unsafe extern "system" fn(Ptr, *const c_void, u32, u32, *mut u8) -> Hr>(slot::COPY_PIXELS)(
            converter.0,
            std::ptr::null(),
            stride,
            buf.len() as u32,
            buf.as_mut_ptr(),
        ))?;
        Some(ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], &buf))
    })
}

fn guid_eq(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}
