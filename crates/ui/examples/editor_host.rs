//! A small CLAP host for trying plug-in editors: a `baseview` window that
//! embeds a plug-in's editor through CLAP's `gui` extension, as a DAW's
//! device window does.
//!
//! ```sh
//! # A test plug-in built in, with every kind of control:
//! cargo run -p lspk-ui --example editor_host
//! # A plug-in from a CLAP file, such as `cargo xtask bundle` makes:
//! cargo run -p lspk-ui --example editor_host -- target/bundle/lspk-example-gain.clap
//! # The scripted check: open, resize from the host, close, and reopen
//! # many times, then exit with the result:
//! cargo run -p lspk-ui --example editor_host -- --check
//! ```
//!
//! Interactively, resizing the host's window resizes the editor, the
//! editor's own resizing and zooming resize the host's window, and every
//! parameter gesture the editor sends is printed as the host hears it.
//! Closing the window closes the editor and the plug-in.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_void};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use baseview::dpi::{LogicalSize, PhysicalSize};
use baseview::{
    Event, EventStatus, HandlerError, TimerHandle, Window, WindowContext, WindowEvent,
    WindowHandler, WindowSettings, WindowSize,
};
use clack_extensions::gui::{
    GuiApiType, GuiConfiguration, GuiSize, HostGui, HostGuiImpl, PluginGui, Window as ClapWindow,
};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
    PluginParams,
};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::prelude::*;
use lspk_params::{ParamDescriptor, ParamId, ParamUnit};
use lspk_plugin::{
    AudioLayout, Buffers, Events, Host as KitHost, Plugin, PluginError, PluginInfo, ProcessConfig,
    ProcessContext, ProcessStatus, Processor,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The test plug-in: one parameter of every kind, and no sound.
struct EveryControl;

impl EveryControl {
    const ID: &str = "com.loombridge.example.every-control";

    fn params() -> Vec<ParamDescriptor> {
        vec![
            ParamDescriptor::gain(ParamId(1), "Gain", 12.0, 0.0)
                .describe("How much louder or quieter the output is."),
            ParamDescriptor::continuous(
                ParamId(2),
                "Cutoff",
                (20.0, 20_000.0),
                1_000.0,
                ParamUnit::Hertz,
            )
            .logarithmic(),
            ParamDescriptor::continuous(ParamId(3), "Mix", (0.0, 100.0), 100.0, ParamUnit::Percent),
            ParamDescriptor::toggle(ParamId(4), "Bypass", false),
            ParamDescriptor::choice(ParamId(5), "Mode", &["Clean", "Warm", "Hot"], 0),
            ParamDescriptor::integer(ParamId(6), "Voices", (1, 16), 8, ParamUnit::None),
        ]
    }
}

impl Plugin for EveryControl {
    type Processor = Silent;

    const INFO: PluginInfo = PluginInfo::new(Self::ID, "Every Control")
        .vendor("Loombridge")
        .audio(AudioLayout::effect(2));

    fn new(_host: KitHost) -> Self {
        Self
    }

    fn params(&self) -> Vec<ParamDescriptor> {
        Self::params()
    }

    fn activate(
        &mut self,
        _config: &ProcessConfig,
        _values: &lspk_plugin::ParamValues,
    ) -> Result<Silent, PluginError> {
        Ok(Silent)
    }

    fn editor(&mut self) -> Option<Box<dyn lspk_plugin::Editor>> {
        Some(Box::new(lspk_ui::generic_editor(
            Self::INFO.name,
            &Self::params(),
        )))
    }
}

/// The test plug-in's processor, which passes its input through.
struct Silent;

impl Processor for Silent {
    fn process(
        &mut self,
        _audio: &mut Buffers<'_>,
        _events: &Events<'_>,
        _cx: &mut ProcessContext<'_>,
    ) -> ProcessStatus {
        ProcessStatus::ContinueIfNotQuiet
    }

    fn flush(&mut self, _events: &Events<'_>, _cx: &mut ProcessContext<'_>) {}
}

static ENTRY: lspk_plugin::clap::EntryDescriptor = lspk_plugin::clap_entry!(EveryControl);

/// The harness as a CLAP host.
struct Harness;

impl HostHandlers for Harness {
    type Shared<'a> = Shared;
    type MainThread<'a> = MainThread;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<'_, Self>, _shared: &Shared) {
        builder.register::<HostParams>().register::<HostGui>();
    }
}

/// What the plug-in asks, from any thread.
#[derive(Default)]
struct Shared {
    callback: AtomicBool,
    flush: AtomicBool,
    resize: Mutex<Option<GuiSize>>,
    closed: AtomicBool,
}

impl SharedHandler<'_> for Shared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {
        self.callback.store(true, Ordering::Release);
    }
}

