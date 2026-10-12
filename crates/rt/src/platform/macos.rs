//! macOS: the Mach time-constraint policy, and audio workgroups through
//! The kit's own bindings to `os_workgroup` and Core Audio.

use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr::{self, NonNull};
use std::sync::OnceLock;
use std::time::Duration;

use crate::thread::{Method, Priority, Promotion, Refusal};
use crate::workgroup::WorkgroupError;

/// Declarations from the macOS SDK that no crate in the dependency tree
/// covers: Core Audio's property call, a little of Core Foundation,
/// `os_workgroup`, and two Mach clock calls that `libc` has deprecated.
#[allow(non_snake_case, non_upper_case_globals, non_camel_case_types)]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    pub(super) type AudioObjectID = u32;
    pub(super) type OSStatus = i32;

    #[repr(C)]
    pub(super) struct AudioObjectPropertyAddress {
        pub(super) mSelector: u32,
        pub(super) mScope: u32,
        pub(super) mElement: u32,
    }

    const fn fourcc(code: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*code)
    }

    pub(super) const kAudioObjectSystemObject: AudioObjectID = 1;
    pub(super) const kAudioObjectUnknown: AudioObjectID = 0;
    pub(super) const kAudioObjectPropertyScopeGlobal: u32 = fourcc(b"glob");
    pub(super) const kAudioObjectPropertyElementMain: u32 = 0;
    pub(super) const kAudioHardwarePropertyDefaultOutputDevice: u32 = fourcc(b"dOut");
    pub(super) const kAudioHardwarePropertyTranslateUIDToDevice: u32 = fourcc(b"uidd");
    pub(super) const kAudioDevicePropertyIOThreadOSWorkgroup: u32 = fourcc(b"oswg");
    pub(super) const kAudioObjectPropertyName: u32 = fourcc(b"lnam");

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn AudioObjectGetPropertyData(
            inObjectID: AudioObjectID,
            inAddress: *const AudioObjectPropertyAddress,
            inQualifierDataSize: u32,
            inQualifierData: *const c_void,
            ioDataSize: *mut u32,
            outData: *mut c_void,
        ) -> OSStatus;
    }

    pub(super) type CFStringRef = *const c_void;
    pub(super) const kCFStringEncodingUTF8: u32 = 0x0800_0100;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            numBytes: isize,
            encoding: u32,
            isExternalRepresentation: u8,
        ) -> CFStringRef;
        pub(super) fn CFStringGetLength(string: CFStringRef) -> isize;
        pub(super) fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
        pub(super) fn CFStringGetCString(
            string: CFStringRef,
            buffer: *mut c_char,
            bufferSize: isize,
            encoding: u32,
        ) -> u8;
        pub(super) fn CFRelease(cf: *const c_void);
    }

    /// `os_workgroup_t` and its subclasses.
    pub(super) type os_workgroup_t = *mut c_void;

    /// `struct os_workgroup_join_token_opaque_s` on 64-bit targets.
    #[repr(C)]
    pub(super) struct os_workgroup_join_token_s {
        pub(super) sig: u32,
        pub(super) opaque: [u8; 36],
    }

    pub(super) const OS_CLOCK_MACH_ABSOLUTE_TIME: u32 = 32;

    #[repr(C)]
    #[derive(Default)]
    pub(super) struct mach_timebase_info {
        pub(super) numer: u32,
        pub(super) denom: u32,
    }

    // In libSystem, which every program links.
    unsafe extern "C" {
        pub(super) fn os_workgroup_join(
            wg: os_workgroup_t,
            token_out: *mut os_workgroup_join_token_s,
        ) -> c_int;
        pub(super) fn os_workgroup_leave(wg: os_workgroup_t, token: *mut os_workgroup_join_token_s);
        pub(super) fn os_workgroup_max_parallel_threads(
            wg: os_workgroup_t,
            attr: *mut c_void,
        ) -> c_int;
        pub(super) fn os_workgroup_interval_start(
            wg: os_workgroup_t,
            start: u64,
            deadline: u64,
            data: *mut c_void,
        ) -> c_int;
        pub(super) fn os_workgroup_interval_finish(wg: os_workgroup_t, data: *mut c_void) -> c_int;
        pub(super) fn os_retain(object: *mut c_void) -> *mut c_void;
        pub(super) fn os_release(object: *mut c_void);
        pub(super) fn mach_absolute_time() -> u64;
        pub(super) fn mach_timebase_info(info: *mut mach_timebase_info) -> c_int;
    }

    #[link(name = "AudioToolbox", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn AudioWorkIntervalCreate(
            name: *const c_char,
            clock: u32,
            attr: *mut c_void,
        ) -> os_workgroup_t;
    }

    // The Objective-C runtime: workgroups are Objective-C objects, and their
    // `debugDescription` is the only public way to read a workgroup's name.
    #[link(name = "objc")]
    unsafe extern "C" {
        pub(super) fn objc_msgSend();
        pub(super) fn sel_registerName(name: *const c_char) -> *const c_void;
        pub(super) fn objc_autoreleasePoolPush() -> *mut c_void;
        pub(super) fn objc_autoreleasePoolPop(pool: *mut c_void);
    }
}

