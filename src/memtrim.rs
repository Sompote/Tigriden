//! Hands freed heap memory back to the system on a timer.
//!
//! The allocator keeps pages it has freed so the next allocation is cheap.
//! Each painted frame, PDF page and closed tab frees large blocks, and over a
//! day or two of terminal output the process ends up holding a gigabyte or
//! more that it no longer uses. macOS counts those pages against Tigriden
//! until memory pressure forces them out, which on a busy machine means swap.
//! Asking the allocator to release them once a minute keeps the resident size
//! close to what is really in use, at a cost of a few milliseconds on a thread
//! that never touches the UI.

use std::time::Duration;

const INTERVAL: Duration = Duration::from_secs(60);

#[cfg(target_os = "macos")]
extern "C" {
    /// Returns free pages in `zone` (all zones when null) to the kernel until
    /// `goal` bytes are released, or as many as it can when `goal` is 0.
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
extern "C" {
    fn malloc_trim(pad: usize) -> i32;
}

/// Releases whatever the allocator holds free. Safe to call from any thread.
pub fn release_now() {
    // Safety: plain libc calls with no pointers of ours; both take the
    // allocator's own locks.
    #[cfg(target_os = "macos")]
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        malloc_trim(0);
    }
}

/// Starts the background thread that releases free memory every minute.
pub fn start() {
    let _ = std::thread::Builder::new().name("memtrim".into()).spawn(|| loop {
        std::thread::sleep(INTERVAL);
        release_now();
    });
}
