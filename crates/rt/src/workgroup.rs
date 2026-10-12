//! macOS audio workgroups (`os_workgroup`), through the kit's own bindings.
//!
//! A workgroup tells the macOS scheduler which real-time threads work to the
//! same deadline, so it can keep them on performance cores and wake them
//! together. Core Audio makes one per audio device
//! (`kAudioDevicePropertyIOThreadOSWorkgroup`) and puts the device's I/O
//! thread in it. Any other thread that works to that device's deadline, such
//! as the program's real-time worker pool, joins it with [`Workgroup::join`].
//! Threads that keep their own time, such as the null device's, use a
//! [`WorkInterval`] instead.
//!
//! As of October 2026 no published crate binds these calls, so they're
//! declared in `platform/macos.rs`. On other platforms every constructor
//! returns an error and no [`Workgroup`] can exist.

// Elsewhere the raw types are uninhabited, so the compiler knows code after
// a platform call can't run; it's still written for macOS.
#![cfg_attr(not(target_os = "macos"), allow(unreachable_code, unused_variables))]

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::platform;
use crate::thread::serial;

/// Whether a thread is in its workgroup, after set-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkgroupJoin {
    /// There was no workgroup to join: the platform has none, or the device
    /// doesn't offer one.
    #[default]
    None,
    /// The thread joined it.
    Joined,
    /// The thread already belongs to a workgroup this one doesn't nest with
    /// (`EALREADY`). Core Audio's I/O thread is a member of its device's
    /// workgroup from the start, so this is the expected answer there.
    AlreadyMember,
    /// The join failed with this `errno`; `EINVAL` means the workgroup was
    /// cancelled, for example because its device went away.
    Failed(i32),
}

impl fmt::Display for WorkgroupJoin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Joined => f.write_str("joined"),
            Self::AlreadyMember => f.write_str("already a member"),
            Self::Failed(code) => write!(f, "join failed with errno {code}"),
        }
    }
}

/// Why a workgroup couldn't be found or made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkgroupError {
    message: String,
}

impl WorkgroupError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for WorkgroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WorkgroupError {}

/// An audio workgroup. Cloning is cheap and allocation-free: clones share
/// one retained OS object, which is released when the last clone drops.
#[derive(Clone)]
pub struct Workgroup {
    raw: platform::RawWorkgroup,
    label: Arc<str>,
    members: Arc<Members>,
}

