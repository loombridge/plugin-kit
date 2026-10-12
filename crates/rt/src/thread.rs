//! Real-time priority for audio threads, and the set-up every real-time
//! thread runs once.
//!
//! Two kinds of thread need set-up, and they get it in different ways:
//!
//! - **Threads the program starts** (the null device's, the engine's worker
//!   pool) call [`prepare_current_thread`] before they start real-time work.
//!   No deadline is running yet, so it may allocate and, on Linux, ask rtkit
//!   over D-Bus.
//! - **Threads an audio API owns** (Core Audio's I/O thread, WASAPI's, an
//!   ASIO driver's, ALSA's and JACK's) first run the program's code inside a
//!   device callback, with the block's deadline already running. For them,
//!   [`AudioThreadSetup`] does everything it can beforehand, off that thread,
//!   and [`AudioThreadSetup::enter`] does on the thread only what the system
//!   requires there, once, without allocating or blocking.
//!
//! | Platform | Real-time class | How the program asks for it | On an audio API's thread |
//! |---|---|---|---|
//! | macOS | Mach time-constraint policy | `thread_policy_set`, then joins the audio workgroup | both on the thread: the join applies to the calling thread |
//! | Windows | MMCSS "Pro Audio" task | `AvSetMmThreadCharacteristicsW` | on the thread: registration applies to the calling thread |
//! | Linux | `SCHED_FIFO` (or rtkit's `SCHED_RR`) | `sched_setscheduler` by thread ID, then rtkit over D-Bus | a helper thread, given the thread's ID |
//!
//! A thread an audio API already runs in real time (Core Audio's I/O thread,
//! JACK's process thread, an ASIO driver's thread at
//! `THREAD_PRIORITY_TIME_CRITICAL`) is left as it is. On macOS that thread is
//! also already a member of its device's workgroup, which Apple documents for
//! `kAudioDevicePropertyIOThreadOSWorkgroup`.

use std::cell::RefCell;
use std::fmt;
use std::time::Duration;

#[cfg(not(target_os = "macos"))]
use std::cell::Cell;
#[cfg(not(target_os = "macos"))]
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex, PoisonError};

use crate::platform;
use crate::workgroup::{Membership, Workgroup, WorkgroupJoin};

/// How the operating system schedules a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Priority {
    /// This platform doesn't say.
    #[default]
    Unknown,
    /// Ordinary time-sharing: the thread competes with every other thread.
    /// `level` is the thread's base priority on macOS, the nice value on
    /// Linux, and `GetThreadPriority` on Windows.
    Normal {
        /// The platform's priority number.
        level: i32,
    },
    /// macOS's real-time class, the Mach time-constraint policy: the thread
    /// needs `computation` of CPU time in every `period`, finished within
    /// `constraint` of the period's start.
    TimeConstraint {
        /// How often the work repeats.
        period: Duration,
        /// How much CPU time each period needs.
        computation: Duration,
        /// The deadline, from the start of each period.
        constraint: Duration,
    },
    /// A Windows Multimedia Class Scheduler Service task, which the service
    /// schedules in the real-time range.
    Mmcss {
        /// The task's name, such as "Pro Audio".
        task: &'static str,
        /// The task index MMCSS assigned.
        task_index: u32,
        /// `GetThreadPriority` after registering.
        level: i32,
    },
    /// Windows `THREAD_PRIORITY_TIME_CRITICAL`, the highest priority outside
    /// MMCSS and the real-time priority class, which ASIO drivers give their
    /// callback threads.
    TimeCritical,
    /// A thread of a Windows process in `REALTIME_PRIORITY_CLASS`, whose
    /// threads all run in the real-time range.
    RealtimeClass {
        /// `GetThreadPriority`.
        level: i32,
    },
    /// POSIX `SCHED_FIFO`.
    Fifo {
        /// The real-time priority, 1 to 99 on Linux.
        level: i32,
    },
    /// POSIX `SCHED_RR`, which rtkit grants.
    RoundRobin {
        /// The real-time priority.
        level: i32,
    },
}

