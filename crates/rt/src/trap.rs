//! The allocation trap: a global allocator that notices allocation and
//! deallocation inside a real-time section.
//!
//! A binary or test crate installs it with [`install_trap!`](crate::install_trap).
//! Code then runs inside [`forbid`] (or [`realtime`](crate::realtime), which
//! adds denormal flushing and RealtimeSanitizer), and every allocation,
//! reallocation, or deallocation there counts as a violation on that thread.
//! The memory operation still happens, so the program carries on and the
//! violation is reported where it can be: [`rt_test!`](crate::rt_test) fails
//! the test, and an audio stream counts it in its statistics.
//!
//! Counting rather than aborting is deliberate. An abort inside the allocator
//! prints only "memory allocation failed", which reads like running out of
//! memory, and takes every other test in the binary down with it.
//!
//! The trap only sees Rust's global allocator. C code that calls `malloc`
//! directly (an operating system's audio stack, a plug-in) is invisible to it;
//! RealtimeSanitizer catches that (see [`sanitize`](crate::sanitize)).

use std::alloc::{GlobalAlloc, Layout, System};
use std::backtrace::Backtrace;
use std::cell::{Cell, RefCell};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// What a real-time section did to the heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryOp {
    /// `alloc` or `alloc_zeroed`.
    Allocate,
    /// `realloc`.
    Reallocate,
    /// `dealloc`.
    Free,
}

impl fmt::Display for MemoryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Allocate => "allocated",
            Self::Reallocate => "reallocated",
            Self::Free => "freed",
        })
    }
}

/// One violation: what happened and how many bytes it involved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Violation {
    /// The operation.
    pub op: MemoryOp,
    /// The size of the block, in bytes (the new size for a reallocation).
    pub bytes: usize,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} bytes", self.op, self.bytes)
    }
}

/// Whether the trap is this process's global allocator. Set by the first
/// allocation that goes through it.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Violations on every thread since the process started.
static TOTAL: AtomicU64 = AtomicU64::new(0);

