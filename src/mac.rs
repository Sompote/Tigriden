//! The AppKit bits the file panel needs and neither Slint nor winit exposes:
//! the system pasteboard's *file list* (so ⌘C/⌘V interoperate with Finder) and
//! the pointer location during an external drag — winit's `HoveredFile` /
//! `DroppedFile` events carry a path but no coordinates.

use std::path::PathBuf;

/// A point in the macOS global screen space: points, origin bottom-left.
#[derive(Clone, Copy, Debug, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Point;
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::path::PathBuf;

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        // Declared bare: every call site transmutes it to the exact signature
        // the selector expects, which is the only ABI-correct way to use
        // objc_msgSend (a variadic declaration lies about the calling
        // convention on arm64).
        fn objc_msgSend();
    }

    // NSPasteboard/NSEvent live in AppKit; winit links it anyway, this makes
    // the dependency explicit.
    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    type Send0<R> = unsafe extern "C" fn(*mut c_void, *mut c_void) -> R;
    type Send1<A, R> = unsafe extern "C" fn(*mut c_void, *mut c_void, A) -> R;
    type Send2<A, B, R> = unsafe extern "C" fn(*mut c_void, *mut c_void, A, B) -> R;

    unsafe fn send0<R>(obj: *mut c_void, sel: *mut c_void) -> R {
        let f: Send0<R> = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        f(obj, sel)
    }

    unsafe fn send1<A, R>(obj: *mut c_void, sel: *mut c_void, a: A) -> R {
        let f: Send1<A, R> = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        f(obj, sel, a)
    }

    unsafe fn send2<A, B, R>(obj: *mut c_void, sel: *mut c_void, a: A, b: B) -> R {
        let f: Send2<A, B, R> = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        f(obj, sel, a, b)
    }

    unsafe fn class(name: &CStr) -> *mut c_void {
        objc_getClass(name.as_ptr())
    }

    unsafe fn sel(name: &CStr) -> *mut c_void {
        sel_registerName(name.as_ptr())
    }

    /// `+[NSString stringWithUTF8String:]` — autoreleased, so callers must not
    /// hold it past the current event-loop turn.
    unsafe fn nsstring(text: &str) -> *mut c_void {
        let Ok(cstr) = CString::new(text) else { return std::ptr::null_mut() };
        send1(class(c"NSString"), sel(c"stringWithUTF8String:"), cstr.as_ptr())
    }

    unsafe fn to_string(nsstring: *mut c_void) -> Option<String> {
        if nsstring.is_null() {
            return None;
        }
        let ptr: *const c_char = send0(nsstring, sel(c"UTF8String"));
        if ptr.is_null() {
            return None;
        }
        CStr::from_ptr(ptr).to_str().ok().map(str::to_owned)
    }

    unsafe fn pasteboard() -> *mut c_void {
        send0(class(c"NSPasteboard"), sel(c"generalPasteboard"))
    }

    /// `+[NSEvent modifierFlags]`: the current hardware modifier state,
    /// readable by the app itself without any TCC permission (unlike the
    /// CGEventSource APIs, which are Input-Monitoring-gated on modern macOS).
    pub fn modifier_flags() -> usize {
        unsafe {
            let cls = class(c"NSEvent");
            if cls.is_null() {
                return 0;
            }
            send0(cls, sel(c"modifierFlags"))
        }
    }

    /// `+[NSEvent mouseLocation]`. Two doubles come back in registers on both
    /// arm64 and x86-64, so the plain `objc_msgSend` is correct here.
    pub fn pointer_location() -> Option<Point> {
        unsafe {
            let cls = class(c"NSEvent");
            if cls.is_null() {
                return None;
            }
            Some(send0(cls, sel(c"mouseLocation")))
        }
    }

    /// Puts `paths` on the general pasteboard as file URLs, the representation
    /// Finder pastes as files (rather than as their names).
    pub fn write_files(paths: &[PathBuf]) -> bool {
        if paths.is_empty() {
            return false;
        }
        unsafe {
            let urls: Vec<*mut c_void> = paths
                .iter()
                .filter_map(|p| {
                    let s = nsstring(&p.to_string_lossy());
                    if s.is_null() {
                        return None;
                    }
                    let url: *mut c_void =
                        send1(class(c"NSURL"), sel(c"fileURLWithPath:"), s);
                    (!url.is_null()).then_some(url)
                })
                .collect();
            if urls.is_empty() {
                return false;
            }
            let array: *mut c_void = send2(
                class(c"NSArray"),
                sel(c"arrayWithObjects:count:"),
                urls.as_ptr(),
                urls.len(),
            );
            let pb = pasteboard();
            if pb.is_null() || array.is_null() {
                return false;
            }
            let _: usize = send0(pb, sel(c"clearContents"));
            let ok: bool = send1(pb, sel(c"writeObjects:"), array);
            ok
        }
    }

    /// Reads the file list off the general pasteboard, empty when it holds
    /// something else (plain text, an image, …).
    pub fn read_files() -> Vec<PathBuf> {
        unsafe {
            let pb = pasteboard();
            if pb.is_null() {
                return Vec::new();
            }
            // The legacy plist type is what AppKit synthesises for a Finder
            // copy, and it is one call instead of a loop.
            let kind = nsstring("NSFilenamesPboardType");
            let list: *mut c_void = send1(pb, sel(c"propertyListForType:"), kind);
            if !list.is_null() {
                let count: usize = send0(list, sel(c"count"));
                let paths: Vec<PathBuf> = (0..count)
                    .filter_map(|i| {
                        let item: *mut c_void = send1(list, sel(c"objectAtIndex:"), i);
                        to_string(item).map(PathBuf::from)
                    })
                    .collect();
                if !paths.is_empty() {
                    return paths;
                }
            }
            // Fall back to per-item `public.file-url`, which is what apps that
            // skip the compatibility mapping write.
            let items: *mut c_void = send0(pb, sel(c"pasteboardItems"));
            if items.is_null() {
                return Vec::new();
            }
            let url_type = nsstring("public.file-url");
            let count: usize = send0(items, sel(c"count"));
            (0..count)
                .filter_map(|i| {
                    let item: *mut c_void = send1(items, sel(c"objectAtIndex:"), i);
                    let text: *mut c_void = send1(item, sel(c"stringForType:"), url_type);
                    if text.is_null() {
                        return None;
                    }
                    let url: *mut c_void = send1(class(c"NSURL"), sel(c"URLWithString:"), text);
                    if url.is_null() {
                        return None;
                    }
                    let path: *mut c_void = send0(url, sel(c"path"));
                    to_string(path).map(PathBuf::from)
                })
                .collect()
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Point;
    use std::path::PathBuf;

    pub fn modifier_flags() -> usize {
        0
    }

    pub fn pointer_location() -> Option<Point> {
        None
    }

    pub fn write_files(_paths: &[PathBuf]) -> bool {
        false
    }

    pub fn read_files() -> Vec<PathBuf> {
        Vec::new()
    }
}

pub fn modifier_flags() -> usize {
    imp::modifier_flags()
}

pub fn pointer_location() -> Option<Point> {
    imp::pointer_location()
}

pub fn write_files(paths: &[PathBuf]) -> bool {
    imp::write_files(paths)
}

pub fn read_files() -> Vec<PathBuf> {
    imp::read_files()
}
