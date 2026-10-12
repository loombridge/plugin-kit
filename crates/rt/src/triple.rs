//! A triple buffer: one writer publishes values, one reader takes the latest
//! one, and neither ever waits for the other or allocates.
//!
//! Three slots hold values. The writer owns one, the reader owns one, and the
//! third, the back slot, is exchanged through an atomic index. Writing fills
//! the writer's slot and swaps it with the back slot, marked fresh; reading
//! swaps the reader's slot with the back slot if that's fresh, then reads the
//! reader's own slot. Each side touches only the slot it owns, so no slot is
//! ever read and written at once.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// Set in the back index when the back slot holds a value the reader hasn't
/// taken.
const FRESH: u8 = 0b100;
/// The slot number in an index.
const SLOT: u8 = 0b011;

struct Shared<T> {
    slots: [UnsafeCell<T>; 3],
    /// The back slot's number, with [`FRESH`] set when it holds a new value.
    back: AtomicU8,
}

// SAFETY: each slot is accessed only by the side that owns it at the time,
// and ownership passes between the writer and the reader through `back` with
// acquire-release swaps, so a value moves between threads but is never shared.
unsafe impl<T: Send> Sync for Shared<T> {}

/// The writing end of a [`triple_buffer`].
pub(crate) struct Input<T> {
    shared: Arc<Shared<T>>,
    slot: u8,
}

/// The reading end of a [`triple_buffer`].
pub(crate) struct Output<T> {
    shared: Arc<Shared<T>>,
    slot: u8,
}

/// A triple buffer whose slots all start as copies of `initial`, as a writer
/// and a reader. Allocates once, here.
pub(crate) fn triple_buffer<T: Clone>(initial: &T) -> (Input<T>, Output<T>) {
    let shared = Arc::new(Shared {
        slots: [
            UnsafeCell::new(initial.clone()),
            UnsafeCell::new(initial.clone()),
            UnsafeCell::new(initial.clone()),
        ],
        back: AtomicU8::new(1),
    });
    let input = Input {
        shared: Arc::clone(&shared),
        slot: 0,
    };
    let output = Output { shared, slot: 2 };
    (input, output)
}

impl<T> Input<T> {
    /// Publishes `value`, replacing any the reader hasn't taken yet.
    /// Wait-free.
    pub(crate) fn write(&mut self, value: T) {
        // SAFETY: the writer owns `self.slot` until it swaps it into `back`.
        unsafe { *self.shared.slots[usize::from(self.slot)].get() = value };
        let back = self.shared.back.swap(self.slot | FRESH, Ordering::AcqRel);
        self.slot = back & SLOT;
    }
}

impl<T> Output<T> {
    /// The latest value published, or the initial value before the first.
    /// Wait-free.
    pub(crate) fn read(&mut self) -> &T {
        if self.shared.back.load(Ordering::Relaxed) & FRESH != 0 {
            let back = self.shared.back.swap(self.slot, Ordering::AcqRel);
            self.slot = back & SLOT;
        }
        // SAFETY: the reader owns `self.slot` until it swaps it into `back`,
        // which needs `&mut self`, so the borrow returned here ends first.
        unsafe { &*self.shared.slots[usize::from(self.slot)].get() }
    }
}

// SAFETY: an end holds the slot it owns and a handle to the shared slots,
// which are `Sync` for `T: Send`; moving an end moves only that ownership.
unsafe impl<T: Send> Send for Input<T> {}
// SAFETY: as for `Input`.
unsafe impl<T: Send> Send for Output<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reader_sees_the_initial_value_until_a_write() {
        let (_input, mut output) = triple_buffer(&7);
        assert_eq!(*output.read(), 7);
        assert_eq!(*output.read(), 7);
    }

    #[test]
    fn the_reader_sees_the_latest_write() {
        let (mut input, mut output) = triple_buffer(&None);
        input.write(Some(1));
        assert_eq!(*output.read(), Some(1));
        input.write(Some(2));
        input.write(Some(3));
        assert_eq!(*output.read(), Some(3));
        assert_eq!(*output.read(), Some(3), "reading again keeps the value");
    }

    #[test]
    fn values_cross_threads_in_order() {
        let (mut input, mut output) = triple_buffer(&0u64);
        let writer = std::thread::spawn(move || {
            for i in 1..=100_000 {
                input.write(i);
            }
        });
        let mut last = 0;
        while last < 100_000 {
            let now = *output.read();
            assert!(now >= last, "went back from {last} to {now}");
            last = now;
        }
        writer.join().unwrap();
    }
}