impl fmt::Debug for Workgroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Workgroup")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl Workgroup {
    /// The workgroup of the I/O thread of the Core Audio device with this UID
    /// (`kAudioDevicePropertyDeviceUID`, which is what `cpal` calls a Core
    /// Audio device's ID).
    pub fn of_core_audio_device(uid: &str) -> Result<Self, WorkgroupError> {
        let (raw, name) = platform::device_workgroup(Some(uid))?;
        Ok(Self::new(raw, format!("the I/O workgroup of {name}")))
    }

    /// The workgroup of the system default output device's I/O thread.
    pub fn of_default_output_device() -> Result<Self, WorkgroupError> {
        let (raw, name) = platform::device_workgroup(None)?;
        Ok(Self::new(raw, format!("the I/O workgroup of {name}")))
    }

    fn new(raw: platform::RawWorkgroup, label: String) -> Self {
        Self {
            raw,
            label: label.into(),
            members: Arc::new(Members::default()),
        }
    }

    /// Where this workgroup comes from, for a person to read: for example
    /// "the I/O workgroup of MacBook Pro Speakers".
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The operating system's own description of the workgroup object, which
    /// includes its name.
    pub fn description(&self) -> String {
        platform::workgroup_description(&self.raw)
    }

    /// How many threads the system suggests running in parallel in this
    /// workgroup, which is how big a real-time worker pool should be.
    pub fn max_parallel_threads(&self) -> Option<u32> {
        platform::workgroup_max_parallel_threads(&self.raw)
    }

    /// Joins the calling thread to this workgroup until the token drops. The
    /// thread should already have real-time priority.
    ///
    /// It allocates the token, so it's for threads the program starts, during
    /// their set-up. A thread an audio API owns joins through
    /// [`AudioThreadSetup`](crate::AudioThreadSetup), which allocates the
    /// token beforehand.
    pub fn join(&self) -> Result<JoinToken, WorkgroupJoin> {
        let mut token = Box::new(platform::Token::new());
        self.join_with(&mut token)?;
        Ok(JoinToken {
            workgroup: self.clone(),
            token: Some(token),
            _not_send: std::marker::PhantomData,
        })
    }

    /// Joins the calling thread, keeping the token in `token`. Allocation-free.
    fn join_with(&self, token: &mut platform::Token) -> Result<(), WorkgroupJoin> {
        platform::workgroup_join(&self.raw, token).map_err(|code| match code {
            libc_errno::EALREADY => WorkgroupJoin::AlreadyMember,
            code => WorkgroupJoin::Failed(code),
        })?;
        self.members.add(serial());
        Ok(())
    }

    /// Leaves, on the thread that joined with `token`. Allocation-free.
    fn leave_with(&self, token: &mut platform::Token) {
        platform::workgroup_leave(&self.raw, token);
        self.members.remove(serial());
    }

    /// Whether the calling thread joined this workgroup through this crate and
    /// hasn't left. Real-time safe.
    ///
    /// The workgroup keeps track of its members itself rather than in a
    /// thread-local, because macOS allocates a thread's thread-local storage
    /// the first time the thread touches it.
    pub fn is_joined_here(&self) -> bool {
        self.members.contains(serial())
    }
}

/// How many member threads a workgroup keeps track of for
/// [`Workgroup::is_joined_here`]. More may join; they just aren't tracked, so
/// [`WorkInterval::start`] refuses them. A real-time worker pool is sized by
/// [`Workgroup::max_parallel_threads`], which is far below this.
const TRACKED_MEMBERS: usize = 64;

/// The threads, by [serial](crate::thread::serial), that joined a workgroup
/// through this crate and haven't left. A workgroup's clones share it, and
/// every operation is a few atomic instructions.
struct Members([AtomicU64; TRACKED_MEMBERS]);

impl Default for Members {
    fn default() -> Self {
        Self(std::array::from_fn(|_| AtomicU64::new(0)))
    }
}