// ---------------------------------------------------------------------------
// Threads and priority

/// The kernel's ID for the calling thread. It's 64 bits and never reused
/// while the system runs. Reading it touches no thread-local storage, which
/// macOS allocates lazily on a thread's first use, so it's safe on a thread
/// an audio API owns from its first instruction.
pub(crate) fn thread_id() -> u64 {
    let mut id = 0u64;
    // SAFETY: a null thread means the calling thread, and `id` is writable.
    // It reads the ID libpthread stored when the thread started.
    unsafe { libc::pthread_threadid_np(0, &mut id) };
    id
}

/// Does, off the real-time path, the one-time work that the calls made on an
/// audio thread would otherwise do there: reading the clock's timebase.
pub(crate) fn warm_up() {
    timebase();
}

/// Nothing to undo on macOS: the policy goes with the thread.
#[derive(Debug, Default)]
pub(crate) struct HeldPriority;

impl HeldPriority {
    pub(crate) fn revert(&mut self) {}

    pub(crate) fn forget(&mut self) {}
}

/// Mach absolute-time ticks per nanosecond, as a fraction.
fn timebase() -> (u64, u64) {
    static TIMEBASE: OnceLock<(u64, u64)> = OnceLock::new();
    *TIMEBASE.get_or_init(|| {
        let mut info = ffi::mach_timebase_info::default();
        // SAFETY: `info` is a valid, writable `mach_timebase_info`.
        let status = unsafe { ffi::mach_timebase_info(&mut info) };
        if status != 0 || info.numer == 0 || info.denom == 0 {
            (1, 1)
        } else {
            (u64::from(info.numer), u64::from(info.denom))
        }
    })
}

fn to_ticks(duration: Duration) -> u64 {
    let (numer, denom) = timebase();
    let ticks = duration.as_nanos() * u128::from(denom) / u128::from(numer);
    u64::try_from(ticks).unwrap_or(u64::MAX)
}

fn from_ticks(ticks: u64) -> Duration {
    let (numer, denom) = timebase();
    let nanos = u128::from(ticks) * u128::from(numer) / u128::from(denom);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

fn this_thread() -> libc::thread_t {
    // SAFETY: `pthread_self` always succeeds, and `pthread_mach_thread_np`
    // returns the calling thread's port without adding a reference.
    unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) }
}

pub(crate) fn current_priority() -> Priority {
    let mut policy = libc::thread_time_constraint_policy {
        period: 0,
        computation: 0,
        constraint: 0,
        preemptible: 0,
    };
    let mut count = libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT;
    let mut get_default: libc::boolean_t = 0;
    // SAFETY: `policy` and `count` describe a buffer of the size the flavour
    // needs, and every pointer is to a live local.
    let status = unsafe {
        libc::thread_policy_get(
            this_thread(),
            libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
            (&raw mut policy).cast(),
            &mut count,
            &mut get_default,
        )
    };
    if status == libc::KERN_SUCCESS && get_default == 0 {
        return Priority::TimeConstraint {
            period: from_ticks(policy.period.into()),
            computation: from_ticks(policy.computation.into()),
            constraint: from_ticks(policy.constraint.into()),
        };
    }

    // Not `pthread_getschedparam`, which takes libpthread's global thread
    // list lock: this runs on audio threads.
    let mut info = libc::thread_extended_info::default();
    let mut count = libc::THREAD_EXTENDED_INFO_COUNT;
    // SAFETY: `info` and `count` describe a buffer of the size the flavour
    // needs.
    let status = unsafe {
        libc::thread_info(
            this_thread(),
            libc::THREAD_EXTENDED_INFO as libc::thread_flavor_t,
            (&raw mut info).cast(),
            &mut count,
        )
    };
    if status == libc::KERN_SUCCESS {
        Priority::Normal {
            level: info.pth_priority,
        }
    } else {
        Priority::Unknown
    }
}

