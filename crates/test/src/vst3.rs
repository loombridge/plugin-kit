//! A VST3 host for tests: makes a plug-in from a VST3 factory, reads its
//! parameters, runs blocks through it, and records what it tells the host
//! through `IComponentHandler` (its edits, and that its state is dirty).
//!
//! The factory comes from the plug-in's `GetPluginFactory`, which
//! `lspk_plugin::export_clap!` exports with the `vst3` feature; a test that
//! exports its own plug-in calls it directly, so the plug-in runs in the
//! test binary.
//!
//! clap-wrapper sends the plug-in's edits to the host from an idle timer on
//! the main thread, as VST3 asks; [`Vst3TestHost::idle`] runs the platform's
//! event loop so that timer fires. On Linux the wrapper gets its timer only
//! from the run loop of an open editor, so without one its edits reach a
//! host only as output parameter changes from `process`.

use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;
use std::time::{Duration, Instant};

use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, BusDirections_::kInput, BusDirections_::kOutput,
    Event, Event_::EventTypes_::kNoteOnEvent, Event__type0, IAudioProcessor, IAudioProcessorTrait,
    IComponent, IComponentHandler, IComponentHandler2, IComponentHandler2Trait,
    IComponentHandlerTrait, IComponentTrait, IEditController, IEditControllerTrait, IEventList,
    IEventListTrait, IHostApplication, IHostApplicationTrait, IParamValueQueue,
    IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait, MediaTypes_::kAudio,
    MediaTypes_::kEvent, NoteOnEvent, ParamID, ParamValue, ParameterInfo, ParameterInfo_,
    ProcessData, ProcessModes_::kRealtime, ProcessSetup, String128,
    SymbolicSampleSizes_::kSample32, TChar,
};
use vst3::Steinberg::{
    FIDString, IBStream, IBStreamTrait, IPluginBase, IPluginBaseTrait, IPluginFactory,
    IPluginFactoryTrait, PClassInfo, TBool, TUID, int32, int64, kInvalidArgument, kNotImplemented,
    kResultFalse, kResultOk, tresult,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};

/// What the plug-in told the host through its component handler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HandlerEvent {
    /// A gesture on parameter `id` began (`beginEdit`).
    Begin(u32),
    /// Parameter `id` changed to a normalised value (`performEdit`).
    Perform(u32, f64),
    /// A gesture on parameter `id` ended (`endEdit`).
    End(u32),
    /// Its state is or isn't dirty (`IComponentHandler2::setDirty`).
    Dirty(bool),
    /// It asked for a restart with these flags (`restartComponent`).
    Restart(i32),
}

/// A parameter, as the controller lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Vst3Param {
    /// Its ID.
    pub id: u32,
    /// Its title.
    pub title: String,
    /// How many steps it has; 0 for a continuous parameter.
    pub steps: i32,
    /// Its default, normalised.
    pub default: f64,
    /// Whether the host should hide it (`kIsHidden`).
    pub hidden: bool,
    /// Whether automation can move it (`kCanAutomate`).
    pub automatable: bool,
}

/// One block's results.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vst3Block {
    /// Each output channel.
    pub outputs: Vec<Vec<f32>>,
    /// The output parameter changes: parameter ID, frame and normalised
    /// value.
    pub changes: Vec<(u32, i32, f64)>,
}

/// A VST3 plug-in instance, hosted for a test.
pub struct Vst3TestHost {
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: ComPtr<IEditController>,
    events: Rc<RefCell<Vec<HandlerEvent>>>,
    _host: ComWrapper<HostApplication>,
    _handler: ComWrapper<ComponentHandler>,
    inputs: usize,
    outputs: usize,
    active: bool,
}

