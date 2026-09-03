//! A live WebKit view, shown over the viewer pane for HTML files.
//!
//! The typeset reader in `src/viewer.rs` is a document reader: it applies no
//! stylesheet and runs no script, which is right for an agent's HTML report
//! and wrong for a site whose content the page builds at load time. WebKit is
//! already in the OS, so a real page gets a real browser view — an `NSView`
//! parked over the pane and moved with it, loading the file straight off disk.

use std::path::Path;

/// A WKWebView parented to the window's content view. Dropping it takes the
/// view out of the hierarchy.
pub struct WebView(imp::View);

impl WebView {
    /// Creates the view as a subview of `parent` (the window's content view,
    /// from the raw window handle). None if WebKit is unavailable.
    ///
    /// # Safety
    /// `parent` must be a live `NSView`.
    pub unsafe fn new(parent: *mut std::ffi::c_void) -> Option<Self> {
        imp::View::new(parent).map(WebView)
    }

    /// Loads a local file, granting the page read access to the folder it
    /// sits in so its own stylesheets, scripts and images resolve.
    pub fn load(&self, path: &Path) -> bool {
        self.0.load(path)
    }

    /// Places the view in the pane, in Slint's top-left window coordinates.
    /// `parent_h` is the window's content height in the same units.
    pub fn set_frame(&self, parent_h: f32, x: f32, y: f32, w: f32, h: f32) {
        self.0.set_frame(parent_h as f64, x as f64, y as f64, w as f64, h as f64);
    }

    pub fn set_hidden(&self, hidden: bool) {
        self.0.set_hidden(hidden);
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::path::Path;

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        // Bare, like `crate::mac`: each call site transmutes it to the exact
        // signature the selector expects, which is the only ABI-correct way
        // to call objc_msgSend on arm64.
        fn objc_msgSend();
    }

    #[link(name = "WebKit", kind = "framework")]
    extern "C" {}

    /// `NSRect`: four doubles, which arm64 passes in registers, so it can go
    /// through the ordinary `objc_msgSend` as an argument.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

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

    unsafe fn nsstring(text: &str) -> *mut c_void {
        let Ok(cstr) = CString::new(text) else { return std::ptr::null_mut() };
        send1(class(c"NSString"), sel(c"stringWithUTF8String:"), cstr.as_ptr())
    }

    /// `+[NSURL fileURLWithPath:isDirectory:]`, which needs no round trip to
    /// the file system to decide what it was handed.
    unsafe fn file_url(path: &Path, directory: bool) -> *mut c_void {
        let s = nsstring(&path.to_string_lossy());
        if s.is_null() {
            return std::ptr::null_mut();
        }
        send2(class(c"NSURL"), sel(c"fileURLWithPath:isDirectory:"), s, directory)
    }

    pub struct View {
        /// The WKWebView, retained for as long as this struct lives.
        obj: *mut c_void,
    }

    impl View {
        pub fn new(parent: *mut c_void) -> Option<Self> {
            if parent.is_null() {
                return None;
            }
            unsafe {
                let cls = class(c"WKWebView");
                let config_cls = class(c"WKWebViewConfiguration");
                if cls.is_null() || config_cls.is_null() {
                    return None;
                }
                let config: *mut c_void =
                    send0(send0(config_cls, sel(c"alloc")), sel(c"init"));
                let frame = Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 };
                let obj: *mut c_void = send2(
                    send0(cls, sel(c"alloc")),
                    sel(c"initWithFrame:configuration:"),
                    frame,
                    config,
                );
                let _: () = send0(config, sel(c"release"));
                if obj.is_null() {
                    return None;
                }
                // The frame is set from the pane's geometry on every sync, so
                // AppKit must not also resize it behind our back.
                let _: () = send1(obj, sel(c"setAutoresizingMask:"), 0usize);
                let _: () = send1(obj, sel(c"setHidden:"), true);
                // Two-finger back/forward would navigate away from a page the
                // pane has no address bar to get back to.
                let _: () = send1(obj, sel(c"setAllowsBackForwardNavigationGestures:"), false);
                let _: () = send1(parent, sel(c"addSubview:"), obj);
                Some(Self { obj })
            }
        }

        pub fn load(&self, path: &Path) -> bool {
            unsafe {
                let url = file_url(path, false);
                let dir = match path.parent() {
                    Some(dir) => file_url(dir, true),
                    None => std::ptr::null_mut(),
                };
                if url.is_null() || dir.is_null() {
                    return false;
                }
                let nav: *mut c_void =
                    send2(self.obj, sel(c"loadFileURL:allowingReadAccessToURL:"), url, dir);
                !nav.is_null()
            }
        }

        pub fn set_frame(&self, parent_h: f64, x: f64, y: f64, w: f64, h: f64) {
            unsafe {
                // AppKit's default view space has its origin bottom-left, so
                // the pane's top-left y has to be flipped — unless the view
                // winit gave us is flipped already.
                let parent: *mut c_void = send0(self.obj, sel(c"superview"));
                let flipped: bool =
                    (!parent.is_null()) && send0(parent, sel(c"isFlipped"));
                let y = if flipped { y } else { parent_h - (y + h) };
                let frame = Rect { x, y, w: w.max(0.0), h: h.max(0.0) };
                let _: () = send1(self.obj, sel(c"setFrame:"), frame);
            }
        }

        pub fn set_hidden(&self, hidden: bool) {
            unsafe {
                let _: () = send1(self.obj, sel(c"setHidden:"), hidden);
            }
        }
    }

    impl Drop for View {
        fn drop(&mut self) {
            unsafe {
                let _: () = send0(self.obj, sel(c"removeFromSuperview"));
                let _: () = send0(self.obj, sel(c"release"));
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::ffi::c_void;
    use std::path::Path;

    pub struct View;

    impl View {
        pub fn new(_parent: *mut c_void) -> Option<Self> {
            None
        }
        pub fn load(&self, _path: &Path) -> bool {
            false
        }
        pub fn set_frame(&self, _parent_h: f64, _x: f64, _y: f64, _w: f64, _h: f64) {}
        pub fn set_hidden(&self, _hidden: bool) {}
    }
}