impl Priority {
    /// Whether this is a real-time class.
    pub fn is_realtime(&self) -> bool {
        !matches!(self, Self::Unknown | Self::Normal { .. })
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = |d: &Duration| d.as_secs_f64() * 1000.0;
        match self {
            Self::Unknown => f.write_str("unknown"),
            Self::Normal { level } => write!(f, "normal (priority {level})"),
            Self::TimeConstraint {
                period,
                computation,
                constraint,
            } => write!(
                f,
                "real-time, Mach time-constraint policy (period {:.3} ms, computation {:.3} ms, constraint {:.3} ms)",
                ms(period),
                ms(computation),
                ms(constraint)
            ),
            Self::Mmcss {
                task,
                task_index,
                level,
            } => write!(
                f,
                "real-time, MMCSS task \"{task}\" (index {task_index}, thread priority {level})"
            ),
            Self::TimeCritical => f.write_str("real-time, THREAD_PRIORITY_TIME_CRITICAL"),
            Self::RealtimeClass { level } => write!(
                f,
                "real-time, REALTIME_PRIORITY_CLASS process (thread priority {level})"
            ),
            Self::Fifo { level } => write!(f, "real-time, SCHED_FIFO priority {level}"),
            Self::RoundRobin { level } => write!(f, "real-time, SCHED_RR priority {level}"),
        }
    }
}

/// A way of raising a thread to real-time priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// macOS `thread_policy_set` with the time-constraint policy.
    TimeConstraint,
    /// Windows MMCSS, task "Pro Audio".
    Mmcss,
    /// Linux `sched_setscheduler` with `SCHED_FIFO`.
    SchedFifo,
    /// rtkit's `MakeThreadRealtime` over the D-Bus system bus (Linux).
    Rtkit,
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TimeConstraint => "the Mach time-constraint policy",
            Self::Mmcss => "MMCSS",
            Self::SchedFifo => "SCHED_FIFO",
            Self::Rtkit => "rtkit",
        })
    }
}

/// Why a request for real-time priority failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The system call failed with this code: an `errno`, a Mach
    /// `kern_return_t`, or a Windows error.
    Os(i32),
    /// No D-Bus system bus to reach rtkit on.
    NoBus,
    /// rtkit isn't running.
    NoRtkit,
    /// rtkit said no. It refuses processes that ask too often, threads that
    /// aren't the caller's, and priorities above its limit.
    RtkitDenied,
    /// The conversation with D-Bus or rtkit went wrong.
    Protocol,
    /// The helper thread that raises audio threads on Linux couldn't start.
    NoHelper,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Os(code) => write!(f, "error {code}"),
            Self::NoBus => f.write_str("no D-Bus system bus"),
            Self::NoRtkit => f.write_str("rtkit isn't running"),
            Self::RtkitDenied => f.write_str("rtkit refused"),
            Self::Protocol => f.write_str("a D-Bus protocol error"),
            Self::NoHelper => f.write_str("no helper thread to ask it"),
        }
    }
}

/// What set-up did about the thread's priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Promotion {
    /// The thread wasn't to be raised, for example because it runs a test
    /// device as fast as it can.
    #[default]
    NotRequested,
    /// The thread was already real-time: the audio API had set it up.
    AlreadyRealtime,
    /// The program raised it.
    Raised(Method),
    /// The program asked and was refused; the last method tried is named.
    Refused(Method, Refusal),
    /// Linux: the audio thread published its ID, and a helper thread hasn't
    /// answered yet. [`ThreadReports::latest`] shows the answer once it has.
    Pending,
    /// This platform has no way to ask.
    Unsupported,
}

impl fmt::Display for Promotion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRequested => f.write_str("not requested"),
            Self::AlreadyRealtime => f.write_str("already real-time when the program got it"),
            Self::Raised(method) => write!(f, "raised by the program through {method}"),
            Self::Refused(method, why) => write!(f, "refused: {method} failed with {why}"),
            Self::Pending => f.write_str("requested; a helper thread is raising it"),
            Self::Unsupported => f.write_str("not supported on this platform"),
        }
    }
}