impl HostParamsImplShared for Shared {
    fn request_flush(&self) {
        self.flush.store(true, Ordering::Release);
    }
}

impl HostGuiImpl for Shared {
    fn resize_hints_changed(&self) {}

    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        *self.resize.lock().unwrap_or_else(|e| e.into_inner()) = Some(new_size);
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn closed(&self, _was_destroyed: bool) {
        self.closed.store(true, Ordering::Release);
    }
}

#[derive(Default)]
struct MainThread;

impl MainThreadHandler<'_> for MainThread {}

impl HostParamsImplMainThread for MainThread {
    fn rescan(&self, _flags: ParamRescanFlags) {}
    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

/// What the scripted check does next.
#[derive(Clone, Copy, Debug)]
enum Step {
    Open,
    /// Resizes the host's window by this much, as a person dragging its
    /// corner, and the editor with it.
    Resize(i32, i32),
    Close,
    Quit,
}

/// The host's window, holding the plug-in.
struct HostWindow {
    window: WindowContext,
    // The instance goes before the entry it came from.
    instance: RefCell<PluginInstance<Harness>>,
    _entry: PluginEntry,
    api: GuiApiType<'static>,
    open: Cell<bool>,
    script: RefCell<Vec<(u32, Step)>>,
    wait: Cell<u32>,
    check: bool,
    /// The plug-in is the built-in one, sharing this process's count of
    /// open editor windows (a loaded CLAP file has its own).
    builtin: bool,
    failures: RefCell<Vec<String>>,
    opened: Cell<u32>,
    _timer: TimerHandle,
}

impl HostWindow {
    fn new(window: WindowContext, path: Option<PathBuf>, check: bool) -> Result<Self, String> {
        let builtin = path.is_none();
        let (entry, id) = match path {
            Some(path) => {
                // SAFETY: loading a plug-in runs its code in this process,
                // as any host does; the harness loads plug-ins it's asked to.
                let entry = unsafe { PluginEntry::load(&path) }.map_err(|e| e.to_string())?;
                let id = entry
                    .get_plugin_factory()
                    .and_then(|factory| {
                        factory
                            .plugin_descriptors()
                            .find_map(|d| d.id().map(CStr::to_owned))
                    })
                    .ok_or("the CLAP file holds no plug-in")?;
                (entry, id)
            }
            None => {
                let exe = CString::new(
                    std::env::current_exe()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .as_bytes(),
                )
                .map_err(|e| e.to_string())?;
                // SAFETY: the entry is a static made by clack-plugin.
                let entry = unsafe { PluginEntry::load_from_raw(&ENTRY, &exe) }
                    .map_err(|e| e.to_string())?;
                (
                    entry,
                    CString::new(EveryControl::ID).map_err(|e| e.to_string())?,
                )
            }
        };
        let info = HostInfo::new("plug-in kit editor host", "Loombridge", "", "0.0.0")
            .map_err(|e| e.to_string())?;
        let instance = PluginInstance::<Harness>::new(
            |()| Shared::default(),
            |_| MainThread,
            &entry,
            &id,
            &info,
        )
        .map_err(|e| e.to_string())?;
        let api = GuiApiType::default_for_current_platform().ok_or("no window API here")?;
        let mut script = vec![(0, Step::Open)];
        if check {
            script.extend([(15, Step::Resize(120, 80)), (10, Step::Resize(-200, -150))]);
            script.push((10, Step::Close));
            for _ in 0..20 {
                script.extend([(2, Step::Open), (4, Step::Close)]);
            }
            script.push((2, Step::Quit));
        }
        script.reverse();
        let timer = window
            .create_timer(Duration::from_millis(16))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            window,
            instance: RefCell::new(instance),
            _entry: entry,
            api,
            open: Cell::new(false),
            script: RefCell::new(script),
            wait: Cell::new(0),
            check,
            builtin,
            failures: RefCell::new(Vec::new()),
            opened: Cell::new(0),
            _timer: timer,
        })
    }

    fn fail(&self, message: String) {
        eprintln!("FAIL: {message}");
        self.failures.borrow_mut().push(message);
    }

    /// The host window's size in the window API's units.
    fn host_size(&self, size: WindowSize) -> GuiSize {
        if self.api.uses_logical_size() {
            GuiSize {
                width: size.logical.width.round() as u32,
                height: size.logical.height.round() as u32,
            }
        } else {
            GuiSize {
                width: size.physical.width,
                height: size.physical.height,
            }
        }
    }

    fn resize_host(&self, size: GuiSize) {
        let result = if self.api.uses_logical_size() {
            self.window.resize(LogicalSize::new(
                f64::from(size.width),
                f64::from(size.height),
            ))
        } else {
            self.window
                .resize(PhysicalSize::new(size.width, size.height))
        };
        if let Err(error) = result {
            self.fail(format!("couldn't resize the host's window: {error}"));
        }
    }

    #[allow(
        clippy::useless_conversion,
        reason = "an X11 window ID is a `c_ulong`, which is 32 bits on Windows"
    )]
    fn parent(&self) -> Option<ClapWindow<'static, 'static>> {
        let handle = self.window.window_handle().ok()?;
        Some(match handle.as_raw() {
            RawWindowHandle::AppKit(h) => {
                ClapWindow::from_generic_ptr(GuiApiType::COCOA, h.ns_view.as_ptr())
            }
            RawWindowHandle::Win32(h) => {
                ClapWindow::from_generic_ptr(GuiApiType::WIN32, h.hwnd.get() as *mut c_void)
            }
            RawWindowHandle::Xlib(h) => ClapWindow::from_x11_handle(h.window),
            RawWindowHandle::Xcb(h) => ClapWindow::from_x11_handle(h.window.get().into()),
            _ => return None,
        })
    }

    fn open_editor(&self) {
        let mut instance = self.instance.borrow_mut();
        let plugin = instance.plugin_handle();
        let Some(gui) = plugin.get_extension::<PluginGui>() else {
            self.fail("the plug-in has no gui extension".into());
            return;
        };
        let configuration = GuiConfiguration {
            api_type: self.api,
            is_floating: false,
        };
        if !gui.is_api_supported(&plugin, configuration) {
            self.fail("the plug-in doesn't support embedding here".into());
            return;
        }
        if gui.create(&plugin, configuration).is_err() {
            self.fail("create failed".into());
            return;
        }
        if !self.api.uses_logical_size() {
            let _ = gui.set_scale(&plugin, self.window.scale_factor());
        }
        let Some(size) = gui.get_size(&plugin) else {
            self.fail("get_size failed".into());
            return;
        };
        drop(instance);
        self.resize_host(size);
        let mut instance = self.instance.borrow_mut();
        let plugin = instance.plugin_handle();
        let Some(parent) = self.parent() else {
            self.fail("no parent window handle".into());
            return;
        };
        // SAFETY: the host's window outlives the editor, which is
        // destroyed before the window closes.
        if unsafe { gui.set_parent(&plugin, parent) }.is_err() {
            self.fail("set_parent failed".into());
            return;
        }
        if gui.show(&plugin).is_err() {
            self.fail("show failed".into());
        }
        self.open.set(true);
        self.opened.set(self.opened.get() + 1);
        println!(
            "opened the editor at {}x{} ({} windows open)",
            size.width,
            size.height,
            lspk_ui::open_windows()
        );
    }

    fn close_editor(&self) {
        if !self.open.replace(false) {
            return;
        }
        let mut instance = self.instance.borrow_mut();
        let plugin = instance.plugin_handle();
        if let Some(gui) = plugin.get_extension::<PluginGui>() {
            let _ = gui.hide(&plugin);
            gui.destroy(&plugin);
        }
        println!(
            "closed the editor ({} windows open)",
            lspk_ui::open_windows()
        );
    }

    /// The host resizes the editor to fit its window, as hosts do when a
    /// person drags the window's corner.
    fn fit_editor(&self, size: GuiSize) -> Option<GuiSize> {
        if !self.open.get() {
            return None;
        }
        let mut instance = self.instance.borrow_mut();
        let plugin = instance.plugin_handle();
        let gui = plugin.get_extension::<PluginGui>()?;
        if !gui.can_resize(&plugin) {
            return None;
        }
        let adjusted = gui.adjust_size(&plugin, size).unwrap_or(size);
        if gui.set_size(&plugin, adjusted).is_err() {
            self.fail(format!(
                "set_size {}x{} failed",
                adjusted.width, adjusted.height
            ));
        }
        let now = gui.get_size(&plugin);
        if now != Some(adjusted) {
            self.fail(format!("asked for {adjusted:?}, the editor is {now:?}"));
        }
        Some(adjusted)
    }

    fn step(&self, step: Step) {
        match step {
            Step::Open => {
                self.open_editor();
                // The in-built plug-in shares this process's count.
                if self.check && self.builtin && lspk_ui::open_windows() != 1 {
                    self.fail(format!(
                        "{} windows open after opening",
                        lspk_ui::open_windows()
                    ));
                }
            }
            Step::Resize(dw, dh) => {
                let now = self.host_size(self.window.size());
                let to = GuiSize {
                    width: now.width.saturating_add_signed(dw),
                    height: now.height.saturating_add_signed(dh),
                };
                println!("the host resizes its window to {}x{}", to.width, to.height);
                if let Some(fitted) = self.fit_editor(to) {
                    println!("the editor took {}x{}", fitted.width, fitted.height);
                    self.resize_host(fitted);
                }
            }
            Step::Close => {
                self.close_editor();
                if self.check && self.builtin && lspk_ui::open_windows() != 0 {
                    self.fail(format!(
                        "{} windows open after closing",
                        lspk_ui::open_windows()
                    ));
                }
            }
            Step::Quit => self.window.request_close(),
        }
    }

    /// Answers the plug-in's requests: callbacks, parameter flushes (where
    /// its gestures arrive), resizes, and a window that closed itself.
    fn serve(&self) {
        let mut instance = self.instance.borrow_mut();
        let shared = instance.access_shared_handler(|shared| {
            (
                shared.callback.swap(false, Ordering::AcqRel),
                shared.flush.swap(false, Ordering::AcqRel),
                shared
                    .resize
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take(),
                shared.closed.swap(false, Ordering::AcqRel),
            )
        });
        let (callback, flush, resize, closed) = shared;
        if callback {
            instance.call_on_main_thread_callback();
        }
        if flush {
            let plugin = instance.plugin_handle();
            if let Some(params) = plugin.get_extension::<PluginParams>() {
                let input_buffer = EventBuffer::new();
                let mut output_buffer = EventBuffer::with_capacity(256);
                let input = InputEvents::from_buffer(&input_buffer);
                let mut output = OutputEvents::from_buffer(&mut output_buffer);
                if let Some(mut inactive) = instance.inactive_plugin_handle() {
                    params.flush(&mut inactive, &input, &mut output);
                }
                for event in output_buffer.iter() {
                    match event.as_core_event() {
                        Some(CoreEventSpace::ParamGestureBegin(e)) => {
                            println!(
                                "gesture begins on parameter {:?}",
                                e.param_id().map(|id| id.get())
                            );
                        }
                        Some(CoreEventSpace::ParamValue(e)) => println!(
                            "  parameter {:?} = {:.4}",
                            e.param_id().map(|id| id.get()),
                            e.value()
                        ),
                        Some(CoreEventSpace::ParamGestureEnd(e)) => {
                            println!(
                                "gesture ends on parameter {:?}",
                                e.param_id().map(|id| id.get())
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
        drop(instance);
        if let Some(size) = resize {
            println!("the editor asks for {}x{}", size.width, size.height);
            self.resize_host(size);
        }
        if closed {
            self.close_editor();
        }
    }
}

impl WindowHandler for HostWindow {
    fn draw(&self) -> Result<(), HandlerError> {
        Ok(())
    }

    fn resized(&self, new_size: WindowSize) -> Result<(), HandlerError> {
        // A person resizing the host's window resizes the editor.
        if !self.check && self.open.get() {
            let to = self.host_size(new_size);
            if let Some(fitted) = self.fit_editor(to)
                && fitted != to
            {
                self.resize_host(fitted);
            }
        }
        Ok(())
    }

    fn on_event(&self, event: Event) -> EventStatus {
        if let Event::Window(WindowEvent::WillClose) = event {
            self.close_editor();
        }
        EventStatus::Ignored
    }

    fn on_timer(&self, _timer: &TimerHandle) {
        self.serve();
        if self.wait.get() > 0 {
            self.wait.set(self.wait.get() - 1);
            return;
        }
        let next = self.script.borrow_mut().pop();
        if let Some((_, step)) = next {
            self.step(step);
            if let Some((wait, _)) = self.script.borrow().last() {
                self.wait.set(*wait);
            }
        }
    }
}

impl Drop for HostWindow {
    fn drop(&mut self) {
        self.close_editor();
        if self.check {
            let failures = self.failures.borrow();
            if failures.is_empty() && self.opened.get() == 21 {
                println!(
                    "check passed: opened and closed the editor 21 times, resized it from the \
                     host twice, {} windows left open",
                    lspk_ui::open_windows()
                );
                // The built-in plug-in's windows share this process's
                // baseview, whose run loop can outlast the host's window.
                std::process::exit(0);
            } else {
                println!("check FAILED ({} opened): {failures:?}", self.opened.get());
                std::process::exit(1);
            }
        }
    }
}

fn main() {
    let mut check = false;
    let mut path = None;
    for arg in std::env::args().skip(1) {
        if arg == "--check" {
            check = true;
        } else {
            path = Some(PathBuf::from(arg));
        }
    }
    let settings = WindowSettings::new()
        .with_title("plug-in kit editor host")
        .with_size(LogicalSize::new(640.0, 400.0))
        .with_resizable(true);
    let window = Window::create(settings, move |window| {
        HostWindow::new(window, path, check).map_err(|error| HandlerError::from_boxed(error.into()))
    })
    .expect("the host's window opens");
    window.run_until_closed().expect("the host's window runs");
}
