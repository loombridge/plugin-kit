//! An Audio Unit (v2) host for tests, on macOS: registers a plug-in's Audio
//! Unit factory in the test process, makes an instance, renders blocks, and
//! records the parameter notifications it sends listeners (its gestures and
//! value changes), as a host such as Logic hears them.
//!
//! The factory is the plug-in's `GetPluginFactoryAUV2`, which
//! `lspk_plugin::export_clap!` exports with the `auv2` feature; a test that
//! exports its own plug-in passes it directly, so nothing is installed and
//! the plug-in runs in the test binary.

use std::cell::RefCell;
use std::ffi::{CString, c_void};
use std::time::{Duration, Instant};

/// An Audio Unit's factory function, as its bundle's `Info.plist` names it.
pub type Factory = unsafe extern "C" fn(*const c_void) -> *mut c_void;

/// What the plug-in told its listeners about a parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AuEvent {
    /// A gesture on parameter `id` began.
    Begin(u32),
    /// Parameter `id` changed to this value.
    Value(u32, f32),
    /// A gesture on parameter `id` ended.
    End(u32),
}

/// An Audio Unit instance, hosted for a test.
pub struct AuTestHost {
    unit: AudioUnit,
    listener: *mut c_void,
    /// What the listener heard, boxed so its callback can keep a pointer
    /// to it.
    events: Box<RefCell<Vec<AuEvent>>>,
    channels: usize,
    sample_time: f64,
    initialized: bool,
}