impl Members {
    fn add(&self, thread: u64) {
        for slot in &self.0 {
            if slot
                .compare_exchange(0, thread, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
    }

    fn remove(&self, thread: u64) {
        for slot in &self.0 {
            if slot
                .compare_exchange(thread, 0, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
    }

    fn contains(&self, thread: u64) -> bool {
        thread != 0
            && self
                .0
                .iter()
                .any(|slot| slot.load(Ordering::Acquire) == thread)
    }
}

/// `errno` values, which are the same on every platform that has workgroups.
mod libc_errno {
    pub(super) const EPERM: i32 = 1;
    pub(super) const ENOMEM: i32 = 12;
    pub(super) const EAGAIN: i32 = 35;
    pub(super) const EALREADY: i32 = 37;
}

/// Membership of a workgroup for the thread that joined. Dropping it leaves
/// the workgroup, which must happen on the same thread, so it isn't `Send`.
pub struct JoinToken {
    workgroup: Workgroup,
    token: Option<Box<platform::Token>>,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl fmt::Debug for JoinToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinToken")
            .field("workgroup", &self.workgroup)
            .finish_non_exhaustive()
    }
}

impl Drop for JoinToken {
    fn drop(&mut self) {
        if let Some(mut token) = self.token.take() {
            self.workgroup.leave_with(&mut token);
        }
    }
}

/// What a thread needs to leave a workgroup it joined: the workgroup and the
/// join's token. Allocated ahead of time, and held by the thread's exit hook
/// while the thread is a member.
struct JoinRecord {
    workgroup: Option<Workgroup>,
    token: platform::Token,
}

impl JoinRecord {
    /// Leaves the workgroup this record holds, if any. Allocation-free.
    fn leave(&mut self) {
        if let Some(workgroup) = self.workgroup.take() {
            workgroup.leave_with(&mut self.token);
        }
    }
}

/// The exit hook's destructor: runs on a thread as it exits while still a
/// member, and leaves. macOS stops the process (`SIGTRAP`) if a thread exits
/// inside a workgroup, and a thread an audio API owns ends when the API
/// decides, without telling the program.
unsafe extern "C" fn leave_at_exit(record: *mut std::ffi::c_void) {
    // SAFETY: the hook holds only records that `Membership::join` handed over
    // with `Box::into_raw`, and libpthread calls this once per value.
    let mut record = unsafe { Box::from_raw(record.cast::<JoinRecord>()) };
    record.leave();
}

/// A thread's membership of one workgroup, for set-up on a thread an audio
/// API owns: everything it needs is allocated in [`new`](Self::new), so
/// joining, leaving, and forgetting are allocation-free.
///
/// A successful join hands its record to the thread's exit hook (a pthread
/// key), so the thread leaves when it exits even if the program never runs on
/// it again. [`leave`](Self::leave) takes the record back. Unlike a
/// [`JoinToken`], dropping a membership doesn't leave: it may drop on another
/// thread, and only the thread that joined may leave.
pub(crate) struct Membership {
    /// Records ready for a join, allocated up front. A record goes to a
    /// thread's exit hook when it joins and comes back when it leaves.
    #[allow(
        clippy::vec_box,
        reason = "each record is handed to a thread as a pointer and may outlive this Vec"
    )]
    spare: Vec<Box<JoinRecord>>,
    /// The serial of the thread that joined through this membership.
    joined_on: Option<u64>,
}

impl Membership {
    /// How many records a membership allocates. A join made on a thread that
    /// then moved on without leaving keeps its record until that thread
    /// exits, so a few are kept in reserve.
    const RECORDS: usize = 4;

    /// Allocates the records, and the process's exit hook if it isn't made
    /// yet. Where there are no workgroups there's no hook, and no records.
    pub(crate) fn new() -> Self {
        let records = if platform::init_exit_hook(leave_at_exit) {
            Self::RECORDS
        } else {
            0
        };
        let mut spare = Vec::with_capacity(records);
        spare.extend((0..records).map(|_| {
            Box::new(JoinRecord {
                workgroup: None,
                token: platform::Token::new(),
            })
        }));
        Self {
            spare,
            joined_on: None,
        }
    }

    /// Joins the calling thread to `workgroup`. An earlier join must have
    /// been left or forgotten. Fails with `EAGAIN` if the exit hook couldn't
    /// be made, and `ENOMEM` if every record is held by a thread that joined
    /// and moved on.
    pub(crate) fn join(&mut self, workgroup: &Workgroup) -> WorkgroupJoin {
        debug_assert!(self.joined_on.is_none(), "leave or forget the earlier join");
        let Some(mut record) = self.spare.pop() else {
            return WorkgroupJoin::Failed(if self.spare.capacity() == 0 {
                libc_errno::EAGAIN
            } else {
                libc_errno::ENOMEM
            });
        };
        match workgroup.join_with(&mut record.token) {
            Ok(()) => {
                record.workgroup = Some(workgroup.clone());
                platform::set_exit_hook(Box::into_raw(record).cast());
                self.joined_on = Some(serial());
                WorkgroupJoin::Joined
            }
            Err(outcome) => {
                self.spare.push(record);
                outcome
            }
        }
    }

