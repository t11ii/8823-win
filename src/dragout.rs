//! Dragging files out of 8823 into other apps (Explorer, Finder, editors, browsers, mail…).
//!
//! egui only knows about drags inside its own window, so a drag that should leave the window
//! is handed to the OS:
//! - Windows: a shell data object for the files (the same kind Explorer hands out, so every
//!   app that accepts files from Explorer accepts these) and `SHDoDragDrop`, which also draws
//!   the usual drag image and lets the drop target pick copy / move / link with Ctrl and Shift
//!   exactly as with Explorer. It runs a modal loop until the drop.
//! - macOS: an `NSDraggingSession` with one file URL per item and the Finder icon as the image.
//!
//! The OS keeps the mouse-button release for itself, so egui would think the button is still
//! held after the drop. Each platform posts a synthetic release to our own window afterwards.

use std::path::PathBuf;

/// Start an OS drag of `paths` from the window of `frame`. Returns false when nothing started
/// (unsupported platform or window, or no usable path).
pub fn start(frame: &eframe::Frame, paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return false;
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Ok(handle) = frame.window_handle() else { return false };
        match handle.as_raw() {
            #[cfg(windows)]
            RawWindowHandle::Win32(h) => win::start(h.hwnd.get() as _, paths),
            #[cfg(target_os = "macos")]
            RawWindowHandle::AppKit(h) => mac::start(h.ns_view.as_ptr(), paths),
            _ => false,
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = frame;
        false
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL, HWND, POINT, S_OK};
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
    use windows_sys::Win32::System::Memory::{GHND, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows_sys::Win32::System::Ole::{CF_HDROP, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, OleInitialize};
    use windows_sys::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows_sys::Win32::UI::Shell::{ILCreateFromPathW, ILFindLastID, ILFree, SHCreateDataObject, SHDoDragDrop};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, PostMessageW, WM_LBUTTONUP};
    use windows_sys::core::GUID;

    const IID_IDATAOBJECT: GUID = GUID::from_u128(0x0000010e_0000_0000_c000_000000000046);

    /// STGMEDIUM with the union reduced to the one member used here (all members are pointers).
    #[repr(C)]
    struct StgMedium {
        tymed: u32,
        hglobal: HGLOBAL,
        unk_for_release: *mut c_void,
    }

    /// The start of the IDataObject vtable, up to the last method called here.
    #[repr(C)]
    struct DataObjectVtbl {
        _query_interface: usize,
        _add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        _get_data: usize,
        _get_data_here: usize,
        query_get_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC) -> i32,
        _get_canonical_format_etc: usize,
        set_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *const StgMedium, i32) -> i32,
    }

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// Absolute shell item ids, freed on drop.
    struct Pidls(Vec<*mut ITEMIDLIST>);
    impl Drop for Pidls {
        fn drop(&mut self) {
            for &p in &self.0 {
                unsafe { ILFree(p) };
            }
        }
    }

    pub fn start(hwnd: HWND, paths: &[PathBuf]) -> bool {
        unsafe {
            // winit has already initialised OLE on this thread for drops into the window;
            // this only bumps the count (S_FALSE) and is never undone, like winit's.
            OleInitialize(null());

            let items = Pidls(paths.iter().map(|p| ILCreateFromPathW(wide(p).as_ptr())).collect());
            if items.0.is_empty() || items.0.iter().any(|p| p.is_null()) {
                return false;
            }
            // Usually all items share a folder: describe them relative to it, as Explorer does.
            // Search results can span folders; then use absolute ids under the desktop.
            let parent = paths[0].parent().filter(|par| paths.iter().all(|p| p.parent() == Some(*par)));
            let folder = Pidls(parent.map(|p| ILCreateFromPathW(wide(p).as_ptr())).into_iter().filter(|p| !p.is_null()).collect());
            let (folder_id, children): (*const ITEMIDLIST, Vec<*const ITEMIDLIST>) = match folder.0.first() {
                Some(&f) => (f, items.0.iter().map(|&p| ILFindLastID(p) as *const _).collect()),
                None => (null(), items.0.iter().map(|&p| p as *const _).collect()),
            };

            let mut obj: *mut c_void = null_mut();
            let hr = SHCreateDataObject(folder_id, children.len() as u32, children.as_ptr(), null_mut(), &IID_IDATAOBJECT, &mut obj);
            if hr != S_OK || obj.is_null() {
                return false;
            }
            let vt = &**(obj as *const *const DataObjectVtbl);
            // Apps that only read the classic file list (CF_HDROP) must get it too. The shell
            // object normally provides it; add it ourselves if this one does not.
            let fmt = FORMATETC { cfFormat: CF_HDROP, ptd: null_mut(), dwAspect: DVASPECT_CONTENT, lindex: -1, tymed: TYMED_HGLOBAL as u32 };
            if (vt.query_get_data)(obj, &fmt) != S_OK
                && let Some(h) = hdrop(paths)
            {
                let med = StgMedium { tymed: TYMED_HGLOBAL as u32, hglobal: h, unk_for_release: null_mut() };
                if (vt.set_data)(obj, &fmt, &med, 1) != S_OK {
                    GlobalFree(h);
                }
            }

            let mut effect = 0;
            SHDoDragDrop(hwnd, obj, null_mut(), DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK, &mut effect);
            (vt.release)(obj);

            // The drag loop swallowed the button release; give it back to the window.
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            ScreenToClient(hwnd, &mut pt);
            let lparam = ((pt.y as u16 as u32) << 16 | pt.x as u16 as u32) as isize;
            PostMessageW(hwnd, WM_LBUTTONUP, 0, lparam);
            true
        }
    }

    /// A CF_HDROP block: DROPFILES header, then NUL-separated wide paths, then a final NUL.
    unsafe fn hdrop(paths: &[PathBuf]) -> Option<HGLOBAL> {
        const HEADER: usize = 20; // DROPFILES: pFiles u32, pt POINT, fNC BOOL, fWide BOOL
        let mut list: Vec<u16> = paths.iter().flat_map(|p| wide(p)).collect();
        list.push(0);
        unsafe {
            let h = GlobalAlloc(GHND, HEADER + list.len() * 2);
            if h.is_null() {
                return None;
            }
            let base = GlobalLock(h) as *mut u8;
            if base.is_null() {
                GlobalFree(h);
                return None;
            }
            (base as *mut u32).write_unaligned(HEADER as u32);
            (base.add(16) as *mut i32).write_unaligned(1);
            std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, base.add(HEADER), list.len() * 2);
            GlobalUnlock(h);
            Some(h)
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::path::PathBuf;

    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent,
        NSEventModifierFlags, NSEventType, NSView, NSWorkspace,
    };
    use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString, NSURL};

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "App8823DragSource"]
        #[ivars = isize]
        struct DragSource;

        unsafe impl NSObjectProtocol for DragSource {}

        unsafe impl NSDraggingSource for DragSource {
            #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
            fn operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
                // Same choices as a drag from Finder; the target picks (⌥ copies, ⌘ moves).
                NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Link | NSDragOperation::Generic
            }

            #[unsafe(method(draggingSession:endedAtPoint:operation:))]
            fn ended(&self, _session: &NSDraggingSession, _point: NSPoint, _op: NSDragOperation) {
                // The session swallowed the mouse-up; post one so egui sees the button released.
                let mtm = self.mtm();
                let app = NSApplication::sharedApplication(mtm);
                let Some(window) = app.windowWithWindowNumber(*self.ivars()) else { return };
                let at = window.mouseLocationOutsideOfEventStream();
                let ts = app.currentEvent().map(|e| e.timestamp()).unwrap_or(0.0);
                let up = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                    NSEventType::LeftMouseUp,
                    at,
                    NSEventModifierFlags::empty(),
                    ts,
                    window.windowNumber(),
                    None,
                    0,
                    1,
                    0.0,
                );
                if let Some(up) = up {
                    app.postEvent_atStart(&up, false);
                }
            }
        }
    );

    impl DragSource {
        fn new(window_number: isize, mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(window_number);
            unsafe { msg_send![super(this), init] }
        }
    }

    pub fn start(ns_view: *mut c_void, paths: &[PathBuf]) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        // SAFETY: winit's NSView for our window, alive for the frame.
        let view = unsafe { &*(ns_view as *const NSView) };
        let Some(window) = view.window() else { return false };
        let app = NSApplication::sharedApplication(mtm);
        let at_window = window.mouseLocationOutsideOfEventStream();
        let at = view.convertPoint_fromView(at_window, None);

        let ws = NSWorkspace::sharedWorkspace();
        let size = 48.0;
        let mut items = Vec::with_capacity(paths.len());
        for (i, p) in paths.iter().enumerate() {
            let s = NSString::from_str(&p.to_string_lossy());
            let url = NSURL::fileURLWithPath_isDirectory(&s, p.is_dir());
            let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), &ProtocolObject::from_retained(url));
            let icon = ws.iconForFile(&s);
            let off = (i.min(8) as f64) * 3.0;
            let frame = NSRect::new(NSPoint::new(at.x - size / 2.0 + off, at.y - size / 2.0 - off), NSSize::new(size, size));
            unsafe { item.setDraggingFrame_contents(frame, Some(&*icon)) };
            items.push(item);
        }
        let items = NSArray::from_retained_slice(&items);

        let ts = app.currentEvent().map(|e| e.timestamp()).unwrap_or(0.0);
        let Some(event) = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseDragged,
            at_window,
            NSEventModifierFlags::empty(),
            ts,
            window.windowNumber(),
            None,
            0,
            1,
            1.0,
        ) else {
            return false;
        };
        let source = DragSource::new(window.windowNumber(), mtm);
        view.beginDraggingSessionWithItems_event_source(&items, &event, ProtocolObject::from_ref(&*source));
        true
    }
}
