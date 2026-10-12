//! Windows: the Multimedia Class Scheduler Service (MMCSS), task "Pro Audio".
//!
//! MMCSS schedules a registered thread in the real-time priority range for
//! its task's share of each period. "Pro Audio" is the task meant for audio
//! engines; the `audio_thread_priority` crate registers "Audio" instead,
//! which is one reason the kit writes this itself.
//!
//! Registration applies to the calling thread, so on a thread an audio API
//! owns it runs inside the first callback (see
//! [`AudioThreadSetup`](crate::AudioThreadSetup)).
//!
//! # Threads the audio API already raised
//!
//! Before registering, set-up reads the thread's priority, and leaves alone a
//! thread that already runs at real-time priority:
//!
//! - A thread at `THREAD_PRIORITY_TIME_CRITICAL`, which ASIO drivers give
//!   their callback threads, and which `cpal` falls back to when its
//!   `realtime` feature can't register with MMCSS.
//! - Any thread of a process in `REALTIME_PRIORITY_CLASS`.
//! - A thread the kit registered itself.
//!
//! A registration someone else made can't be seen: Windows has no documented
//! call that reads a thread's MMCSS task, and MMCSS raises a registered
//! thread's priority without changing what `GetThreadPriority` returns.
//! A program using the kit shouldn't turn on `cpal`'s `realtime` feature,
//! which would register WASAPI's thread with the "Audio" task (Beatlayer
//! doesn't). An ASIO driver that
//! registers its own thread with MMCSS gets registered again, with "Pro
//! Audio"; Microsoft doesn't document what a second registration does, so
//! the first run on a Windows test machine checks it.

use std::cell::Cell;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    AVRT_PRIORITY_CRITICAL, AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
    AvSetMmThreadPriority, GetCurrentProcess, GetCurrentThread, GetPriorityClass,
    GetThreadPriority, REALTIME_PRIORITY_CLASS, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::core::w;

use crate::thread::{Method, Priority, Promotion, Refusal};

const TASK: &str = "Pro Audio";

thread_local! {
    /// The task index of this thread's MMCSS registration, if the kit made
    /// one. Windows sets thread-local storage up with the thread, so reading
    /// it never allocates.
    static TASK_INDEX: Cell<Option<u32>> = const { Cell::new(None) };
}

/// Nothing to do ahead of time on Windows.
pub(crate) fn warm_up() {}

/// A thread's MMCSS registration. Its owner reverts it on the registered
/// thread, or forgets it; dropping it does neither, because it may drop on
/// another thread.
#[derive(Debug, Default)]
pub(crate) struct HeldPriority {
    handle: Option<isize>,
}

impl HeldPriority {
    /// Ends the registration. Call it on the registered thread.
    pub(crate) fn revert(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: the handle came from AvSetMmThreadCharacteristicsW on
            // this thread and hasn't been reverted.
            let _ = unsafe { AvRevertMmThreadCharacteristics(HANDLE(handle as *mut _)) };
            let _ = TASK_INDEX.try_with(|index| index.set(None));
        }
    }

    /// Drops the record of a registration on another thread, which keeps it
    /// until it exits.
    pub(crate) fn forget(&mut self) {
        self.handle = None;
    }
}

fn thread_priority() -> i32 {
    // SAFETY: the pseudo-handle for the current thread is always valid.
    unsafe { GetThreadPriority(GetCurrentThread()) }
}

pub(crate) fn current_priority() -> Priority {
    let level = thread_priority();
    if let Some(task_index) = TASK_INDEX.try_with(Cell::get).ok().flatten() {
        return Priority::Mmcss {
            task: TASK,
            task_index,
            level,
        };
    }
    // SAFETY: the pseudo-handle for the current process is always valid.
    let class = unsafe { GetPriorityClass(GetCurrentProcess()) };
    if class == REALTIME_PRIORITY_CLASS.0 {
        Priority::RealtimeClass { level }
    } else if level == THREAD_PRIORITY_TIME_CRITICAL.0 {
        Priority::TimeCritical
    } else {
        Priority::Normal { level }
    }
}

pub(crate) fn promote(_period: std::time::Duration, held: &mut HeldPriority) -> Promotion {
    let mut task_index = 0u32;
    // SAFETY: the task name is a valid wide string and `task_index` is
    // writable.
    let handle = match unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) } {
        Ok(handle) => handle,
        Err(error) => return Promotion::Refused(Method::Mmcss, Refusal::Os(error.code().0)),
    };
    // SAFETY: `handle` was just returned for this thread.
    if let Err(error) = unsafe { AvSetMmThreadPriority(handle, AVRT_PRIORITY_CRITICAL) } {
        // SAFETY: as above.
        let _ = unsafe { AvRevertMmThreadCharacteristics(handle) };
        return Promotion::Refused(Method::Mmcss, Refusal::Os(error.code().0));
    }
    held.handle = Some(handle.0 as isize);
    let _ = TASK_INDEX.try_with(|index| index.set(Some(task_index)));
    Promotion::Raised(Method::Mmcss)
}