/// What a real-time thread's set-up asks for.
#[derive(Debug, Clone, Default)]
pub struct RtThreadConfig {
    /// How often the thread's work repeats: one audio block. macOS uses it for
    /// the time-constraint policy.
    pub period: Duration,
    /// Whether to raise the thread to real-time priority if it isn't already.
    pub promote: bool,
    /// The workgroup to join, on macOS: the output device's, so the system
    /// schedules this thread with the device's I/O thread.
    pub workgroup: Option<Workgroup>,
}

/// The state of a real-time thread after set-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThreadReport {
    /// How the thread is scheduled now.
    pub priority: Priority,
    /// What set-up did about it.
    pub promotion: Promotion,
    /// Whether the thread is in the workgroup (macOS).
    pub workgroup: WorkgroupJoin,
    /// Whether denormals are flushed to zero inside real-time sections.
    pub denormals_flushed: bool,
    /// Whether the allocation trap is the global allocator, so real-time
    /// sections count heap use.
    pub trap_installed: bool,
    /// Whether RealtimeSanitizer watches real-time sections.
    pub sanitizer_active: bool,
}

thread_local! {
    static HELD: RefCell<Option<HeldHere>> = const { RefCell::new(None) };
}

#[cfg(not(target_os = "macos"))]
thread_local! {
    static SERIAL: Cell<u64> = const { Cell::new(0) };
}

#[cfg(not(target_os = "macos"))]
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// A number for the calling thread that no other thread in this process gets,
/// even after this one exits, and never 0 on a running thread. Real-time
/// safe, and allocation-free from a thread's first instruction.
///
/// On macOS it's the kernel's 64-bit thread ID, which isn't reused while the
/// system runs; it isn't kept in a thread-local, because macOS allocates a
/// thread's thread-local storage the first time the thread touches it.
/// Elsewhere it's a counter in a thread-local, which Linux and Windows set up
/// with the thread. Operating-system thread IDs can be reused there, and
/// Core Audio moves its I/O thread when the default device changes, so
/// per-thread set-up keys on this instead.
pub fn serial() -> u64 {
    #[cfg(target_os = "macos")]
    {
        platform::thread_id()
    }
    #[cfg(not(target_os = "macos"))]
    {
        SERIAL
            .try_with(|serial| {
                if serial.get() == 0 {
                    serial.set(NEXT_SERIAL.fetch_add(1, Relaxed));
                }
                serial.get()
            })
            .unwrap_or(0)
    }
}

/// How the calling thread is scheduled now.
pub fn current_priority() -> Priority {
    platform::current_priority()
}

/// What a thread's set-up holds on to: its workgroup membership, and on
/// Windows its MMCSS registration. Neither is undone on drop, because only
/// the thread that set up may undo them.
struct OnThread {
    membership: Membership,
    platform: platform::HeldPriority,
}

impl OnThread {
    /// Allocates the workgroup token's storage.
    #[allow(
        clippy::default_constructed_unit_structs,
        reason = "HeldPriority is a unit struct only on some platforms"
    )]
    fn new() -> Self {
        Self {
            membership: Membership::new(),
            platform: platform::HeldPriority::default(),
        }
    }

    /// Leaves the workgroup and ends the MMCSS registration. Only the thread
    /// that set up may call it.
    fn release(&mut self) {
        self.membership.leave();
        self.platform.revert();
    }

    /// Drops the records of another thread's set-up, which that thread keeps
    /// until it exits.
    fn forget(&mut self) {
        self.membership.forget();
        self.platform.forget();
    }
}

/// [`OnThread`] kept in a thread-local by [`prepare_current_thread`], which
/// undoes it when the thread exits.
struct HeldHere(OnThread);