impl AuTestHost {
    /// Registers `factory` as the Audio Unit `kind`, `subtype` and
    /// `manufacturer` (four characters each, as `auval` takes them) in this
    /// process, once per set of codes, and makes an instance of it.
    ///
    /// # Errors
    ///
    /// The instance couldn't be made.
    pub fn new(factory: Factory, codes: [&str; 3]) -> Result<Self, String> {
        let [kind, subtype, manufacturer] = codes.map(four_cc);
        let desc = AudioComponentDescription {
            component_type: kind,
            component_sub_type: subtype,
            component_manufacturer: manufacturer,
            component_flags: 0,
            component_flags_mask: 0,
        };
        // SAFETY: looking up, and if need be registering, a component with
        // a description, a name and the plug-in's factory function.
        let component = unsafe {
            let mut component = AudioComponentFindNext(std::ptr::null_mut(), &desc);
            if component.is_null() {
                let name = CString::new("plug-in kit tests").map_err(|e| e.to_string())?;
                let name = CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), UTF8);
                component = AudioComponentRegister(&desc, name, 0x0001_0000, factory);
                CFRelease(name);
            }
            component
        };
        if component.is_null() {
            return Err("AudioComponentRegister failed".into());
        }
        let mut unit: AudioUnit = std::ptr::null_mut();
        // SAFETY: a registered component and an out pointer.
        let result = unsafe { AudioComponentInstanceNew(component, &mut unit) };
        status(result, "AudioComponentInstanceNew")?;

        let events = Box::new(RefCell::new(Vec::new()));
        let mut listener = std::ptr::null_mut();
        // SAFETY: a listener on this thread's run loop, whose callback gets
        // `events`, which lives as long as the listener.
        let result = unsafe {
            AUEventListenerCreate(
                listen,
                std::ptr::from_ref::<RefCell<Vec<AuEvent>>>(&events)
                    .cast_mut()
                    .cast(),
                CFRunLoopGetCurrent(),
                kCFRunLoopDefaultMode,
                0.0,
                0.0,
                &mut listener,
            )
        };
        status(result, "AUEventListenerCreate")?;
        // An effect pulls its input from this callback, which gives silence;
        // an instrument has no input to give it to.
        let callback = RenderCallback {
            proc_: silence,
            ref_con: std::ptr::null_mut(),
        };
        // SAFETY: the Audio Unit's own call, with a value of the property's
        // size.
        unsafe {
            AudioUnitSetProperty(
                unit,
                SET_RENDER_CALLBACK,
                INPUT,
                0,
                std::ptr::from_ref(&callback).cast(),
                u32::try_from(std::mem::size_of::<RenderCallback>()).unwrap_or(16),
            );
        }
        let host = Self {
            unit,
            listener,
            events,
            channels: 2,
            sample_time: 0.0,
            initialized: false,
        };
        for id in host.param_ids() {
            for event_type in [BEGIN_GESTURE, VALUE_CHANGE, END_GESTURE] {
                let event = AudioUnitEvent {
                    event_type,
                    argument: AudioUnitParameter {
                        audio_unit: unit,
                        parameter_id: id,
                        scope: GLOBAL,
                        element: 0,
                    },
                };
                // SAFETY: the listener made above, and an event to listen for.
                unsafe { AUEventListenerAddEventType(listener, std::ptr::null_mut(), &event) };
            }
        }
        Ok(host)
    }

    /// Every parameter's ID, in the Audio Unit's order.
    #[must_use]
    pub fn param_ids(&self) -> Vec<u32> {
        let mut size = 0u32;
        let mut writable = 0u8;
        // SAFETY: the Audio Unit's own call.
        let result = unsafe {
            AudioUnitGetPropertyInfo(
                self.unit,
                PARAMETER_LIST,
                GLOBAL,
                0,
                &mut size,
                &mut writable,
            )
        };
        if result != 0 {
            return Vec::new();
        }
        let mut ids = vec![0u32; size as usize / 4];
        // SAFETY: the Audio Unit's own call, with a buffer of the size it
        // reported.
        let result = unsafe {
            AudioUnitGetProperty(
                self.unit,
                PARAMETER_LIST,
                GLOBAL,
                0,
                ids.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if result != 0 {
            return Vec::new();
        }
        ids.truncate(size as usize / 4);
        ids
    }

    /// Parameter `id`'s value.
    #[must_use]
    pub fn value(&self, id: u32) -> Option<f32> {
        let mut value = 0.0;
        // SAFETY: the Audio Unit's own call.
        let result = unsafe { AudioUnitGetParameter(self.unit, id, GLOBAL, 0, &mut value) };
        (result == 0).then_some(value)
    }

    /// Sets parameter `id`, as a host's automation does.
    ///
    /// # Errors
    ///
    /// The Audio Unit refused.
    pub fn set_value(&mut self, id: u32, value: f32) -> Result<(), String> {
        // SAFETY: the Audio Unit's own call.
        let result = unsafe { AudioUnitSetParameter(self.unit, id, GLOBAL, 0, value, 0) };
        status(result, "AudioUnitSetParameter")
    }

    /// Initialises the Audio Unit for blocks of up to `max_frames`.
    ///
    /// # Errors
    ///
    /// The Audio Unit refused.
    pub fn initialize(&mut self, max_frames: u32) -> Result<(), String> {
        // SAFETY: the Audio Unit's own call, with a value of the property's
        // size.
        let result = unsafe {
            AudioUnitSetProperty(
                self.unit,
                MAXIMUM_FRAMES_PER_SLICE,
                GLOBAL,
                0,
                std::ptr::from_ref(&max_frames).cast(),
                4,
            )
        };
        status(result, "MaximumFramesPerSlice")?;
        // SAFETY: the Audio Unit's own call.
        let result = unsafe { AudioUnitInitialize(self.unit) };
        status(result, "AudioUnitInitialize")?;
        self.initialized = true;
        Ok(())
    }

    /// Sends a MIDI message, as a host does before the next render.
    pub fn midi(&mut self, status_byte: u8, data1: u8, data2: u8) {
        // SAFETY: the Audio Unit's own call.
        unsafe {
            MusicDeviceMIDIEvent(
                self.unit,
                u32::from(status_byte),
                u32::from(data1),
                u32::from(data2),
                0,
            );
        }
    }

    /// Renders `frames` frames of the main output, one buffer a channel,
    /// and returns them.
    ///
    /// # Errors
    ///
    /// The render failed.
    pub fn render(&mut self, frames: u32) -> Result<Vec<Vec<f32>>, String> {
        let mut channels: Vec<Vec<f32>> = (0..self.channels)
            .map(|_| vec![0.0; frames as usize])
            .collect();
        let mut list = AudioBufferList {
            number_buffers: u32::try_from(self.channels).unwrap_or(2),
            buffers: [AudioBuffer::EMPTY; 2],
        };
        for (buffer, channel) in list.buffers.iter_mut().zip(&mut channels) {
            *buffer = AudioBuffer {
                number_channels: 1,
                data_byte_size: frames * 4,
                data: channel.as_mut_ptr().cast(),
            };
        }
        let stamp = AudioTimeStamp {
            sample_time: self.sample_time,
            flags: 1, // kAudioTimeStampSampleTimeValid
            ..AudioTimeStamp::default()
        };
        let mut flags = 0u32;
        // SAFETY: buffers of `frames` frames for each channel, living until
        // the call returns.
        let result =
            unsafe { AudioUnitRender(self.unit, &mut flags, &stamp, 0, frames, &mut list) };
        status(result, "AudioUnitRender")?;
        self.sample_time += f64::from(frames);
        Ok(channels)
    }

    /// Runs this thread's run loop until the listener has heard `event`, or
    /// `timeout` passes; returns whether it heard it.
    pub fn listen_until(&mut self, event: AuEvent, timeout: Duration) -> bool {
        let start = Instant::now();
        loop {
            if self.events.borrow().contains(&event) {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            // SAFETY: running this thread's run loop briefly.
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.02, 0) };
        }
    }

    /// Takes what the listener heard since the last call.
    pub fn take_events(&mut self) -> Vec<AuEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    /// The Audio Unit's state (its class info), as a host saves it.
    ///
    /// # Errors
    ///
    /// The Audio Unit couldn't save it.
    pub fn save_state(&mut self) -> Result<State, String> {
        let mut plist: *const c_void = std::ptr::null();
        let mut size = u32::try_from(std::mem::size_of::<*const c_void>()).unwrap_or(8);
        // SAFETY: the Audio Unit's own call, with a place for a property
        // list it retains for us.
        let result = unsafe {
            AudioUnitGetProperty(
                self.unit,
                CLASS_INFO,
                GLOBAL,
                0,
                std::ptr::from_mut(&mut plist).cast(),
                &mut size,
            )
        };
        status(result, "ClassInfo")?;
        if plist.is_null() {
            return Err("no class info".into());
        }
        Ok(State(plist))
    }

    /// Loads `state` (from [`save_state`](Self::save_state), maybe on
    /// another instance), as a host does when it opens a project. The
    /// instance must be initialised.
    ///
    /// # Errors
    ///
    /// The Audio Unit refused it.
    pub fn load_state(&mut self, state: &State) -> Result<(), String> {
        // SAFETY: the Audio Unit's own call, with a property list.
        let result = unsafe {
            AudioUnitSetProperty(
                self.unit,
                CLASS_INFO,
                GLOBAL,
                0,
                std::ptr::from_ref(&state.0).cast(),
                u32::try_from(std::mem::size_of::<*const c_void>()).unwrap_or(8),
            )
        };
        status(result, "ClassInfo")
    }
}