    /// Leaves the workgroup, if the calling thread joined through this
    /// membership.
    pub(crate) fn leave(&mut self) {
        if self.joined_on != Some(serial()) {
            return;
        }
        self.joined_on = None;
        let record = platform::take_exit_hook();
        if !record.is_null() {
            // SAFETY: the hook held a record `join` handed over on this
            // thread, and taking it back means its destructor won't run.
            let mut record = unsafe { Box::from_raw(record.cast::<JoinRecord>()) };
            record.leave();
            // Within the capacity it came from, so this doesn't allocate.
            self.spare.push(record);
        }
    }

    /// Forgets a join that another thread made. That thread stays a member,
    /// and its exit hook leaves when it exits; only it could leave sooner.
    pub(crate) fn forget(&mut self) {
        self.joined_on = None;
    }
}

/// A workgroup with its own deadlines, for real-time threads that keep their
/// own time rather than a device's (Apple's `AudioWorkIntervalCreate`).
///
/// The thread that drives the work joins [`workgroup`](Self::workgroup), then
/// brackets each cycle with [`start`](Self::start) and
/// [`finish`](Self::finish).
#[derive(Debug, Clone)]
pub struct WorkInterval {
    workgroup: Workgroup,
}

impl WorkInterval {
    /// A new work interval called `name`.
    pub fn new(name: &str) -> Result<Self, WorkgroupError> {
        let raw = platform::work_interval_create(name)?;
        Ok(Self {
            workgroup: Workgroup::new(raw, format!("the work interval \"{name}\"")),
        })
    }

    /// The interval's workgroup, which its threads join.
    pub fn workgroup(&self) -> &Workgroup {
        &self.workgroup
    }

    /// Tells the system a cycle starts now and must finish within `length`.
    /// Real-time safe.
    ///
    /// The calling thread must have joined the workgroup. If it hasn't, this
    /// returns `EPERM` without asking the system, because macOS stops the
    /// process rather than return the error its header documents.
    pub fn start(&self, length: Duration) -> Result<(), i32> {
        if !self.workgroup.is_joined_here() {
            return Err(libc_errno::EPERM);
        }
        platform::work_interval_start(&self.workgroup.raw, length)
    }