/// The time-constraint parameters for work that repeats every `period`: the
/// whole period as the deadline, half of it as the CPU time needed, kept
/// within the range the kernel accepts.
fn time_constraint(period: Duration) -> libc::thread_time_constraint_policy {
    const MIN_COMPUTATION: Duration = Duration::from_micros(100);
    const MAX_COMPUTATION: Duration = Duration::from_millis(50);
    let computation = (period / 2).clamp(MIN_COMPUTATION, MAX_COMPUTATION);
    let constraint = period.max(computation);
    let ticks = |d: Duration| u32::try_from(to_ticks(d)).unwrap_or(u32::MAX);
    libc::thread_time_constraint_policy {
        period: ticks(period),
        computation: ticks(computation),
        constraint: ticks(constraint),
        preemptible: 1,
    }
}

pub(crate) fn promote(period: Duration, _held: &mut HeldPriority) -> Promotion {
    let mut policy = time_constraint(period);
    // SAFETY: `policy` is a valid time-constraint policy of the size the
    // count says.
    let status = unsafe {
        libc::thread_policy_set(
            this_thread(),
            libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
            (&raw mut policy).cast(),
            libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
        )
    };
    if status == libc::KERN_SUCCESS {
        Promotion::Raised(Method::TimeConstraint)
    } else {
        Promotion::Refused(Method::TimeConstraint, Refusal::Os(status))
    }
}

// ---------------------------------------------------------------------------
// Workgroups

/// A retained `os_workgroup_t`.
#[derive(Debug)]
pub(crate) struct RawWorkgroup(NonNull<c_void>);

// SAFETY: OS objects are reference counted with atomic operations, and the
// workgroup calls are documented as callable from any thread.
unsafe impl Send for RawWorkgroup {}
// SAFETY: as above; nothing here mutates through a shared reference.
unsafe impl Sync for RawWorkgroup {}

impl RawWorkgroup {
    /// Takes ownership of one reference to `raw`.
    fn from_retained(raw: ffi::os_workgroup_t) -> Option<Self> {
        NonNull::new(raw).map(Self)
    }

    fn as_ptr(&self) -> ffi::os_workgroup_t {
        self.0.as_ptr()
    }
}

impl Clone for RawWorkgroup {
    fn clone(&self) -> Self {
        // SAFETY: `self` holds a reference, so the object is alive.
        unsafe { ffi::os_retain(self.as_ptr()) };
        Self(self.0)
    }
}

impl Drop for RawWorkgroup {
    fn drop(&mut self) {
        // SAFETY: releases the reference this value owns.
        unsafe { ffi::os_release(self.as_ptr()) };
    }
}

/// A join token, as `os_workgroup_join` fills it in. Callers keep it boxed,
/// at a stable address, from the join to the leave.
pub(crate) struct Token(ffi::os_workgroup_join_token_s);

impl Token {
    pub(crate) fn new() -> Self {
        Self(ffi::os_workgroup_join_token_s {
            sig: 0,
            opaque: [0; 36],
        })
    }
}

/// The process's one thread-exit hook for workgroup joins: a pthread key
/// whose destructor runs on a thread as it exits, before the system checks
/// that the thread left its workgroup. Made once, off the real-time path.
static EXIT_KEY: OnceLock<Option<libc::pthread_key_t>> = OnceLock::new();

