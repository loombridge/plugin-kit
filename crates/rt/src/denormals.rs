//! Flushing denormal (subnormal) floats to zero.
//!
//! A filter or reverb tail that decays towards silence passes through numbers
//! too small for the normal float format. Many CPUs handle those in microcode,
//! up to a hundred times slower, which can make a quiet passage overrun its
//! block. Real-time sections therefore run with the CPU set to treat them as
//! zero:
//!
//! - x86 and x86-64: the FTZ (flush to zero) and DAZ (denormals are zero)
//!   bits of MXCSR, which cover SSE and AVX arithmetic.
//! - AArch64: the FZ bit of FPCR, which flushes both inputs and results.
//!
//! The setting is per thread. [`DenormalGuard`] sets it for a scope and puts
//! the previous value back, so a thread the program borrows from an audio API
//! is returned as it was found.
//!
//! The Rust compiler assumes the default floating-point environment, so it
//! may fold a constant expression differently from how the CPU would
//! evaluate it here. Audio code doesn't depend on denormal results, which is
//! why every audio engine makes this trade.

/// The control-register bits this module sets.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
const FLUSH_BITS: u32 = (1 << 15) | (1 << 6); // FTZ | DAZ

#[cfg(target_arch = "aarch64")]
const FLUSH_BITS: u64 = 1 << 24; // FZ

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
type Register = u32;

#[cfg(target_arch = "aarch64")]
type Register = u64;

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
type Register = ();

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn read() -> Register {
    let mut value: u32 = 0;
    // SAFETY: STMXCSR stores the 32-bit MXCSR register to the given address,
    // which points at a live, aligned local.
    unsafe {
        std::arch::asm!(
            "stmxcsr [{}]",
            in(reg) &raw mut value,
            options(nostack, preserves_flags)
        );
    }
    value
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn write(value: Register) {
    // SAFETY: LDMXCSR loads MXCSR from the given address. Only the FTZ and
    // DAZ bits differ from a value read earlier on this thread, and neither
    // can raise an exception.
    unsafe {
        std::arch::asm!(
            "ldmxcsr [{}]",
            in(reg) &raw const value,
            options(nostack, preserves_flags, readonly)
        );
    }
}

#[cfg(target_arch = "aarch64")]
fn read() -> Register {
    let value: u64;
    // SAFETY: reading FPCR has no side effects.
    unsafe {
        std::arch::asm!("mrs {}, fpcr", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

#[cfg(target_arch = "aarch64")]
fn write(value: Register) {
    // SAFETY: writing FPCR changes only this thread's floating-point
    // behaviour. Only the FZ bit differs from a value read earlier on this
    // thread.
    unsafe {
        std::arch::asm!("msr fpcr, {}", in(reg) value, options(nomem, nostack, preserves_flags));
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
fn read() -> Register {}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
fn write(_: Register) {}

/// Whether this CPU architecture can flush denormals (x86, x86-64, AArch64).
pub const SUPPORTED: bool = cfg!(any(
    target_arch = "x86",
    target_arch = "x86_64",
    target_arch = "aarch64"
));

/// Whether denormals are flushed to zero on this thread now.
pub fn flushing() -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
    {
        read() & FLUSH_BITS == FLUSH_BITS
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

/// Flushes denormals to zero on this thread from now on. For threads
/// the program owns; on a borrowed thread, use a [`DenormalGuard`].
pub fn flush_on_this_thread() {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
    write(read() | FLUSH_BITS);
}

/// Flushes denormals to zero on this thread until it drops, then restores the
/// previous setting. Costs two control-register writes.
#[derive(Debug)]
#[must_use = "denormals are flushed only while the guard lives"]
pub struct DenormalGuard {
    previous: Register,
    /// The setting is per thread, so the guard must stay on its thread.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl DenormalGuard {
    /// Starts flushing denormals on this thread.
    pub fn new() -> Self {
        let previous = read();
        #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
        if previous & FLUSH_BITS != FLUSH_BITS {
            write(previous | FLUSH_BITS);
        }
        Self {
            previous,
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Default for DenormalGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DenormalGuard {
    fn drop(&mut self) {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
        if read() != self.previous {
            write(self.previous);
        }
    }
}

#[cfg(all(
    test,
    any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")
))]
mod tests {
    use super::*;
    use std::hint::black_box;

    /// Halving the smallest normal float gives a denormal unless results are
    /// flushed. `black_box` keeps the compiler from working it out at build
    /// time, in the default floating-point environment.
    fn halve_smallest_normal() -> f32 {
        black_box(f32::MIN_POSITIVE) / black_box(2.0f32)
    }

    /// Multiplying a denormal by one gives it back unless inputs are flushed.
    fn denormal_times_one() -> f32 {
        black_box(f32::from_bits(1)) * black_box(1.0f32)
    }

    fn denormal_f64() -> f64 {
        black_box(f64::MIN_POSITIVE) / black_box(4.0f64)
    }

    #[test]
    fn denormals_survive_without_the_guard() {
        // Test threads start in the default environment.
        assert!(!flushing());
        assert!(halve_smallest_normal().is_subnormal());
        assert!(denormal_times_one().is_subnormal());
        assert!(denormal_f64().is_subnormal());
    }

    #[test]
    fn the_guard_flushes_results_and_inputs_to_zero() {
        let guard = DenormalGuard::new();
        assert!(flushing());
        assert_eq!(halve_smallest_normal(), 0.0, "results flush");
        assert_eq!(denormal_times_one(), 0.0, "inputs flush");
        assert_eq!(denormal_f64(), 0.0, "doubles flush too");
        // Normal numbers are untouched.
        assert_eq!(
            black_box(f32::MIN_POSITIVE) * black_box(2.0f32),
            f32::MIN_POSITIVE * 2.0
        );
        drop(guard);
        assert!(!flushing());
        assert!(
            halve_smallest_normal().is_subnormal(),
            "the guard restores the old setting"
        );
    }

    #[test]
    fn guards_nest() {
        let outer = DenormalGuard::new();
        {
            let _inner = DenormalGuard::new();
            assert!(flushing());
        }
        assert!(
            flushing(),
            "the inner guard restores the outer one's setting"
        );
        drop(outer);
        assert!(!flushing());
    }

    #[test]
    fn flushing_is_per_thread() {
        std::thread::spawn(|| {
            flush_on_this_thread();
            assert!(flushing());
            assert_eq!(halve_smallest_normal(), 0.0);
        })
        .join()
        .unwrap();
        assert!(!flushing(), "another thread's setting doesn't leak here");
    }

    #[test]
    fn a_decaying_signal_reaches_zero_instead_of_lingering_as_denormals() {
        // A one-pole decay: y = 0.5 y. Without flushing it spends 23 steps in
        // the denormal range before reaching zero; with flushing, none.
        let count_denormal_steps = || {
            let mut y = black_box(1.0f32);
            let mut denormal_steps = 0;
            while y != 0.0 {
                y = black_box(y) * black_box(0.5f32);
                if y.is_subnormal() {
                    denormal_steps += 1;
                }
            }
            denormal_steps
        };
        assert_eq!(count_denormal_steps(), 23);
        let _guard = DenormalGuard::new();
        assert_eq!(count_denormal_steps(), 0);
    }
}