thread_local! {
    // Every cell here is `const`-initialised and has no destructor, so the
    // allocator can read it at any point in a thread's life, including while
    // other thread-locals are being destroyed.
    static FORBID: Cell<u32> = const { Cell::new(0) };
    static PERMIT: Cell<u32> = const { Cell::new(0) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
    static LAST: Cell<Option<Violation>> = const { Cell::new(None) };
    static CAPTURE: Cell<bool> = const { Cell::new(false) };
    static WATCH_FIRST: Cell<Option<Violation>> = const { Cell::new(None) };
    // Holds a backtrace, so it has a destructor; only touched when CAPTURE is
    // set, which only tests do.
    static BACKTRACE: RefCell<Option<Backtrace>> = const { RefCell::new(None) };
}

/// A global allocator that wraps another (the system allocator by default)
/// and counts heap use inside real-time sections.
///
/// Install it with [`install_trap!`](crate::install_trap) rather than by hand.
#[derive(Debug, Default)]
pub struct TrapAllocator<A = System> {
    inner: A,
}

impl TrapAllocator<System> {
    /// The trap over the system allocator.
    pub const fn system() -> Self {
        Self { inner: System }
    }
}

impl<A> TrapAllocator<A> {
    /// The trap over `inner`.
    pub const fn new(inner: A) -> Self {
        Self { inner }
    }
}

// SAFETY: every method forwards to the inner allocator with the same
// arguments; the checks before it only read and write thread-locals and
// atomics, and never touch the memory being managed.
unsafe impl<A: GlobalAlloc> GlobalAlloc for TrapAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        check(MemoryOp::Allocate, layout.size());
        // SAFETY: the caller upholds `alloc`'s contract, which we pass on.
        unsafe { self.inner.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        check(MemoryOp::Allocate, layout.size());
        // SAFETY: as for `alloc`.
        unsafe { self.inner.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        check(MemoryOp::Free, layout.size());
        // SAFETY: as for `alloc`.
        unsafe { self.inner.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        check(MemoryOp::Reallocate, new_size);
        // SAFETY: as for `alloc`.
        unsafe { self.inner.realloc(ptr, layout, new_size) }
    }
}

/// Installs [`TrapAllocator`] as the global allocator of the binary or test
/// crate that invokes it.
///
/// Put it at the top level of a binary's `main.rs` (in debug builds, at
/// least) or of an integration test file. Each test binary needs
/// its own, because a library can't choose the allocator of the program it's
/// linked into.
///
/// ```ignore
/// lspk_rt::install_trap!();
/// ```
#[macro_export]
macro_rules! install_trap {
    () => {
        #[global_allocator]
        static LSPK_RT_ALLOCATION_TRAP: $crate::trap::TrapAllocator =
            $crate::trap::TrapAllocator::system();
    };
}

#[inline]
fn check(op: MemoryOp, bytes: usize) {
    if !INSTALLED.load(Relaxed) {
        INSTALLED.store(true, Relaxed);
    }
    let forbidden =
        FORBID.try_with(Cell::get).unwrap_or(0) > 0 && PERMIT.try_with(Cell::get).unwrap_or(0) == 0;
    if forbidden {
        record(Violation { op, bytes });
    }
}

#[cold]
fn record(violation: Violation) {
    TOTAL.fetch_add(1, Relaxed);
    let _ = COUNT.try_with(|count| count.set(count.get() + 1));
    let _ = LAST.try_with(|last| last.set(Some(violation)));
    if !CAPTURE.try_with(Cell::get).unwrap_or(false) {
        return;
    }
    let first = WATCH_FIRST
        .try_with(|slot| slot.get().is_none() && slot.replace(Some(violation)).is_none())
        .unwrap_or(false);
    if first {
        // Capturing allocates, so it runs with allocation permitted. The
        // allocator is re-entered, sees the permit, and passes straight on.
        permit(|| {
            let backtrace = Backtrace::force_capture();
            let _ = BACKTRACE.try_with(|slot| *slot.borrow_mut() = Some(backtrace));
        });
    }
}

/// Whether [`TrapAllocator`] is this process's global allocator.
///
/// Answers `false` until something has allocated, which in practice has
/// always happened before `main` or a test body runs.
pub fn is_installed() -> bool {
    INSTALLED.load(Relaxed)
}

/// Runs `f` with heap use forbidden on this thread: each allocation,
/// reallocation, or deallocation inside counts as a violation.
///
/// Sections nest. [`permit`] inside one allows heap use again.
pub fn forbid<R>(f: impl FnOnce() -> R) -> R {
    let _guard = Forbid::enter();
    f()
}

/// Runs `f` with heap use allowed, even inside [`forbid`]. For code that is
/// known to run off the real-time path, such as a test's own bookkeeping.
pub fn permit<R>(f: impl FnOnce() -> R) -> R {
    struct Permit;
    impl Drop for Permit {
        fn drop(&mut self) {
            let _ = PERMIT.try_with(|p| p.set(p.get() - 1));
        }
    }
    let _ = PERMIT.try_with(|p| p.set(p.get() + 1));
    let _guard = Permit;
    f()
}

/// A forbidden section that ends when the guard drops, for code that can't
/// put the section in a closure. It's tied to the thread it was made on.
///
/// Where the trap isn't installed (release builds), it does nothing and
/// touches no thread-local, so an audio thread's first block doesn't make
/// macOS allocate thread-local storage for it.
#[derive(Debug)]
#[must_use = "the section ends when the guard drops"]
pub struct Forbid {
    counted: bool,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Forbid {
    /// Starts a forbidden section on this thread.
    pub fn enter() -> Self {
        let counted = is_installed() && FORBID.try_with(|f| f.set(f.get() + 1)).is_ok();
        Self {
            counted,
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for Forbid {
    fn drop(&mut self) {
        if self.counted {
            let _ = FORBID.try_with(|f| f.set(f.get() - 1));
        }
    }
}

/// Violations on this thread since it started. Real-time safe; where the
/// trap isn't installed it's always 0 and touches no thread-local.
pub fn thread_violations() -> u64 {
    if !is_installed() {
        return 0;
    }
    COUNT.try_with(Cell::get).unwrap_or(0)
}

/// Touches the calling thread's trap counters, so that macOS, which
/// allocates a thread's thread-local storage on first touch, does so now
/// rather than in the thread's first real-time section. Set-up on threads
/// the program starts calls it.
///
/// A thread an audio API owns can't run anything before its first block, so
/// in builds with the trap that block's section is where macOS allocates
/// these counters. That allocation is the system's, through `malloc`, so the
/// trap doesn't count it, and it happens once per thread, in debug and test
/// builds only.
pub fn prepare_thread() {
    if is_installed() {
        let _ = FORBID.try_with(Cell::get);
        let _ = PERMIT.try_with(Cell::get);
        let _ = COUNT.try_with(Cell::get);
        let _ = LAST.try_with(Cell::get);
    }
}

/// The latest violation on this thread, if any. Real-time safe.
pub fn last_violation() -> Option<Violation> {
    LAST.try_with(Cell::get).unwrap_or(None)
}

/// Violations on every thread since the process started.
pub fn total_violations() -> u64 {
    TOTAL.load(Relaxed)
}

/// What [`watch`] saw.
#[derive(Debug)]
pub struct Report {
    /// How many violations happened.
    pub count: u64,
    /// The first one.
    pub first: Option<Violation>,
    /// Where the first one happened, if a backtrace was captured.
    pub backtrace: Option<Backtrace>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let noun = if self.count == 1 {
            "operation"
        } else {
            "operations"
        };
        write!(f, "{} heap {noun} in a real-time section", self.count)?;
        if let Some(first) = self.first {
            write!(f, "; the first {first}")?;
        }
        if let Some(backtrace) = &self.backtrace {
            write!(f, ", at:\n{backtrace}")?;
        }
        Ok(())
    }
}

/// Runs `f` (which sets up its own forbidden sections, or is one) and reports
/// the violations it caused on this thread, with a backtrace of the first.
/// For tests: capturing a backtrace is slow.
pub fn watch<R>(f: impl FnOnce() -> R) -> (R, Option<Report>) {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = CAPTURE.try_with(|c| c.set(self.0));
        }
    }

    let before = thread_violations();
    let _ = BACKTRACE.try_with(|slot| slot.borrow_mut().take());
    let _ = WATCH_FIRST.try_with(|slot| slot.set(None));
    let was_capturing = CAPTURE.try_with(|c| c.replace(true)).unwrap_or(false);
    let result = {
        let _restore = Restore(was_capturing);
        f()
    };
    let count = thread_violations() - before;
    if count == 0 {
        return (result, None);
    }
    let report = Report {
        count,
        first: WATCH_FIRST.try_with(Cell::take).unwrap_or(None),
        backtrace: BACKTRACE
            .try_with(|slot| slot.borrow_mut().take())
            .ok()
            .flatten(),
    };
    (result, Some(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;

    // The unit-test binary installs the trap in lib.rs.

    #[test]
    fn the_trap_is_installed_in_this_test_binary() {
        assert!(is_installed());
    }

    #[test]
    fn allocation_outside_a_section_is_fine() {
        let before = thread_violations();
        black_box(vec![1u8; 64]);
        assert_eq!(thread_violations(), before);
    }

    #[test]
    fn each_operation_inside_a_section_counts() {
        let mut held = Vec::<u64>::with_capacity(1);
        let before = thread_violations();
        forbid(|| {
            let boxed = black_box(Box::new(7u32));
            drop(boxed); // allocate and free
            held.reserve(1000); // reallocate
        });
        assert_eq!(thread_violations() - before, 3);
        assert_eq!(
            last_violation(),
            Some(Violation {
                op: MemoryOp::Reallocate,
                bytes: 1000 * 8
            })
        );
    }

    #[test]
    fn permit_allows_heap_use_inside_a_section() {
        let before = thread_violations();
        forbid(|| permit(|| black_box(vec![0u8; 16])));
        assert_eq!(thread_violations(), before);
    }

    #[test]
    fn sections_nest() {
        let before = thread_violations();
        forbid(|| {
            forbid(|| ());
            black_box(Box::new(1u8));
        });
        assert_eq!(
            thread_violations() - before,
            2,
            "still forbidden after the inner section"
        );
    }

    #[test]
    fn the_guard_ends_its_section_when_it_drops() {
        let before = thread_violations();
        {
            let _section = Forbid::enter();
            black_box(Box::new(1u8));
        }
        black_box(Box::new(1u8));
        assert_eq!(thread_violations() - before, 2);
    }

    #[test]
    fn watch_reports_the_first_violation_with_a_backtrace() {
        let ((), report) = watch(|| {
            forbid(|| {
                black_box(Box::new([0u8; 40]));
            })
        });
        let report = report.expect("a violation");
        assert_eq!(report.count, 2);
        assert!(report.backtrace.is_some());
        let text = report.to_string();
        assert!(
            text.starts_with("2 heap operations in a real-time section"),
            "{text}"
        );
    }

    #[test]
    fn watch_reports_nothing_for_a_clean_section() {
        let (value, report) = watch(|| forbid(|| black_box(2) + 2));
        assert_eq!(value, 4);
        assert!(report.is_none());
    }

    #[test]
    fn violations_on_one_thread_do_not_touch_another() {
        let before = thread_violations();
        let total_before = total_violations();
        // The box is made inside the section and dropped outside it: one
        // violation, on the spawned thread. Count from here, since the
        // test may run alone in its process.
        std::thread::spawn(|| forbid(|| black_box(Box::new(1u8))))
            .join()
            .unwrap();
        assert_eq!(thread_violations(), before);
        assert!(total_violations() > total_before);
    }
}
