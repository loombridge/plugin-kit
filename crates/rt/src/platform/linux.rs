//! Linux: `SCHED_FIFO` directly where the process may set it, otherwise
//! through rtkit.
//!
//! The direct call works when `RLIMIT_RTPRIO` allows it (a member of an
//! `audio` group with rtprio limits, as audio-focused distributions set up) or
//! the process has `CAP_SYS_NICE`. It's one system call, so it comes first.
//! Desktop distributions without those limits run rtkit, a system service
//! that grants real-time scheduling (`SCHED_RR`, at most priority 20 by
//! default) to threads that ask over D-Bus.
//!
//! Both take a thread ID, so neither has to run on the thread being raised.
//! On a thread an audio API owns, the thread only publishes its ID, and a
//! helper thread does the rest (see
//! [`AudioThreadSetup`](crate::AudioThreadSetup)): rtkit is a round trip over
//! D-Bus to another process, which can block for as long as that process
//! takes.
//!
//! The scheduling calls here are `sched_*` with a thread ID rather than
//! `pthread_*`: glibc's `pthread_getschedparam` caches a thread's policy, so
//! it wouldn't see a change made from another thread, and both `pthread_*`
//! calls take a lock inside the thread's descriptor.

use std::time::Duration;

use crate::rtkit;
use crate::thread::{Method, Priority, Promotion};

/// The real-time priority the kit asks for: in the middle of the range and
/// below PipeWire's own audio threads (88 by default), so an engine thread
/// never preempts the sound server it feeds. rtkit lowers it to its own cap.
const WANTED: i32 = 70;

/// Nothing to do ahead of time on Linux.
pub(crate) fn warm_up() {}

/// Nothing to undo on Linux: the policy goes with the thread.
#[derive(Debug, Default)]
pub(crate) struct HeldPriority;

impl HeldPriority {
    pub(crate) fn revert(&mut self) {}

    pub(crate) fn forget(&mut self) {}
}

/// The calling thread's ID. One system call that doesn't block.
pub(crate) fn current_thread_id() -> libc::pid_t {
    // SAFETY: `gettid` always succeeds.
    unsafe { libc::gettid() }
}

pub(crate) fn current_priority() -> Priority {
    priority_of(current_thread_id())
}

/// How thread `tid` of this process is scheduled. Three system calls that
/// don't block.
pub(crate) fn priority_of(tid: libc::pid_t) -> Priority {
    // SAFETY: no pointers.
    let policy = unsafe { libc::sched_getscheduler(tid) };
    if policy < 0 {
        return Priority::Unknown;
    }
    // SAFETY: `sched_param` is plain data, so zeroed is a valid value.
    let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
    // SAFETY: `param` is writable.
    if unsafe { libc::sched_getparam(tid, &mut param) } != 0 {
        return Priority::Unknown;
    }
    match policy & !libc::SCHED_RESET_ON_FORK {
        libc::SCHED_FIFO => Priority::Fifo {
            level: param.sched_priority,
        },
        libc::SCHED_RR => Priority::RoundRobin {
            level: param.sched_priority,
        },
        _ => {
            // SAFETY: reads the nice value of one thread. -1 is also a valid
            // nice value, so an error can't be told apart; it doesn't matter
            // for a report.
            let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, tid as libc::id_t) };
            Priority::Normal { level: nice }
        }
    }
}

/// The highest real-time priority this process may set by itself, if any.
fn rtprio_limit() -> Option<i32> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is writable.
    if unsafe { libc::getrlimit(libc::RLIMIT_RTPRIO, &mut limit) } != 0 {
        return None;
    }
    if limit.rlim_cur == libc::RLIM_INFINITY {
        Some(i32::MAX)
    } else {
        i32::try_from(limit.rlim_cur).ok()
    }
}

fn set_fifo(tid: libc::pid_t, level: i32) -> Result<(), i32> {
    let param = libc::sched_param {
        sched_priority: level,
    };
    // SAFETY: `param` is a valid `sched_param`. SCHED_RESET_ON_FORK keeps
    // child processes from inheriting real-time scheduling.
    let status = unsafe {
        libc::sched_setscheduler(tid, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param)
    };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1))
    }
}

/// Raises the calling thread. For threads the program starts, before their
/// real-time work: it may ask rtkit, which can block.
pub(crate) fn promote(_period: Duration, _held: &mut HeldPriority) -> Promotion {
    promote_thread(current_thread_id())
}

/// Raises thread `tid` of this process: `SCHED_FIFO` directly if the process
/// may, otherwise through rtkit. May block on rtkit, so it never runs on an
/// audio thread.
pub(crate) fn promote_thread(tid: libc::pid_t) -> Promotion {
    if priority_of(tid).is_realtime() {
        return Promotion::AlreadyRealtime;
    }
    // SAFETY: no preconditions.
    let max = unsafe { libc::sched_get_priority_max(libc::SCHED_FIFO) };
    let level = match rtprio_limit() {
        // A limit of zero still lets a process with CAP_SYS_NICE through.
        Some(limit) if limit > 0 => WANTED.min(limit),
        _ => WANTED,
    }
    .min(max)
    .max(1);
    if set_fifo(tid, level).is_ok() {
        return Promotion::Raised(Method::SchedFifo);
    }
    match rtkit::make_thread_realtime(u64::try_from(tid).unwrap_or(0), WANTED) {
        Ok(_) => Promotion::Raised(Method::Rtkit),
        Err(refusal) => Promotion::Refused(Method::Rtkit, refusal),
    }
}