impl Vst3TestHost {
    /// Makes an instance of the factory's first audio module class.
    ///
    /// # Safety
    ///
    /// `factory` is what a VST3 module's `GetPluginFactory` returned, and
    /// stays valid while the instance lives. The instance takes a reference
    /// to it of its own.
    ///
    /// # Errors
    ///
    /// The factory holds no audio module, or it failed to start.
    pub unsafe fn from_factory(factory: *mut c_void) -> Result<Self, String> {
        // SAFETY: as the caller promises, a factory. The instance takes a
        // reference of its own, leaving the caller's alone.
        let factory = unsafe { ComRef::<IPluginFactory>::from_raw(factory.cast()) }
            .ok_or("no factory")?
            .to_com_ptr();
        let mut cid: Option<TUID> = None;
        // SAFETY: the factory's own calls, with buffers of the sizes VST3
        // gives.
        unsafe {
            for index in 0..factory.countClasses() {
                let mut info: PClassInfo = std::mem::zeroed();
                if factory.getClassInfo(index, &mut info) == kResultOk
                    && c_text(&info.category) == "Audio Module Class"
                {
                    cid = Some(info.cid);
                    break;
                }
            }
        }
        let cid = cid.ok_or("the factory holds no audio module")?;
        let mut object = std::ptr::null_mut();
        // SAFETY: a class ID and interface ID from the factory and the
        // bindings, and an out pointer.
        let result = unsafe {
            factory.createInstance(cid.as_ptr(), IComponent::IID.as_ptr().cast(), &mut object)
        };
        if result != kResultOk {
            return Err(format!("createInstance failed ({result})"));
        }
        // SAFETY: createInstance gave a component with a reference for us.
        let component =
            unsafe { ComPtr::<IComponent>::from_raw(object.cast()) }.ok_or("no component")?;

        let host = ComWrapper::new(HostApplication);
        let context = host
            .to_com_ptr::<IHostApplication>()
            .ok_or("no host application")?;
        // SAFETY: the component's own call, with the host's context.
        let result = unsafe { component.initialize(context.as_ptr().cast()) };
        if result != kResultOk {
            return Err(format!("initialize failed ({result})"));
        }
        let processor = component
            .cast::<IAudioProcessor>()
            .ok_or("the component isn't an audio processor")?;
        // clap-wrapper makes a single component, which is its own
        // controller.
        let controller = component
            .cast::<IEditController>()
            .ok_or("the component isn't its own controller")?;

        let events = Rc::new(RefCell::new(Vec::new()));
        let handler = ComWrapper::new(ComponentHandler {
            events: events.clone(),
        });
        let handler_ptr = handler
            .to_com_ptr::<IComponentHandler>()
            .ok_or("no component handler")?;
        // SAFETY: the controller's own call; it takes a reference.
        unsafe { controller.setComponentHandler(handler_ptr.as_ptr()) };

        // SAFETY: the component's own calls, with a buffer of its size.
        let (inputs, outputs) = unsafe {
            let count = |dir| {
                let mut info = std::mem::zeroed();
                let main = component.getBusCount(kAudio as _, dir as _) > 0
                    && component.getBusInfo(kAudio as _, dir as _, 0, &mut info) == kResultOk;
                if main {
                    usize::try_from(info.channelCount).unwrap_or(0)
                } else {
                    0
                }
            };
            (count(kInput), count(kOutput))
        };
        Ok(Self {
            component,
            processor,
            controller,
            events,
            _host: host,
            _handler: handler,
            inputs,
            outputs,
            active: false,
        })
    }

    /// Every parameter, in the controller's order.
    #[must_use]
    pub fn params(&self) -> Vec<Vst3Param> {
        // SAFETY: the controller's own calls, with a buffer of its size.
        unsafe {
            (0..self.controller.getParameterCount())
                .filter_map(|index| {
                    let mut info: ParameterInfo = std::mem::zeroed();
                    (self.controller.getParameterInfo(index, &mut info) == kResultOk).then(|| {
                        Vst3Param {
                            id: info.id,
                            title: utf16_text(&info.title),
                            steps: info.stepCount,
                            default: info.defaultNormalizedValue,
                            hidden: info.flags & ParameterInfo_::ParameterFlags_::kIsHidden != 0,
                            automatable: info.flags & ParameterInfo_::ParameterFlags_::kCanAutomate
                                != 0,
                        }
                    })
                })
                .collect()
        }
    }

    /// Parameter `id`'s value, normalised, as the controller has it.
    #[must_use]
    pub fn value(&self, id: u32) -> f64 {
        // SAFETY: the controller's own call.
        unsafe { self.controller.getParamNormalized(id) }
    }

    /// How the controller shows parameter `id` at the normalised value
    /// `value`.
    #[must_use]
    pub fn text(&self, id: u32, value: f64) -> Option<String> {
        // SAFETY: the controller's own call, with a buffer of its size.
        unsafe {
            let mut text: String128 = std::mem::zeroed();
            (self.controller.getParamStringByValue(id, value, &mut text) == kResultOk)
                .then(|| utf16_text(&text))
        }
    }

