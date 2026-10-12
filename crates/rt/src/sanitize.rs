//! RealtimeSanitizer (LLVM's RTSan) around real-time sections.
//!
//! RTSan replaces `malloc`, `pthread_mutex_lock`, `sleep`, file and socket
//! calls, and a few hundred more with versions that check whether the
//! calling thread is inside a real-time section, and stop the program with a
//! report and a stack trace if it is. That catches what the allocation trap
//! can't: locks, blocking system calls, and allocation by C code, including
//! the operating system's own.
//!
//! It costs nothing unless a build asks for it: the code below exists only
//! under `--cfg lspk_rtsan`, which only a RealtimeSanitizer harness sets.
//! Such a harness is best kept as a Cargo workspace of its own, outside the
//! main one, so nothing that could fetch or build LLVM is in the main
//! dependency graph (Beatlayer's, in its `rtsan/` directory, is one). It
//! sets the flag for every crate it builds, and links a runtime already on
//! the machine: `libclang_rt.rtsan_osx_dynamic.dylib` from Homebrew's `llvm`
//! on macOS, or `libclang_rt.rtsan*.a` from LLVM 20 or later on Linux.

/// Whether this build runs real-time sections under RealtimeSanitizer.
pub const ACTIVE: bool = cfg!(lspk_rtsan)
    && cfg!(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    ));

/// The runtime's entry points (`compiler-rt/lib/rtsan/rtsan.h`), which the
/// harness links.
#[cfg(lspk_rtsan)]
mod ffi {
    use std::ffi::c_char;

    unsafe extern "C" {
        pub(super) fn __rtsan_realtime_enter();
        pub(super) fn __rtsan_realtime_exit();
        pub(super) fn __rtsan_disable();
        pub(super) fn __rtsan_enable();
        pub(super) fn __rtsan_ensure_initialized();
        pub(super) fn __rtsan_notify_blocking_call(function: *const c_char);
    }
}

/// Marks the calling thread as inside a real-time section until it drops.
#[derive(Debug)]
pub(crate) struct Scope {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Scope {
    #[inline]
    pub(crate) fn enter() -> Self {
        // SAFETY: the runtime keeps a per-thread depth; this guard pairs the
        // enter with an exit on the same thread.
        #[cfg(lspk_rtsan)]
        unsafe {
            ffi::__rtsan_realtime_enter()
        };
        Self {
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for Scope {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: pairs the enter in `Scope::enter` on this thread.
        #[cfg(lspk_rtsan)]
        unsafe {
            ffi::__rtsan_realtime_exit()
        };
    }
}

/// Initialises the sanitizer's runtime, if it's linked. Set-up calls this so
/// the runtime's own first-use work never happens inside a real-time section.
pub fn ensure_initialized() {
    // SAFETY: may be called any number of times, from any thread.
    #[cfg(lspk_rtsan)]
    unsafe {
        ffi::__rtsan_ensure_initialized()
    };
}

/// Runs `f` with RTSan's reports switched off, for a call that RTSan can't
/// see is safe. Each use needs a comment saying why it's safe.
pub fn unchecked<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(lspk_rtsan)]
    struct Enable;
    #[cfg(lspk_rtsan)]
    impl Drop for Enable {
        fn drop(&mut self) {
            // SAFETY: pairs the disable below on this thread.
            unsafe { ffi::__rtsan_enable() };
        }
    }
    // SAFETY: paired with the enable when `_enable` drops, even on unwind.
    #[cfg(lspk_rtsan)]
    let _enable = {
        unsafe { ffi::__rtsan_disable() };
        Enable
    };
    f()
}

/// Tells RTSan that the calling function blocks, so calling it from a
/// real-time section is reported even if nothing it calls is intercepted:
/// for example a function that spins on a lock built from atomics.
pub fn notify_blocking(function: &'static std::ffi::CStr) {
    // SAFETY: `function` is a valid C string for the life of the program.
    #[cfg(lspk_rtsan)]
    unsafe {
        ffi::__rtsan_notify_blocking_call(function.as_ptr())
    };
    #[cfg(not(lspk_rtsan))]
    let _ = function;
}