impl Drop for HeldHere {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// The set-up that runs on the thread itself: checks denormal flushing, reads
/// the priority and raises it if asked, and joins the workgroup.
///
/// With `raise_here` false, a thread that needs raising is reported as
/// [`Promotion::Pending`] for a helper to raise. Then it allocates nothing
/// and nothing in it blocks: on macOS it makes a few Mach calls
/// (`thread_policy_get`, `thread_info`, and `thread_policy_set` if the
/// thread isn't real-time) and `os_workgroup_join`; on Windows
/// `GetThreadPriority`, `GetPriorityClass`, and the MMCSS registration if the
/// thread isn't real-time; on Linux `gettid` and three scheduling queries.
fn set_up_here(config: &RtThreadConfig, held: &mut OnThread, raise_here: bool) -> ThreadReport {
    let denormals_flushed = {
        let _flushed = crate::denormals::DenormalGuard::new();
        crate::denormals::flushing()
    };
    let before = platform::current_priority();
    let promotion = if before.is_realtime() {
        Promotion::AlreadyRealtime
    } else if !config.promote {
        Promotion::NotRequested
    } else if raise_here {
        platform::promote(config.period, &mut held.platform)
    } else {
        Promotion::Pending
    };
    // After raising, so the thread is real-time before it joins.
    let workgroup = match &config.workgroup {
        None => WorkgroupJoin::None,
        Some(workgroup) => held.membership.join(workgroup),
    };
    ThreadReport {
        priority: if matches!(promotion, Promotion::Raised(_)) {
            platform::current_priority()
        } else {
            before
        },
        promotion,
        workgroup,
        denormals_flushed,
        trap_installed: crate::trap::is_installed(),
        sanitizer_active: crate::sanitize::ACTIVE,
    }
}

/// Sets up the calling thread, which the program started, for real-time work:
/// raises its priority if asked and needed, joins the workgroup, and reports
/// the result.
///
/// Call it once, before the thread's first real-time section and before any
/// deadline runs; calling it again on the same thread replaces the earlier
/// set-up. It may allocate, and on Linux it may ask rtkit over D-Bus, which
/// blocks, so it never runs on a thread an audio API owns: that's what
/// [`AudioThreadSetup`] is for. What it acquires is released when the thread
/// exits, or by [`release_current_thread`].
pub fn prepare_current_thread(config: &RtThreadConfig) -> ThreadReport {
    // Do every first-use cost of a real-time section now: the trap's
    // thread-locals (which macOS allocates on first touch), this thread's
    // log queue, the sanitizer's runtime, and the clock's timebase.
    serial();
    crate::trap::prepare_thread();
    crate::log::register_current_thread();
    crate::sanitize::ensure_initialized();
    platform::warm_up();

    HELD.with(|held| {
        let mut held = held.borrow_mut();
        let held = held.get_or_insert_with(|| HeldHere(OnThread::new()));
        // Leave a previous workgroup before joining another: workgroups must
        // be left in the reverse order they were joined.
        held.0.membership.leave();
        set_up_here(config, &mut held.0, true)
    })
}

/// Undoes what [`prepare_current_thread`] acquired on the calling thread:
/// leaves the workgroup and ends the MMCSS registration. The priority itself
/// stays. A thread the program owns calls this before it ends, because the
/// order in which a thread's clean-up runs at exit isn't promised.
pub fn release_current_thread() {
    let held = HELD
        .try_with(|held| held.borrow_mut().take())
        .ok()
        .flatten();
    drop(held);
}

/// Whether [`AudioThreadSetup::enter`] raises the thread itself. On Linux a
/// helper thread does, because rtkit is a D-Bus round trip.
const RAISE_ON_THE_AUDIO_THREAD: bool = !cfg!(target_os = "linux");

/// Real-time set-up for threads an audio API owns, which first run
/// the program's code inside a device callback.
///
/// [`new`](Self::new) runs off the audio thread, before the stream starts,
/// and allocates everything set-up will need: the workgroup token's storage,
/// the report channel, and on Linux a helper thread. The audio thread then
/// calls [`enter`](Self::enter) once, before its first block, and again only
/// if the backend moves the callback to another thread
/// ([`is_current_thread`](Self::is_current_thread) says when). `enter`
/// allocates nothing and nothing in it blocks; it does only what has to run
/// on the thread itself:
///
/// - macOS: reads the thread's policy, raises it with `thread_policy_set` if
///   it isn't real-time (Core Audio's I/O thread always is), and calls
///   `os_workgroup_join`, which applies to the calling thread. Core Audio's
///   I/O thread is already a member, so the join returns `EALREADY` there.
///   A join that succeeds hands its record, allocated in `new`, to a
///   thread-exit hook (`pthread_setspecific`, which stores it inside the
///   thread's own structure), because macOS stops a process whose thread
///   exits inside a workgroup, and the API may end the thread without
///   the program running on it again.
/// - Windows: reads the thread's priority and, if it isn't real-time,
///   registers it with MMCSS "Pro Audio" (`AvSetMmThreadCharacteristicsW`),
///   which applies to the calling thread.
/// - Linux: reads its scheduling policy and, if it needs raising, publishes
///   its thread ID through an atomic. A helper thread polls for it and raises
///   the thread by ID, with `sched_setscheduler` or, failing that, rtkit.
/// - Everywhere: checks that a real-time section flushes denormals. The
///   flushing itself happens for every block.
///
/// A backend's runner plays silence for the block whose callback did the
/// set-up, so set-up never shares a deadline with audio work.
pub struct AudioThreadSetup {
    config: RtThreadConfig,
    /// The serial of the thread set up last, or 0.
    thread: u64,
    setups: u64,
    held: OnThread,
    report: crate::triple::Input<Option<ThreadReport>>,
    #[cfg(target_os = "linux")]
    helper: Option<Arc<Helper>>,
}

impl fmt::Debug for AudioThreadSetup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioThreadSetup")
            .field("config", &self.config)
            .field("thread", &self.thread)
            .field("setups", &self.setups)
            .finish_non_exhaustive()
    }
}