    /// Activates the instance and starts processing, at `sample_rate` in
    /// blocks of up to `max_frames`.
    ///
    /// # Errors
    ///
    /// The plug-in refused.
    pub fn activate(&mut self, sample_rate: f64, max_frames: u32) -> Result<(), String> {
        let mut setup = ProcessSetup {
            processMode: kRealtime as _,
            symbolicSampleSize: kSample32 as _,
            maxSamplesPerBlock: i32::try_from(max_frames).map_err(|e| e.to_string())?,
            sampleRate: sample_rate,
        };
        // SAFETY: the processor's and component's own calls.
        unsafe {
            check(
                self.processor.setupProcessing(&mut setup),
                "setupProcessing",
            )?;
            for dir in [kInput, kOutput] {
                for index in 0..self.component.getBusCount(kAudio as _, dir as _) {
                    self.component.activateBus(kAudio as _, dir as _, index, 1);
                }
                for index in 0..self.component.getBusCount(kEvent as _, dir as _) {
                    self.component.activateBus(kEvent as _, dir as _, index, 1);
                }
            }
            check(self.component.setActive(1), "setActive")?;
            check(self.processor.setProcessing(1), "setProcessing")?;
        }
        self.active = true;
        Ok(())
    }

    /// Stops or restarts processing, leaving the instance active, as a host
    /// does when it stops calling `process` for a while.
    ///
    /// # Errors
    ///
    /// The plug-in refused.
    pub fn set_processing(&mut self, processing: bool) -> Result<(), String> {
        assert!(self.active, "activate the instance first");
        // SAFETY: the processor's own call.
        check(
            unsafe { self.processor.setProcessing(u8::from(processing)) },
            "setProcessing",
        )
    }

    /// Stops processing and deactivates the instance.
    pub fn deactivate(&mut self) {
        if self.active {
            // SAFETY: the processor's and component's own calls.
            unsafe {
                self.processor.setProcessing(0);
                self.component.setActive(0);
            }
            self.active = false;
        }
    }

    /// Runs one block of `frames` frames: `input` on every input channel,
    /// `notes` (frame, key, velocity) on the first event bus, and
    /// `params` (ID, frame, normalised value) as input parameter changes.
    ///
    /// # Panics
    ///
    /// If the instance isn't active or the block is too long.
    pub fn process(
        &mut self,
        frames: usize,
        input: Option<&[f32]>,
        notes: &[(i32, i16, f32)],
        params: &[(u32, i32, f64)],
    ) -> Vst3Block {
        assert!(self.active, "activate the instance first");
        let mut ins: Vec<Vec<f32>> = (0..self.inputs)
            .map(|_| input.map_or_else(|| vec![0.0; frames], <[f32]>::to_vec))
            .collect();
        let mut outs: Vec<Vec<f32>> = (0..self.outputs).map(|_| vec![0.0; frames]).collect();
        let mut in_ptrs: Vec<*mut f32> = ins.iter_mut().map(|c| c.as_mut_ptr()).collect();
        let mut out_ptrs: Vec<*mut f32> = outs.iter_mut().map(|c| c.as_mut_ptr()).collect();
        let bus = |channels: &mut Vec<*mut f32>| AudioBusBuffers {
            numChannels: i32::try_from(channels.len()).unwrap_or(0),
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: channels.as_mut_ptr(),
            },
        };
        let mut in_bus = bus(&mut in_ptrs);
        let mut out_bus = bus(&mut out_ptrs);