impl Drop for AuTestHost {
    fn drop(&mut self) {
        // SAFETY: the instance and listener made in `new`, each disposed
        // once.
        unsafe {
            AUListenerDispose(self.listener);
            if self.initialized {
                AudioUnitUninitialize(self.unit);
            }
            AudioComponentInstanceDispose(self.unit);
        }
    }
}

/// An Audio Unit's saved state, a property list.
pub struct State(*const c_void);

impl Drop for State {
    fn drop(&mut self) {
        // SAFETY: the property list the Audio Unit retained for us.
        unsafe { CFRelease(self.0) };
    }
}

/// The listener's callback: records each parameter notification.
unsafe extern "C" fn listen(
    sink: *mut c_void,
    _object: *mut c_void,
    event: *const AudioUnitEvent,
    _host_time: u64,
    value: f32,
) {
    // SAFETY: `sink` is the boxed list `AuTestHost` keeps while the listener
    // lives, and `event` the framework's event.
    let (sink, event) = unsafe { (&*sink.cast::<RefCell<Vec<AuEvent>>>(), &*event) };
    let id = event.argument.parameter_id;
    let heard = match event.event_type {
        BEGIN_GESTURE => AuEvent::Begin(id),
        END_GESTURE => AuEvent::End(id),
        VALUE_CHANGE => AuEvent::Value(id, value),
        _ => return,
    };
    if let Ok(mut events) = sink.try_borrow_mut() {
        events.push(heard);
    }
}

/// The input callback: silence on every buffer.
unsafe extern "C" fn silence(
    _ref_con: *mut c_void,
    _flags: *mut u32,
    _stamp: *const AudioTimeStamp,
    _bus: u32,
    _frames: u32,
    data: *mut AudioBufferList,
) -> i32 {
    // SAFETY: the Audio Unit passes a buffer list of its own to fill, with
    // as many buffers as it says.
    unsafe {
        let count = (*data).number_buffers as usize;
        let buffers = std::ptr::addr_of_mut!((*data).buffers).cast::<AudioBuffer>();
        for i in 0..count {
            let buffer = &mut *buffers.add(i);
            if !buffer.data.is_null() {
                std::ptr::write_bytes(buffer.data.cast::<u8>(), 0, buffer.data_byte_size as usize);
            }
        }
    }
    0
}

/// A four-character code, such as `aufx`.
fn four_cc(code: &str) -> u32 {
    code.bytes()
        .chain(std::iter::repeat(b' '))
        .take(4)
        .fold(0, |n, b| (n << 8) | u32::from(b))
}

fn status(result: i32, call: &str) -> Result<(), String> {
    if result == 0 {
        Ok(())
    } else {
        Err(format!("{call} failed ({result})"))
    }
}

type AudioUnit = *mut c_void;