impl AudioThreadSetup {
    /// Prepares set-up for threads that will run with `config`, and returns
    /// the reader for its reports. Call it off the audio thread: it
    /// allocates and, on Linux, may start a helper thread.
    pub fn new(config: RtThreadConfig) -> (Self, ThreadReports) {
        crate::sanitize::ensure_initialized();
        platform::warm_up();
        let (input, output) = crate::triple::triple_buffer(&None);
        #[cfg(target_os = "linux")]
        let helper = config.promote.then(Helper::spawn).flatten();
        let reports = ThreadReports {
            latest: output,
            #[cfg(target_os = "linux")]
            helper: helper.clone(),
        };
        let setup = Self {
            config,
            thread: 0,
            setups: 0,
            held: OnThread::new(),
            report: input,
            #[cfg(target_os = "linux")]
            helper,
        };
        (setup, reports)
    }

    /// Whether the calling thread is the one set up last. Real-time safe.
    pub fn is_current_thread(&self) -> bool {
        self.thread != 0 && self.thread == serial()
    }

    /// How many times [`enter`](Self::enter) has run.
    pub fn setups(&self) -> u64 {
        self.setups
    }

    /// Sets up the calling thread, publishes the report, and returns it. Run
    /// it on the audio thread before its first block; it allocates nothing
    /// and doesn't block (see the type's documentation for what it does on
    /// each platform).
    ///
    /// What an earlier set-up acquired on another thread (a workgroup
    /// membership, an MMCSS registration) stays with that thread until it
    /// exits, because only that thread could undo it.
    pub fn enter(&mut self) -> ThreadReport {
        let thread = serial();
        if thread == self.thread {
            self.held.membership.leave();
        } else {
            self.held.forget();
        }
        self.thread = thread;
        self.setups += 1;
        #[allow(unused_mut)]
        let mut report = set_up_here(&self.config, &mut self.held, RAISE_ON_THE_AUDIO_THREAD);
        #[cfg(target_os = "linux")]
        if report.promotion == Promotion::Pending {
            match &self.helper {
                Some(helper) => helper.request(self.setups, platform::current_thread_id()),
                None => report.promotion = Promotion::Refused(Method::Rtkit, Refusal::NoHelper),
            }
        }
        self.report.write(Some(report));
        report
    }

