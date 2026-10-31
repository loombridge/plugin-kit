//! The real-time thread harness: what every thread that runs audio needs
//! before its first block, and the checks that keep it within the real-time
//! rules: no allocation, lock or blocking call on an audio thread.
//!
//! - [`thread`]: real-time priority per platform (the Mach time-constraint
//!   policy on macOS, MMCSS "Pro Audio" on Windows, `SCHED_FIFO` or rtkit on
//!   Linux) and the set-up each real-time thread runs once:
//!   [`prepare_current_thread`] on threads the program starts, and
//!   [`AudioThreadSetup`] on threads an audio API owns, where it allocates
//!   nothing and nothing in it blocks.
//! - [`workgroup`]: macOS audio workgroups, joined so the scheduler treats a
//!   device's I/O thread and the program's real-time threads as one job.
//! - [`denormals`]: flushing subnormal floats to zero.
//! - [`trap`]: the allocation trap, a global allocator that counts heap use
//!   inside real-time sections.
//! - [`sanitize`]: LLVM's RealtimeSanitizer, which also catches locks and
//!   blocking calls, in builds that ask for it (`--cfg lspk_rtsan`).
//! - [`log`]: a lock-free logger for real-time threads.
//! - [`telemetry`]: a wait-free channel of live values to an editor, written
//!   only while one is watching.
//!
//! A real-time section is code that runs on the audio path. [`realtime`]
//! runs one: denormals flushed, heap use trapped, and RealtimeSanitizer
//! watching when it's active. An audio back end runs every audio block in one.
//!
//! # Testing real-time code
//!
//! A test crate installs the trap once, then wraps real-time code in
//! [`rt_test!`], which fails the test if the code touches the heap (and, under
//! RealtimeSanitizer, if it takes a lock or blocks):
//!
//! ```ignore
//! lspk_rt::install_trap!();
//!
//! lspk_rt::rt_test! {
//!     fn mixing_two_buffers_stays_off_the_heap() {
//!         // ...
//!     }
//! }
//!
//! #[test]
//! fn set_up_may_allocate_but_processing_may_not() {
//!     let mut buffer = vec![0.0f32; 512];
//!     lspk_rt::rt_test!({ buffer.iter_mut().for_each(|s| *s *= 0.5) });
//! }
//! ```

#[cfg(test)]
install_trap!();

pub mod denormals;
pub mod log;
pub mod sanitize;
pub mod telemetry;
pub mod thread;
pub mod trap;
pub mod workgroup;

#[doc(hidden)]
pub mod testing;

mod platform;
mod triple;

// rtkit is only used on Linux, but its D-Bus client is tested on every Unix
// against a fake bus.
#[cfg(any(target_os = "linux", all(test, unix)))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod dbus;
#[cfg(any(target_os = "linux", all(test, unix)))]
mod rtkit;

pub use thread::{
    AudioThreadSetup, Priority, Promotion, RtThreadConfig, ThreadReport, ThreadReports,
    prepare_current_thread,
};
pub use workgroup::{WorkInterval, Workgroup, WorkgroupJoin};

/// Runs `f` as a real-time section on this thread: denormals are flushed to
/// zero, heap use counts against the [allocation trap](trap), and
/// RealtimeSanitizer watches if this build has it.
///
/// Costs two floating-point control-register writes and a few thread-local
/// increments.
#[inline]
pub fn realtime<R>(f: impl FnOnce() -> R) -> R {
    let _section = Realtime::enter();
    f()
}

/// A real-time section that ends when the guard drops, for code that can't
/// put the section in a closure. Like [`realtime`], it's tied to its thread.
#[derive(Debug)]
#[must_use = "the section ends when the guard drops"]
pub struct Realtime {
    // Fields drop in order, so the section unwinds in the reverse of the
    // order `enter` set it up.
    _sanitizer: sanitize::Scope,
    _trap: trap::Forbid,
    _denormals: denormals::DenormalGuard,
}

impl Realtime {
    /// Starts a real-time section on this thread.
    #[inline]
    pub fn enter() -> Self {
        let denormals = denormals::DenormalGuard::new();
        let trap = trap::Forbid::enter();
        Self {
            _sanitizer: sanitize::Scope::enter(),
            _trap: trap,
            _denormals: denormals,
        }
    }
}

/// Runs code as a real-time section in a test, and fails the test if it
/// touched the heap. Under RealtimeSanitizer (`--cfg lspk_rtsan`), it also stops
/// the test at the first lock or blocking call.
///
/// Two forms: one defines a whole `#[test]` whose body is the section; the
/// other runs a block as a section inside an ordinary test, so set-up before
/// it may allocate. The test crate needs
/// [`install_trap!`](crate::install_trap).
///
/// ```ignore
/// lspk_rt::rt_test! {
///     #[should_panic(expected = "in a real-time section")]
///     fn allocating_fails() {
///         std::hint::black_box(Box::new(1));
///     }
/// }
///
/// let total = lspk_rt::rt_test!({ samples.iter().sum::<f32>() });
/// ```
#[macro_export]
macro_rules! rt_test {
    ($(#[$meta:meta])* fn $name:ident() $body:block) => {
        #[test]
        $(#[$meta])*
        fn $name() {
            $crate::testing::run(|| $body)
        }
    };
    ($body:expr) => {
        $crate::testing::run(|| $body)
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;

    #[test]
    fn a_real_time_section_flushes_denormals_and_traps_the_heap() {
        let before = trap::thread_violations();
        let flushed = realtime(|| {
            black_box(Box::new(1u8));
            denormals::flushing()
        });
        assert_eq!(flushed, denormals::SUPPORTED);
        assert!(
            !denormals::flushing(),
            "the section restores the thread's setting"
        );
        assert_eq!(trap::thread_violations() - before, 2);
    }

    #[test]
    fn the_guard_form_ends_with_its_scope() {
        let before = trap::thread_violations();
        {
            let _section = Realtime::enter();
            black_box(Box::new(1u8));
        }
        black_box(Box::new(1u8));
        assert_eq!(trap::thread_violations() - before, 2);
    }

    rt_test! {
        fn rt_test_passes_code_that_stays_off_the_heap() {
            let mut sum = 0.0f32;
            for i in 0..64 {
                sum += black_box(i as f32) * 0.5;
            }
            assert_eq!(sum, 1008.0);
        }
    }

    rt_test! {
        #[should_panic(expected = "in a real-time section; the first allocated 4096 bytes")]
        fn rt_test_fails_code_that_allocates() {
            black_box(vec![0u8; 4096]);
        }
    }

    #[test]
    fn rt_test_runs_a_block_after_set_up_that_allocates() {
        let mut buffer = vec![1.0f32; 256];
        let peak = rt_test!({
            buffer.iter_mut().for_each(|s| *s *= 0.25);
            buffer.iter().copied().fold(0.0f32, f32::max)
        });
        assert_eq!(peak, 0.25);
    }
}