const GLOBAL: u32 = 0;
const INPUT: u32 = 1;
const SET_RENDER_CALLBACK: u32 = 23;
const CLASS_INFO: u32 = 0;
const PARAMETER_LIST: u32 = 3;
const MAXIMUM_FRAMES_PER_SLICE: u32 = 14;
const VALUE_CHANGE: u32 = 0;
const BEGIN_GESTURE: u32 = 1;
const END_GESTURE: u32 = 2;
const UTF8: u32 = 0x0800_0100;

#[repr(C)]
struct AudioComponentDescription {
    component_type: u32,
    component_sub_type: u32,
    component_manufacturer: u32,
    component_flags: u32,
    component_flags_mask: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioUnitParameter {
    audio_unit: AudioUnit,
    parameter_id: u32,
    scope: u32,
    element: u32,
}

/// `AudioUnitEvent`, whose argument is a union of a parameter and a
/// property of the same layout; only parameters are listened for.
#[repr(C)]
struct AudioUnitEvent {
    event_type: u32,
    argument: AudioUnitParameter,
}

#[repr(C)]
#[derive(Default)]
struct SmpteTime {
    subframes: i16,
    subframe_divisor: i16,
    counter: u32,
    kind: u32,
    flags: u32,
    hours: i16,
    minutes: i16,
    seconds: i16,
    frames: i16,
}

#[repr(C)]
#[derive(Default)]
struct AudioTimeStamp {
    sample_time: f64,
    host_time: u64,
    rate_scalar: f64,
    word_clock_time: u64,
    smpte_time: SmpteTime,
    flags: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioBuffer {
    number_channels: u32,
    data_byte_size: u32,
    data: *mut c_void,
}

impl AudioBuffer {
    const EMPTY: Self = Self {
        number_channels: 0,
        data_byte_size: 0,
        data: std::ptr::null_mut(),
    };
}

#[repr(C)]
struct AudioBufferList {
    number_buffers: u32,
    buffers: [AudioBuffer; 2],
}

#[repr(C)]
struct RenderCallback {
    proc_: unsafe extern "C" fn(
        *mut c_void,
        *mut u32,
        *const AudioTimeStamp,
        u32,
        u32,
        *mut AudioBufferList,
    ) -> i32,
    ref_con: *mut c_void,
}

type ListenerProc = unsafe extern "C" fn(*mut c_void, *mut c_void, *const AudioUnitEvent, u64, f32);

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioComponentFindNext(
        component: *mut c_void,
        desc: *const AudioComponentDescription,
    ) -> *mut c_void;
    fn AudioComponentRegister(
        desc: *const AudioComponentDescription,
        name: *const c_void,
        version: u32,
        factory: Factory,
    ) -> *mut c_void;
    fn AudioComponentInstanceNew(component: *mut c_void, unit: *mut AudioUnit) -> i32;
    fn AudioComponentInstanceDispose(unit: AudioUnit) -> i32;
    fn AudioUnitInitialize(unit: AudioUnit) -> i32;
    fn AudioUnitUninitialize(unit: AudioUnit) -> i32;
    fn AudioUnitGetPropertyInfo(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        size: *mut u32,
        writable: *mut u8,
    ) -> i32;
    fn AudioUnitGetProperty(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> i32;
    fn AudioUnitSetProperty(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        data: *const c_void,
        size: u32,
    ) -> i32;
    fn AudioUnitGetParameter(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        value: *mut f32,
    ) -> i32;
    fn AudioUnitSetParameter(
        unit: AudioUnit,
        id: u32,
        scope: u32,
        element: u32,
        value: f32,
        offset: u32,
    ) -> i32;
    fn AudioUnitRender(
        unit: AudioUnit,
        flags: *mut u32,
        stamp: *const AudioTimeStamp,
        bus: u32,
        frames: u32,
        data: *mut AudioBufferList,
    ) -> i32;
    fn MusicDeviceMIDIEvent(
        unit: AudioUnit,
        status: u32,
        data1: u32,
        data2: u32,
        offset: u32,
    ) -> i32;
    fn AUEventListenerCreate(
        proc_: ListenerProc,
        user_data: *mut c_void,
        run_loop: *mut c_void,
        mode: *const c_void,
        interval: f32,
        granularity: f32,
        listener: *mut *mut c_void,
    ) -> i32;
    fn AUEventListenerAddEventType(
        listener: *mut c_void,
        object: *mut c_void,
        event: *const AudioUnitEvent,
    ) -> i32;
    fn AUListenerDispose(listener: *mut c_void) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: u8) -> i32;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        text: *const std::ffi::c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFRelease(object: *const c_void);
}