    /// Undoes [`enter`](Self::enter) on the calling thread, if it's the one
    /// set up last: leaves the workgroup and ends the MMCSS registration. For
    /// a thread the backend owns and is about to end; a thread the audio API
    /// owns keeps its set-up until it exits.
    pub fn release(&mut self) {
        if self.is_current_thread() {
            self.held.release();
            self.thread = 0;
        }
    }
}

impl Drop for AudioThreadSetup {
    fn drop(&mut self) {
        // On the thread set up last (a backend's own thread unwinding from
        // a panicking callback, say), undo the set-up; elsewhere only that
        // thread could, and its workgroup exit hook will.
        if self.is_current_thread() {
            self.held.release();
        } else {
            self.held.forget();
        }
        #[cfg(target_os = "linux")]
        if let Some(helper) = &self.helper {
            helper.stop.store(true, Ordering::Release);
        }
    }
}

/// The reading end of an [`AudioThreadSetup`]'s reports, for whoever reads a
/// stream's statistics. Not for the audio thread.
pub struct ThreadReports {
    latest: crate::triple::Output<Option<ThreadReport>>,
    #[cfg(target_os = "linux")]
    helper: Option<Arc<Helper>>,
}

impl fmt::Debug for ThreadReports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadReports").finish_non_exhaustive()
    }
}

impl ThreadReports {
    /// The latest thread's report, or `None` before the first set-up. On
    /// Linux it includes the helper thread's answer once there is one.
    pub fn latest(&mut self) -> Option<ThreadReport> {
        #[allow(unused_mut)]
        let mut report = (*self.latest.read())?;
        #[cfg(target_os = "linux")]
        if report.promotion == Promotion::Pending
            && let Some(helper) = &self.helper
            && let Some((promotion, priority)) = helper.answer()
        {
            report.promotion = promotion;
            report.priority = priority;
        }
        Some(report)
    }
}

/// Linux: the thread that raises audio threads, and what it shares with them.
///
/// The audio thread stores a request (the set-up's number and its thread ID)
/// in an atomic, which is all it does. The helper looks every
/// [`POLL`](Helper::POLL), raises the thread by ID (which may mean a D-Bus
/// round trip to rtkit), and keeps the answer for [`ThreadReports`].
#[cfg(target_os = "linux")]
struct Helper {
    /// `(set-up number << 32) | thread ID` of the latest request, or 0.
    request: AtomicU64,
    /// The latest answer, with the request it answers.
    answer: Mutex<Option<(u64, Promotion, Priority)>>,
    stop: AtomicBool,
}

#[cfg(target_os = "linux")]
impl Helper {
    /// How often the helper looks for a request: the longest an audio thread
    /// runs at normal priority after its first block, plus the time rtkit
    /// takes.
    const POLL: Duration = Duration::from_millis(10);