        let event_list = ComWrapper::new(EventList {
            events: notes
                .iter()
                .map(|&(frame, key, velocity)| note_on(frame, key, velocity))
                .collect(),
        });
        let input_changes = ComWrapper::new(ParameterChanges::default());
        for &(id, frame, value) in params {
            input_changes.add(id, frame, value);
        }
        let output_changes = ComWrapper::new(ParameterChanges::default());
        let as_ptr = |w: &ComWrapper<ParameterChanges>| {
            w.to_com_ptr::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |p| p.as_ptr())
        };
        let event_ptr = event_list
            .to_com_ptr::<IEventList>()
            .map_or(std::ptr::null_mut(), |p| p.as_ptr());
        let mut data = ProcessData {
            processMode: kRealtime as _,
            symbolicSampleSize: kSample32 as _,
            numSamples: i32::try_from(frames).expect("a block of fewer than 2^31 frames"),
            numInputs: i32::from(self.inputs > 0),
            numOutputs: i32::from(self.outputs > 0),
            inputs: &mut in_bus,
            outputs: &mut out_bus,
            inputParameterChanges: as_ptr(&input_changes),
            outputParameterChanges: as_ptr(&output_changes),
            inputEvents: event_ptr,
            outputEvents: std::ptr::null_mut(),
            processContext: std::ptr::null_mut(),
        };
        // SAFETY: every buffer and list lives until the call returns, and
        // the counts match them.
        let result = unsafe { self.processor.process(&mut data) };
        assert_eq!(result, kResultOk, "process failed");
        Vst3Block {
            outputs: outs,
            changes: output_changes.points(),
        }
    }

    /// Runs the platform's event loop for `duration`, so the wrapper's idle
    /// timer fires on this (the main) thread.
    pub fn idle(&mut self, duration: Duration) {
        pump(duration);
    }

    /// Runs the event loop until the handler has seen `event`, or `timeout`
    /// passes; returns whether it saw it.
    pub fn idle_until(&mut self, event: HandlerEvent, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.events.borrow().contains(&event) {
                return true;
            }
            pump(Duration::from_millis(20));
        }
        self.events.borrow().contains(&event)
    }

    /// Takes what the plug-in told the host since the last call.
    pub fn take_handler_events(&mut self) -> Vec<HandlerEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    /// The component's state, as a host saves it in a project.
    ///
    /// # Errors
    ///
    /// The plug-in failed to save.
    pub fn save_state(&mut self) -> Result<Vec<u8>, String> {
        let stream = ComWrapper::new(Stream::default());
        let ptr = stream.to_com_ptr::<IBStream>().ok_or("no stream")?;
        // SAFETY: the component's own call, with a stream that outlives it.
        check(unsafe { self.component.getState(ptr.as_ptr()) }, "getState")?;
        Ok(stream.bytes.borrow().clone())
    }

    /// Loads `bytes` as the component's state, and gives it to the
    /// controller too, as a host does when it opens a project.
    ///
    /// # Errors
    ///
    /// The plug-in refused the state.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), String> {
        for (what, controller) in [("setState", false), ("setComponentState", true)] {
            let stream = ComWrapper::new(Stream {
                bytes: RefCell::new(bytes.to_vec()),
                at: RefCell::new(0),
            });
            let ptr = stream.to_com_ptr::<IBStream>().ok_or("no stream")?;
            // SAFETY: the plug-in's own calls, with a stream that outlives
            // them.
            let result = unsafe {
                if controller {
                    self.controller.setComponentState(ptr.as_ptr())
                } else {
                    self.component.setState(ptr.as_ptr())
                }
            };
            // A single component, as clap-wrapper makes, is its own
            // controller, so it needn't take the state twice.
            if !(controller && result == kNotImplemented) {
                check(result, what)?;
            }
        }
        Ok(())
    }
}

impl Drop for Vst3TestHost {
    fn drop(&mut self) {
        self.deactivate();
        // SAFETY: the plug-in's own calls, the last it gets.
        unsafe {
            self.controller.setComponentHandler(std::ptr::null_mut());
            let base: Option<ComPtr<IPluginBase>> = self.component.cast();
            if let Some(base) = base {
                base.terminate();
            }
        }
    }
}

/// `result` as a `Result`, naming `call` if it failed.
fn check(result: tresult, call: &str) -> Result<(), String> {
    if result == kResultOk {
        Ok(())
    } else {
        Err(format!("{call} failed ({result})"))
    }
}

/// A NUL-terminated byte string's text.
fn c_text(bytes: &[i8]) -> String {
    let bytes: Vec<u8> = bytes
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b.to_ne_bytes()[0])
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A NUL-terminated UTF-16 string's text.
fn utf16_text(chars: &[TChar]) -> String {
    let units: Vec<u16> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| u16::from_ne_bytes(c.to_ne_bytes()))
        .collect();
    String::from_utf16_lossy(&units)
}

/// A note-on event on the first event bus.
fn note_on(frame: i32, key: i16, velocity: f32) -> Event {
    Event {
        busIndex: 0,
        sampleOffset: frame,
        ppqPosition: 0.0,
        flags: 0,
        r#type: kNoteOnEvent as _,
        __field0: Event__type0 {
            noteOn: NoteOnEvent {
                channel: 0,
                pitch: key,
                tuning: 0.0,
                velocity,
                length: 0,
                noteId: -1,
            },
        },
    }
}

