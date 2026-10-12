//! Handing compiled programs to the audio thread, and the programs it
//! retires back, without locks or allocation on the audio side.
//!
//! The sender (the main thread, or wherever programs are compiled) boxes a
//! program and swaps it into the incoming slot; an unsent program it
//! replaces is dropped there. At a block's start the engine takes the
//! incoming program, but only once the retired slot is empty, and puts the
//! program it replaces in the retired slot. The sender empties that slot,
//! dropping the old program, on its next send or [`collect`](ProgramSender::collect).
//! So every program is freed on the sending thread, never the audio thread.

use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::program::Program;

struct Shared {
    incoming: AtomicPtr<Program>,
    retired: AtomicPtr<Program>,
}

impl Shared {
    /// Takes the box in `slot`, if any.
    fn take(slot: &AtomicPtr<Program>) -> Option<Box<Program>> {
        let p = slot.swap(ptr::null_mut(), Ordering::AcqRel);
        // SAFETY: a non-null pointer in a slot came from `Box::into_raw`, and
        // the swap took it out, so this is its only owner.
        (!p.is_null()).then(|| unsafe { Box::from_raw(p) })
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        drop(Self::take(&self.incoming));
        drop(Self::take(&self.retired));
    }
}

/// The sending end: compiled programs go in, retired ones are dropped here.
pub struct ProgramSender {
    shared: Arc<Shared>,
}

/// The receiving end, which an [`Engine`](crate::Engine) holds once
/// [attached](crate::Engine::attach).
pub struct ProgramReceiver {
    shared: Arc<Shared>,
}

/// A mailbox for programs: a sender for the thread that compiles, and a
/// receiver for the engine. Allocates; make it off the audio thread.
#[must_use]
pub fn mailbox() -> (ProgramSender, ProgramReceiver) {
    let shared = Arc::new(Shared {
        incoming: AtomicPtr::new(ptr::null_mut()),
        retired: AtomicPtr::new(ptr::null_mut()),
    });
    (
        ProgramSender {
            shared: Arc::clone(&shared),
        },
        ProgramReceiver { shared },
    )
}

impl ProgramSender {
    /// Sends `program`; the engine takes it at its next block. A program
    /// sent earlier and not yet taken is dropped.
    pub fn send(&mut self, program: Program) {
        self.collect();
        let p = Box::into_raw(Box::new(program));
        drop(Shared::take_replaced(&self.shared.incoming, p));
    }

    /// Drops the program the engine last retired, if any. Returns whether
    /// there was one.
    pub fn collect(&mut self) -> bool {
        Shared::take(&self.shared.retired).is_some()
    }

    /// Whether a sent program is still waiting for the engine.
    #[must_use]
    pub fn pending(&self) -> bool {
        !self.shared.incoming.load(Ordering::Acquire).is_null()
    }
}

impl Shared {
    /// Puts `p` in `slot` and takes what was there.
    fn take_replaced(slot: &AtomicPtr<Program>, p: *mut Program) -> Option<Box<Program>> {
        let old = slot.swap(p, Ordering::AcqRel);
        // SAFETY: as in `take`: the swap took the only pointer to the box.
        (!old.is_null()).then(|| unsafe { Box::from_raw(old) })
    }
}

impl ProgramReceiver {
    /// The waiting program, if there is one and the retired slot is free
    /// for the program it replaces. Wait-free.
    pub(crate) fn take(&mut self) -> Option<Box<Program>> {
        if !self.shared.retired.load(Ordering::Acquire).is_null() {
            return None;
        }
        Shared::take(&self.shared.incoming)
    }

    /// Hands `old` back to the sender to drop. Wait-free.
    pub(crate) fn retire(&mut self, old: Box<Program>) {
        // Only this side fills the retired slot, and `take` saw it empty.
        let previous = Shared::take_replaced(&self.shared.retired, Box::into_raw(old));
        debug_assert!(previous.is_none(), "the retired slot was empty");
        // Never reached: forget rather than free on the audio thread.
        std::mem::forget(previous);
    }
}