/// Makes the exit hook's key, with `leave` as its destructor, if it doesn't
/// exist yet. Returns whether it exists.
pub(crate) fn init_exit_hook(leave: unsafe extern "C" fn(*mut c_void)) -> bool {
    EXIT_KEY
        .get_or_init(|| {
            let mut key = 0;
            // SAFETY: `key` is writable and `leave` is a valid destructor.
            (unsafe { libc::pthread_key_create(&mut key, Some(leave)) } == 0).then_some(key)
        })
        .is_some()
}

/// Hands `value` to the calling thread's exit hook. Allocation-free: macOS
/// keeps a thread's key values inside its thread structure. The hook must
/// exist ([`init_exit_hook`]).
pub(crate) fn set_exit_hook(value: *mut c_void) {
    if let Some(Some(key)) = EXIT_KEY.get() {
        // SAFETY: the key exists; the value is the caller's to hand over.
        unsafe { libc::pthread_setspecific(*key, value) };
    }
}

/// Takes back what [`set_exit_hook`] handed over on this thread, or null.
pub(crate) fn take_exit_hook() -> *mut c_void {
    let Some(Some(key)) = EXIT_KEY.get() else {
        return ptr::null_mut();
    };
    // SAFETY: the key exists.
    let value = unsafe { libc::pthread_getspecific(*key) };
    if !value.is_null() {
        // SAFETY: as above; clearing the value means the destructor won't
        // see it.
        unsafe { libc::pthread_setspecific(*key, ptr::null()) };
    }
    value
}

fn global(selector: u32) -> ffi::AudioObjectPropertyAddress {
    ffi::AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: ffi::kAudioObjectPropertyScopeGlobal,
        mElement: ffi::kAudioObjectPropertyElementMain,
    }
}

/// Reads a fixed-size property of a Core Audio object.
///
/// # Safety
///
/// `T` must be the property's type, and `qualifier` the qualifier it takes.
unsafe fn property<T: Copy>(
    object: ffi::AudioObjectID,
    selector: u32,
    qualifier: Option<&ffi::CFStringRef>,
    empty: T,
) -> Result<T, i32> {
    let address = global(selector);
    let mut value = empty;
    let mut size = u32::try_from(size_of::<T>()).unwrap_or(0);
    let (qualifier_size, qualifier) = match qualifier {
        Some(q) => (
            u32::try_from(size_of::<ffi::CFStringRef>()).unwrap_or(0),
            ptr::from_ref(q).cast::<c_void>(),
        ),
        None => (0, ptr::null()),
    };
    // SAFETY: the caller promises `T` matches the property; `size` is its
    // size and `value` is writable.
    let status = unsafe {
        ffi::AudioObjectGetPropertyData(
            object,
            &address,
            qualifier_size,
            qualifier,
            &mut size,
            (&raw mut value).cast(),
        )
    };
    if status == 0 { Ok(value) } else { Err(status) }
}

/// An owned `CFStringRef`, released on drop.
struct CfString(ffi::CFStringRef);

impl CfString {
    fn new(text: &str) -> Option<Self> {
        let len = isize::try_from(text.len()).ok()?;
        // SAFETY: `text` is valid UTF-8 of `len` bytes; a null allocator
        // means the default one.
        let raw = unsafe {
            ffi::CFStringCreateWithBytes(
                ptr::null(),
                text.as_ptr(),
                len,
                ffi::kCFStringEncodingUTF8,
                0,
            )
        };
        (!raw.is_null()).then_some(Self(raw))
    }

    fn to_string_lossy(&self) -> String {
        // SAFETY: `self.0` is a live CFString; the buffer is as large as the
        // maximum the encoding can need, plus the terminator.
        unsafe {
            let length = ffi::CFStringGetLength(self.0);
            let size =
                ffi::CFStringGetMaximumSizeForEncoding(length, ffi::kCFStringEncodingUTF8) + 1;
            let mut buffer = vec![0u8; usize::try_from(size).unwrap_or(1)];
            if ffi::CFStringGetCString(
                self.0,
                buffer.as_mut_ptr().cast(),
                size,
                ffi::kCFStringEncodingUTF8,
            ) == 0
            {
                return String::new();
            }
            CStr::from_ptr(buffer.as_ptr().cast())
                .to_string_lossy()
                .into_owned()
        }
    }
}