    /// Tells the system the current cycle is done. Real-time safe. Like
    /// [`start`](Self::start), it needs the calling thread to be a member.
    pub fn finish(&self) -> Result<(), i32> {
        if !self.workgroup.is_joined_here() {
            return Err(libc_errno::EPERM);
        }
        platform::work_interval_finish(&self.workgroup.raw)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::thread::{Promotion, RtThreadConfig, prepare_current_thread};

    #[test]
    fn a_work_interval_can_be_joined_started_and_left() {
        let interval = WorkInterval::new("lspk-rt test").unwrap();
        assert!(interval.workgroup().label().contains("lspk-rt test"));
        let description = interval.workgroup().description();
        assert!(!description.is_empty());
        let max = interval.workgroup().max_parallel_threads();
        assert!(max.is_some_and(|n| n >= 1), "{max:?}");

        let worker = interval.clone();
        std::thread::spawn(move || {
            let report = prepare_current_thread(&RtThreadConfig {
                period: Duration::from_millis(5),
                promote: true,
                workgroup: Some(worker.workgroup().clone()),
            });
            assert!(matches!(report.promotion, Promotion::Raised(_)));
            assert_eq!(report.workgroup, WorkgroupJoin::Joined);
            for _ in 0..3 {
                worker.start(Duration::from_millis(5)).unwrap();
                worker.finish().unwrap();
            }
            crate::thread::release_current_thread();
            assert!(!worker.workgroup().is_joined_here());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn a_thread_cannot_be_in_two_workgroups_that_do_not_nest() {
        let first = WorkInterval::new("first").unwrap();
        let second = WorkInterval::new("second").unwrap();
        std::thread::spawn(move || {
            let token = first.workgroup().join().unwrap();
            let again = second.workgroup().join().map(drop);
            assert_eq!(again, Err(WorkgroupJoin::AlreadyMember));
            drop(token);
            // Having left the first, the thread can join the second.
            second.workgroup().join().unwrap();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn starting_an_interval_needs_membership() {
        let interval = WorkInterval::new("not joined").unwrap();
        let five_ms = Duration::from_millis(5);
        assert_eq!(interval.start(five_ms), Err(libc_errno::EPERM));
        assert_eq!(interval.finish(), Err(libc_errno::EPERM));
        std::thread::spawn(move || {
            let token = interval.workgroup().join().unwrap();
            assert!(interval.workgroup().is_joined_here());
            interval.start(five_ms).unwrap();
            interval.finish().unwrap();
            drop(token);
            assert!(!interval.workgroup().is_joined_here());
            assert_eq!(interval.start(five_ms), Err(libc_errno::EPERM));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn membership_is_per_thread() {
        let interval = WorkInterval::new("per thread").unwrap();
        let elsewhere = interval.clone();
        std::thread::spawn(move || {
            let _token = elsewhere.workgroup().join().unwrap();
            assert!(elsewhere.workgroup().is_joined_here());
            let other = elsewhere.clone();
            let there = std::thread::spawn(move || other.workgroup().is_joined_here())
                .join()
                .unwrap();
            assert!(!there, "another thread isn't a member");
        })
        .join()
        .unwrap();
        assert!(!interval.workgroup().is_joined_here());
    }

    /// macOS stops a process whose thread exits inside a workgroup, so a
    /// thread that joined through a membership and never ran the program's
    /// code again must still leave: its exit hook does.
    #[test]
    fn a_thread_that_exits_while_joined_leaves_through_its_exit_hook() {
        let interval = WorkInterval::new("exit hook").unwrap();
        let mut membership = Membership::new();
        let watcher = interval.clone();
        let (membership, thread) = std::thread::spawn(move || {
            assert_eq!(membership.join(interval.workgroup()), WorkgroupJoin::Joined);
            (membership, serial())
        })
        .join()
        .unwrap();
        assert!(
            !watcher.workgroup().members.contains(thread),
            "the exit hook left on the way out"
        );
        assert_eq!(membership.spare.len(), Membership::RECORDS - 1);
    }

    #[test]
    fn a_membership_joins_and_leaves_without_allocating() {
        let interval = WorkInterval::new("membership").unwrap();
        std::thread::spawn(move || {
            let mut membership = Membership::new();
            let ((joined, started), report) = crate::trap::watch(|| {
                crate::trap::forbid(|| {
                    let joined = membership.join(interval.workgroup());
                    let started = interval.start(Duration::from_millis(5));
                    let _ = interval.finish();
                    membership.leave();
                    (joined, started)
                })
            });
            assert_eq!(joined, WorkgroupJoin::Joined);
            assert_eq!(started, Ok(()));
            assert!(report.is_none(), "{}", report.unwrap());
            assert!(!interval.workgroup().is_joined_here());
        })
        .join()
        .unwrap();
    }

    /// Needs an output device; Mac CI runners may have none, so the test
    /// says so and passes rather than failing.
    #[test]
    fn the_default_output_device_has_a_workgroup() {
        match Workgroup::of_default_output_device() {
            Ok(workgroup) => {
                assert!(workgroup.label().starts_with("the I/O workgroup of "));
                let joined = std::thread::spawn(move || workgroup.join().map(drop))
                    .join()
                    .unwrap();
                assert_eq!(
                    joined,
                    Ok(()),
                    "a fresh thread can join a device's workgroup"
                );
            }
            Err(error) => eprintln!("skipped: {error}"),
        }
    }

    #[test]
    fn an_unknown_device_uid_is_an_error() {
        let error = Workgroup::of_core_audio_device("no-such-device-uid").unwrap_err();
        assert!(error.to_string().contains("no-such-device-uid"), "{error}");
    }
}