    /// Starts the helper thread, or returns `None` if it couldn't start.
    fn spawn() -> Option<Arc<Self>> {
        let helper = Arc::new(Self {
            request: AtomicU64::new(0),
            answer: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let shared = Arc::clone(&helper);
        std::thread::Builder::new()
            .name("lspk-rt-raise".into())
            .spawn(move || shared.run())
            .ok()
            .map(|_| helper)
    }

    /// Called on the audio thread: one atomic store.
    fn request(&self, setup: u64, tid: libc::pid_t) {
        let tid = u64::from(tid.unsigned_abs());
        self.request.store((setup << 32) | tid, Ordering::Release);
    }

    fn run(&self) {
        let mut handled = 0;
        while !self.stop.load(Ordering::Acquire) {
            let request = self.request.load(Ordering::Acquire);
            if request != handled {
                handled = request;
                // The low 32 bits are the thread ID, which Linux keeps below
                // 2^22.
                let tid = (request & 0xFFFF_FFFF) as libc::pid_t;
                let promotion = platform::promote_thread(tid);
                let priority = platform::priority_of(tid);
                *self.answer.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((request, promotion, priority));
            }
            std::thread::park_timeout(Self::POLL);
        }
    }

    /// The answer to the latest request, if the helper has given it.
    fn answer(&self) -> Option<(Promotion, Priority)> {
        let answer = *self.answer.lock().unwrap_or_else(PoisonError::into_inner);
        answer
            .filter(|&(request, ..)| request == self.request.load(Ordering::Acquire))
            .map(|(_, promotion, priority)| (promotion, priority))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serials_are_unique_per_thread_and_stable_within_one() {
        let here = serial();
        assert_ne!(here, 0);
        assert_eq!(serial(), here);
        let there = std::thread::spawn(serial).join().unwrap();
        assert_ne!(there, here);
        let again = std::thread::spawn(serial).join().unwrap();
        assert_ne!(again, there, "an exited thread's serial isn't reused");
    }

    #[test]
    fn a_test_thread_starts_with_normal_priority() {
        let priority = current_priority();
        assert!(!priority.is_realtime(), "{priority}");
    }

    #[test]
    fn set_up_without_promotion_changes_nothing() {
        let report = std::thread::spawn(|| {
            prepare_current_thread(&RtThreadConfig {
                period: Duration::from_millis(5),
                promote: false,
                workgroup: None,
            })
        })
        .join()
        .unwrap();
        assert_eq!(report.promotion, Promotion::NotRequested);
        assert!(!report.priority.is_realtime());
        assert_eq!(report.workgroup, WorkgroupJoin::None);
        assert_eq!(report.denormals_flushed, crate::denormals::SUPPORTED);
        assert!(report.trap_installed);
    }

    /// Raising a thread's priority works without special rights on macOS and
    /// Windows. Linux needs rtprio limits or rtkit, which CI checks.
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn set_up_raises_a_thread_to_real_time() {
        let (report, after_exit) = std::thread::spawn(|| {
            let report = prepare_current_thread(&RtThreadConfig {
                period: Duration::from_micros(2_667),
                promote: true,
                workgroup: None,
            });
            (report, current_priority())
        })
        .join()
        .unwrap();
        assert!(
            matches!(report.promotion, Promotion::Raised(_)),
            "{}",
            report.promotion
        );
        assert!(report.priority.is_realtime(), "{}", report.priority);
        assert!(after_exit.is_realtime());
        #[cfg(target_os = "macos")]
        assert!(
            matches!(report.priority, Priority::TimeConstraint { period, .. }
                if period.abs_diff(Duration::from_micros(2_667)) < Duration::from_micros(1)),
            "{}",
            report.priority
        );
        #[cfg(windows)]
        assert!(matches!(
            report.priority,
            Priority::Mmcss {
                task: "Pro Audio",
                ..
            }
        ));
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn a_thread_that_is_already_real_time_is_left_alone() {
        let second = std::thread::spawn(|| {
            let config = RtThreadConfig {
                period: Duration::from_millis(10),
                promote: true,
                workgroup: None,
            };
            prepare_current_thread(&config);
            prepare_current_thread(&config)
        })
        .join()
        .unwrap();
        assert_eq!(second.promotion, Promotion::AlreadyRealtime);
        assert!(second.priority.is_realtime());
    }

    /// What set-up on an audio API's thread asks for in the tests: real-time
    /// priority and, on macOS, a workgroup the thread really joins.
    fn audio_thread_config() -> RtThreadConfig {
        RtThreadConfig {
            period: Duration::from_micros(2_667),
            promote: true,
            #[cfg(target_os = "macos")]
            workgroup: Some(
                crate::WorkInterval::new("lspk-rt audio thread test")
                    .unwrap()
                    .workgroup()
                    .clone(),
            ),
            #[cfg(not(target_os = "macos"))]
            workgroup: None,
        }
    }

    /// Waits for a Linux helper's answer; elsewhere returns the report as is.
    fn settled(reports: &mut ThreadReports) -> ThreadReport {
        let began = std::time::Instant::now();
        loop {
            let report = reports.latest().expect("a report");
            if report.promotion != Promotion::Pending {
                return report;
            }
            assert!(
                began.elapsed() < Duration::from_secs(10),
                "the helper never answered"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The finding this type answers: set-up on an audio API's thread must
    /// not allocate. Each thread's set-up runs inside a forbidden section of
    /// the allocation trap, including a second thread's, which forgets the
    /// first thread's workgroup membership.
    #[test]
    fn set_up_on_an_audio_thread_allocates_nothing() {
        let (mut setup, mut reports) = AudioThreadSetup::new(audio_thread_config());
        assert_eq!(reports.latest(), None);
        for round in 1..=2 {
            let (back, report, heap) = std::thread::spawn(move || {
                let (report, heap) = crate::trap::watch(|| crate::trap::forbid(|| setup.enter()));
                (setup, report, heap.map(|heap| heap.to_string()))
            })
            .join()
            .unwrap();
            setup = back;
            assert_eq!(heap, None, "set-up {round} touched the heap");
            assert_eq!(setup.setups(), round);
            assert_eq!(report.denormals_flushed, crate::denormals::SUPPORTED);
            #[cfg(target_os = "macos")]
            {
                assert_eq!(report.promotion, Promotion::Raised(Method::TimeConstraint));
                assert_eq!(report.workgroup, WorkgroupJoin::Joined);
            }
            #[cfg(windows)]
            assert_eq!(report.promotion, Promotion::Raised(Method::Mmcss));
            #[cfg(target_os = "linux")]
            assert_eq!(report.promotion, Promotion::Pending);
        }
        // Linux CI machines may lack rtprio limits and rtkit, so the answer
        // may be a refusal, but there is one.
        let settled = settled(&mut reports);
        assert_ne!(settled.promotion, Promotion::Pending);
        #[cfg(any(target_os = "macos", windows))]
        assert!(settled.priority.is_realtime(), "{}", settled.priority);
    }

    #[test]
    fn set_up_runs_once_per_thread() {
        let (mut setup, _reports) = AudioThreadSetup::new(RtThreadConfig {
            period: Duration::from_millis(5),
            promote: false,
            workgroup: None,
        });
        assert!(!setup.is_current_thread());
        let setup = std::thread::spawn(move || {
            setup.enter();
            assert!(setup.is_current_thread());
            setup
        })
        .join()
        .unwrap();
        assert!(!setup.is_current_thread(), "another thread needs its own");
        assert_eq!(setup.setups(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn release_leaves_the_workgroup_on_the_thread_that_joined() {
        let interval = crate::WorkInterval::new("lspk-rt release test").unwrap();
        let (mut setup, _reports) = AudioThreadSetup::new(RtThreadConfig {
            period: Duration::from_millis(5),
            promote: false,
            workgroup: Some(interval.workgroup().clone()),
        });
        std::thread::spawn(move || {
            assert_eq!(setup.enter().workgroup, WorkgroupJoin::Joined);
            assert!(interval.workgroup().is_joined_here());
            interval.start(Duration::from_millis(5)).unwrap();
            interval.finish().unwrap();
            setup.release();
            assert!(!interval.workgroup().is_joined_here());
            assert!(!setup.is_current_thread());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn priorities_describe_themselves() {
        let tc = Priority::TimeConstraint {
            period: Duration::from_micros(2_667),
            computation: Duration::from_micros(1_000),
            constraint: Duration::from_micros(2_667),
        };
        assert_eq!(
            tc.to_string(),
            "real-time, Mach time-constraint policy (period 2.667 ms, computation 1.000 ms, constraint 2.667 ms)"
        );
        assert_eq!(
            Priority::Normal { level: 31 }.to_string(),
            "normal (priority 31)"
        );
        assert!(Priority::TimeCritical.is_realtime());
        assert_eq!(
            Priority::TimeCritical.to_string(),
            "real-time, THREAD_PRIORITY_TIME_CRITICAL"
        );
        assert!(Priority::RealtimeClass { level: 0 }.is_realtime());
        assert_eq!(
            Promotion::Refused(Method::Rtkit, Refusal::NoRtkit).to_string(),
            "refused: rtkit failed with rtkit isn't running"
        );
        assert_eq!(
            Promotion::Pending.to_string(),
            "requested; a helper thread is raising it"
        );
    }
}