impl Drop for CfString {
    fn drop(&mut self) {
        // SAFETY: releases the reference this value owns.
        unsafe { ffi::CFRelease(self.0) };
    }
}

fn device_name(device: ffi::AudioObjectID) -> String {
    // SAFETY: the name property is a CFStringRef the caller must release.
    let name = unsafe {
        property(
            device,
            ffi::kAudioObjectPropertyName,
            None,
            ptr::null::<c_void>(),
        )
    };
    match name {
        Ok(raw) if !raw.is_null() => CfString(raw).to_string_lossy(),
        _ => format!("Core Audio device {device}"),
    }
}

/// The workgroup of a device's I/O thread and the device's name: the device
/// with this UID, or the default output device.
pub(crate) fn device_workgroup(
    uid: Option<&str>,
) -> Result<(RawWorkgroup, String), WorkgroupError> {
    let device = match uid {
        Some(uid) => {
            let cf = CfString::new(uid)
                .ok_or_else(|| WorkgroupError::new(format!("`{uid}` can't be a device UID")))?;
            // SAFETY: the translation takes a CFStringRef qualifier and yields
            // an AudioObjectID.
            unsafe {
                property(
                    ffi::kAudioObjectSystemObject,
                    ffi::kAudioHardwarePropertyTranslateUIDToDevice,
                    Some(&cf.0),
                    ffi::kAudioObjectUnknown,
                )
            }
            .map_err(|status| {
                WorkgroupError::new(format!(
                    "Core Audio couldn't look up `{uid}`: OSStatus {status}"
                ))
            })?
        }
        // SAFETY: the default output device is an AudioObjectID.
        None => unsafe {
            property(
                ffi::kAudioObjectSystemObject,
                ffi::kAudioHardwarePropertyDefaultOutputDevice,
                None,
                ffi::kAudioObjectUnknown,
            )
        }
        .map_err(|status| {
            WorkgroupError::new(format!(
                "Core Audio has no default output device: OSStatus {status}"
            ))
        })?,
    };
    if device == ffi::kAudioObjectUnknown {
        return Err(WorkgroupError::new(match uid {
            Some(uid) => format!("Core Audio has no device with UID `{uid}`"),
            None => "Core Audio has no default output device".to_owned(),
        }));
    }
    let name = device_name(device);
    // SAFETY: the workgroup property is a retained os_workgroup_t.
    let raw = unsafe {
        property::<ffi::os_workgroup_t>(
            device,
            ffi::kAudioDevicePropertyIOThreadOSWorkgroup,
            None,
            ptr::null_mut(),
        )
    }
    .map_err(|status| {
        WorkgroupError::new(format!("{name} has no I/O workgroup: OSStatus {status}"))
    })?;
    let workgroup = RawWorkgroup::from_retained(raw)
        .ok_or_else(|| WorkgroupError::new(format!("{name} has no I/O workgroup")))?;
    Ok((workgroup, name))
}

pub(crate) fn workgroup_description(workgroup: &RawWorkgroup) -> String {
    type Send0 = unsafe extern "C" fn(*mut c_void, *const c_void) -> *mut c_void;
    type SendCStr = unsafe extern "C" fn(*mut c_void, *const c_void) -> *const c_char;
    // SAFETY: `objc_msgSend` is called through the exact signatures of the
    // two methods: `-[NSObject debugDescription]` returns an NSString, and
    // `-[NSString UTF8String]` a C string that lives as long as the
    // autorelease pool, which is popped only after the string is copied.
    unsafe {
        let pool = ffi::objc_autoreleasePoolPush();
        let send0: Send0 = std::mem::transmute(ffi::objc_msgSend as unsafe extern "C" fn());
        let send_cstr: SendCStr = std::mem::transmute(ffi::objc_msgSend as unsafe extern "C" fn());
        let description = send0(
            workgroup.as_ptr(),
            ffi::sel_registerName(c"debugDescription".as_ptr()),
        );
        let text = if description.is_null() {
            String::new()
        } else {
            let utf8 = send_cstr(description, ffi::sel_registerName(c"UTF8String".as_ptr()));
            if utf8.is_null() {
                String::new()
            } else {
                CStr::from_ptr(utf8).to_string_lossy().into_owned()
            }
        };
        ffi::objc_autoreleasePoolPop(pool);
        text
    }
}

