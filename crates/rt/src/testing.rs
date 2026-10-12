//! What [`rt_test!`](crate::rt_test) expands to. Not for direct use.

use crate::trap;

/// Runs `f` as a real-time section and panics, with where the first heap
/// operation happened, if it touched the heap.
#[track_caller]
pub fn run<R>(f: impl FnOnce() -> R) -> R {
    assert!(
        trap::is_installed(),
        "rt_test! needs the allocation trap: add `lspk_rt::install_trap!();` to this test crate"
    );
    // First use of these allocates thread-local storage on macOS, or the
    // sanitizer's own state; do it outside the section.
    crate::sanitize::ensure_initialized();
    crate::thread::serial();
    crate::trap::prepare_thread();
    let (value, report) = trap::watch(|| crate::realtime(f));
    if let Some(report) = report {
        panic!("{report}");
    }
    value
}