/// Runs this thread's event loop for `duration`.
fn pump(duration: Duration) {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: running the current thread's run loop in its default mode.
        unsafe { mac::CFRunLoopRunInMode(mac::kCFRunLoopDefaultMode, duration.as_secs_f64(), 0) };
    }
    #[cfg(windows)]
    {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            // SAFETY: the usual message loop, with a message buffer of the
            // size Windows writes.
            unsafe {
                let mut msg: win::Msg = std::mem::zeroed();
                while win::PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, 1) != 0 {
                    win::TranslateMessage(&msg);
                    win::DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    std::thread::sleep(duration);
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub static kCFRunLoopDefaultMode: *const c_void;
        pub fn CFRunLoopRunInMode(
            mode: *const c_void,
            seconds: f64,
            return_after_source: u8,
        ) -> i32;
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    #[repr(C)]
    pub struct Msg {
        hwnd: *mut c_void,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt: [i32; 2],
        private: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn PeekMessageW(
            msg: *mut Msg,
            hwnd: *mut c_void,
            min: u32,
            max: u32,
            remove: u32,
        ) -> i32;
        pub fn TranslateMessage(msg: *const Msg) -> i32;
        pub fn DispatchMessageW(msg: *const Msg) -> isize;
    }
}

/// The host's context for the plug-in.
struct HostApplication;

impl Class for HostApplication {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApplication {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        let text: Vec<u16> = "plug-in kit tests".encode_utf16().collect();
        // SAFETY: the plug-in passes a String128 to fill.
        let name = unsafe { &mut *name };
        name.fill(0);
        for (to, from) in name.iter_mut().zip(text.iter().take(127)) {
            *to = *from as TChar;
        }
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        _cid: *mut TUID,
        _iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if !obj.is_null() {
            // SAFETY: the plug-in passes a place for the object.
            unsafe { *obj = std::ptr::null_mut() };
        }
        kResultFalse
    }
}

/// Records what the plug-in tells the host.
struct ComponentHandler {
    events: Rc<RefCell<Vec<HandlerEvent>>>,
}

impl Class for ComponentHandler {
    type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl IComponentHandlerTrait for ComponentHandler {
    unsafe fn beginEdit(&self, id: ParamID) -> tresult {
        self.events.borrow_mut().push(HandlerEvent::Begin(id));
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        self.events
            .borrow_mut()
            .push(HandlerEvent::Perform(id, value));
        kResultOk
    }

    unsafe fn endEdit(&self, id: ParamID) -> tresult {
        self.events.borrow_mut().push(HandlerEvent::End(id));
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        self.events.borrow_mut().push(HandlerEvent::Restart(flags));
        kResultOk
    }
}

impl IComponentHandler2Trait for ComponentHandler {
    unsafe fn setDirty(&self, state: TBool) -> tresult {
        self.events
            .borrow_mut()
            .push(HandlerEvent::Dirty(state != 0));
        kResultOk
    }

    unsafe fn requestOpenEditor(&self, _name: FIDString) -> tresult {
        kResultFalse
    }

    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

/// A block's note events.
struct EventList {
    events: Vec<Event>,
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> int32 {
        i32::try_from(self.events.len()).unwrap_or(0)
    }

    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        match usize::try_from(index).ok().and_then(|i| self.events.get(i)) {
            Some(event) if !e.is_null() => {
                // SAFETY: the plug-in passes an Event to fill.
                unsafe { *e = *event };
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn addEvent(&self, _e: *mut Event) -> tresult {
        kResultFalse
    }
}

/// A block's parameter changes, one queue per parameter.
#[derive(Default)]
struct ParameterChanges {
    queues: RefCell<Vec<ComWrapper<ParamValueQueue>>>,
}

impl ParameterChanges {
    fn add(&self, id: u32, frame: i32, value: f64) {
        let mut index = 0;
        let queue = self.queue(id, &mut index);
        queue.points.borrow_mut().push((frame, value));
    }

    fn queue(&self, id: u32, index: &mut i32) -> ComWrapper<ParamValueQueue> {
        let mut queues = self.queues.borrow_mut();
        if let Some(at) = queues.iter().position(|q| q.id == id) {
            *index = i32::try_from(at).unwrap_or(0);
            return queues[at].clone();
        }
        let queue = ComWrapper::new(ParamValueQueue {
            id,
            points: RefCell::new(Vec::new()),
        });
        *index = i32::try_from(queues.len()).unwrap_or(0);
        queues.push(queue.clone());
        queue
    }

    fn points(&self) -> Vec<(u32, i32, f64)> {
        self.queues
            .borrow()
            .iter()
            .flat_map(|q| {
                q.points
                    .borrow()
                    .iter()
                    .map(|&(frame, value)| (q.id, frame, value))
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

impl Class for ParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for ParameterChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        i32::try_from(self.queues.borrow().len()).unwrap_or(0)
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        let queues = self.queues.borrow();
        usize::try_from(index)
            .ok()
            .and_then(|i| queues.get(i))
            .and_then(|q| q.as_com_ref::<IParamValueQueue>())
            .map_or(std::ptr::null_mut(), |q| q.as_ptr())
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        if id.is_null() {
            return std::ptr::null_mut();
        }
        let mut at = 0;
        // SAFETY: the plug-in passes a parameter ID.
        let queue = self.queue(unsafe { *id }, &mut at);
        if !index.is_null() {
            // SAFETY: the plug-in passes a place for the index.
            unsafe { *index = at };
        }
        queue
            .as_com_ref::<IParamValueQueue>()
            .map_or(std::ptr::null_mut(), |q| q.as_ptr())
    }
}

/// One parameter's changes in a block.
struct ParamValueQueue {
    id: u32,
    points: RefCell<Vec<(i32, f64)>>,
}

impl Class for ParamValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamValueQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id
    }

    unsafe fn getPointCount(&self) -> int32 {
        i32::try_from(self.points.borrow().len()).unwrap_or(0)
    }

    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        let points = self.points.borrow();
        let Some(&(frame, v)) = usize::try_from(index).ok().and_then(|i| points.get(i)) else {
            return kInvalidArgument;
        };
        if offset.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plug-in passes places for the point.
        unsafe {
            *offset = frame;
            *value = v;
        }
        kResultOk
    }

    unsafe fn addPoint(&self, offset: int32, value: ParamValue, index: *mut int32) -> tresult {
        let mut points = self.points.borrow_mut();
        if !index.is_null() {
            // SAFETY: the plug-in passes a place for the index.
            unsafe { *index = i32::try_from(points.len()).unwrap_or(0) };
        }
        points.push((offset, value));
        kResultOk
    }
}

/// A stream in memory, for saving and loading state.
#[derive(Default)]
struct Stream {
    bytes: RefCell<Vec<u8>>,
    at: RefCell<usize>,
}

impl Class for Stream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for Stream {
    unsafe fn read(&self, buffer: *mut c_void, count: int32, read: *mut int32) -> tresult {
        let bytes = self.bytes.borrow();
        let mut at = self.at.borrow_mut();
        let n = usize::try_from(count)
            .unwrap_or(0)
            .min(bytes.len().saturating_sub(*at));
        if n > 0 {
            // SAFETY: the plug-in passes a buffer of `count` bytes.
            unsafe { std::ptr::copy_nonoverlapping(bytes[*at..].as_ptr(), buffer.cast(), n) };
        }
        *at += n;
        if !read.is_null() {
            // SAFETY: the plug-in passes a place for the count.
            unsafe { *read = i32::try_from(n).unwrap_or(0) };
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, count: int32, written: *mut int32) -> tresult {
        let n = usize::try_from(count).unwrap_or(0);
        let mut bytes = self.bytes.borrow_mut();
        let mut at = self.at.borrow_mut();
        if bytes.len() < *at + n {
            bytes.resize(*at + n, 0);
        }
        if n > 0 {
            // SAFETY: the plug-in passes a buffer of `count` bytes.
            unsafe { std::ptr::copy_nonoverlapping(buffer.cast(), bytes[*at..].as_mut_ptr(), n) };
        }
        *at += n;
        if !written.is_null() {
            // SAFETY: the plug-in passes a place for the count.
            unsafe { *written = count };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let len = i64::try_from(self.bytes.borrow().len()).unwrap_or(i64::MAX);
        let mut at = self.at.borrow_mut();
        let base = match mode {
            0 => 0,
            1 => i64::try_from(*at).unwrap_or(0),
            2 => len,
            _ => return kInvalidArgument,
        };
        let target = base + pos;
        if target < 0 {
            return kInvalidArgument;
        }
        *at = usize::try_from(target).unwrap_or(0);
        if !result.is_null() {
            // SAFETY: the plug-in passes a place for the position.
            unsafe { *result = target };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plug-in passes a place for the position.
        unsafe { *pos = i64::try_from(*self.at.borrow()).unwrap_or(0) };
        kResultOk
    }
}