pub(crate) fn workgroup_max_parallel_threads(workgroup: &RawWorkgroup) -> Option<u32> {
    // SAFETY: a null attribute asks for the default answer.
    let threads =
        unsafe { ffi::os_workgroup_max_parallel_threads(workgroup.as_ptr(), ptr::null_mut()) };
    u32::try_from(threads).ok().filter(|&n| n > 0)
}

/// Joins the calling thread to `workgroup`, filling in `token`. Allocation-free.
pub(crate) fn workgroup_join(workgroup: &RawWorkgroup, token: &mut Token) -> Result<(), i32> {
    // SAFETY: the workgroup is alive and the token is writable memory of
    // the size the SDK declares.
    let status = unsafe { ffi::os_workgroup_join(workgroup.as_ptr(), &mut token.0) };
    if status == 0 { Ok(()) } else { Err(status) }
}

/// Leaves `workgroup`. The caller makes sure `token` holds this thread's
/// successful join of it, and leaves once.
pub(crate) fn workgroup_leave(workgroup: &RawWorkgroup, token: &mut Token) {
    // SAFETY: as the caller promises.
    unsafe { ffi::os_workgroup_leave(workgroup.as_ptr(), &mut token.0) };
}

pub(crate) fn work_interval_create(name: &str) -> Result<RawWorkgroup, WorkgroupError> {
    let c_name = CString::new(name)
        .map_err(|_| WorkgroupError::new("a work interval's name can't contain a NUL"))?;
    // SAFETY: the name is a valid C string, the clock is one the SDK names,
    // and the attribute must be null. The result is retained.
    let raw = unsafe {
        ffi::AudioWorkIntervalCreate(
            c_name.as_ptr(),
            ffi::OS_CLOCK_MACH_ABSOLUTE_TIME,
            ptr::null_mut(),
        )
    };
    RawWorkgroup::from_retained(raw).ok_or_else(|| {
        WorkgroupError::new(format!("macOS couldn't create work interval \"{name}\""))
    })
}

pub(crate) fn work_interval_start(workgroup: &RawWorkgroup, length: Duration) -> Result<(), i32> {
    // SAFETY: reading the clock has no preconditions.
    let start = unsafe { ffi::mach_absolute_time() };
    let deadline = start.saturating_add(to_ticks(length));
    // SAFETY: the workgroup is alive and was made by AudioWorkIntervalCreate,
    // so it's an interval; the data argument must be null.
    let status = unsafe {
        ffi::os_workgroup_interval_start(workgroup.as_ptr(), start, deadline, ptr::null_mut())
    };
    if status == 0 { Ok(()) } else { Err(status) }
}

pub(crate) fn work_interval_finish(workgroup: &RawWorkgroup) -> Result<(), i32> {
    // SAFETY: as for `work_interval_start`.
    let status = unsafe { ffi::os_workgroup_interval_finish(workgroup.as_ptr(), ptr::null_mut()) };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_round_trip_through_nanoseconds() {
        for micros in [100, 2_667, 46_440, 1_000_000] {
            let d = Duration::from_micros(micros);
            assert!(from_ticks(to_ticks(d)).abs_diff(d) < Duration::from_micros(1));
        }
    }

    #[test]
    fn time_constraints_stay_in_the_range_the_kernel_takes() {
        for (period_us, computation_us) in
            [(333, 166), (2_667, 1_333), (150, 100), (1_000_000, 50_000)]
        {
            let policy = time_constraint(Duration::from_micros(period_us));
            let computation = from_ticks(policy.computation.into());
            assert!(
                computation.abs_diff(Duration::from_micros(computation_us))
                    < Duration::from_micros(2),
                "{period_us} µs: {computation:?}"
            );
            assert!(policy.constraint >= policy.computation);
        }
    }

    #[test]
    fn extreme_periods_are_accepted() {
        for period in [Duration::from_micros(333), Duration::from_secs(1)] {
            let promotion = std::thread::spawn(move || promote(period, &mut HeldPriority))
                .join()
                .unwrap();
            assert_eq!(
                promotion,
                Promotion::Raised(Method::TimeConstraint),
                "{period:?}"
            );
        }
    }
}
