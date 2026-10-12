//! The standalone app's audio loop and device handling, driven through the
//! fake backend: notes and edits reach the processor, the loop stays off
//! the heap, and the app keeps playing through device changes.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use lspk_params::{ParamDescriptor, ParamId, ParamUnit};
use lspk_plugin::{
    AudioLayout, Buffers, Event, Events, Host, NotePorts, ParamValues, Plugin, PluginError,
    PluginInfo, ProcessConfig, ProcessContext, ProcessStatus, Processor,
};
use lspk_standalone::audio::{DeviceInfo, Failure};
use lspk_standalone::fake::FakeBackend;
use lspk_standalone::settings::Settings;
use lspk_standalone::{Standalone, Status};

lspk_rt::install_trap!();

const LEVEL: ParamId = ParamId(1);

thread_local! {
    /// The configuration each test plug-in was last activated with.
    static ACTIVATED: RefCell<Vec<ProcessConfig>> = const { RefCell::new(Vec::new()) };
}

fn level() -> Vec<ParamDescriptor> {
    vec![ParamDescriptor::continuous(
        LEVEL,
        "Level",
        (0.0, 1.0),
        0.5,
        ParamUnit::None,
    )]
}

/// An instrument that plays a constant `LEVEL` while any note is held, and
/// keeps a byte of state of its own.
struct Drone {
    own: u8,
}

struct DroneProcessor {
    level: f32,
    held: u32,
}

impl Plugin for Drone {
    type Processor = DroneProcessor;
    const INFO: PluginInfo = PluginInfo::new("com.example.drone", "Drone")
        .audio(AudioLayout::instrument(2))
        .notes(NotePorts::INPUT);

    fn new(_host: Host) -> Self {
        Self { own: 0 }
    }

    fn params(&self) -> Vec<ParamDescriptor> {
        level()
    }

    fn activate(
        &mut self,
        config: &ProcessConfig,
        values: &ParamValues,
    ) -> Result<DroneProcessor, PluginError> {
        ACTIVATED.with(|a| a.borrow_mut().push(*config));
        Ok(DroneProcessor {
            level: values.get(LEVEL).unwrap_or(0.0) as f32,
            held: 0,
        })
    }

    fn save_state(&self) -> Vec<u8> {
        vec![self.own]
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        match data {
            [own] => {
                self.own = *own;
                Ok(())
            }
            _ => Err(PluginError::new("one byte")),
        }
    }
}

impl Processor for DroneProcessor {
    fn process(
        &mut self,
        audio: &mut Buffers<'_>,
        events: &Events<'_>,
        _cx: &mut ProcessContext<'_>,
    ) -> ProcessStatus {
        for event in events.iter() {
            match event {
                Event::NoteOn { .. } => self.held += 1,
                Event::NoteOff { .. } => self.held = self.held.saturating_sub(1),
                Event::Param { value, .. } => self.level = value as f32,
                _ => {}
            }
        }
        let out = if self.held > 0 { self.level } else { 0.0 };
        for c in 0..audio.channels() {
            audio.channel_mut(c).fill(out);
        }
        if self.held > 0 {
            ProcessStatus::Continue
        } else {
            ProcessStatus::Sleep
        }
    }

    fn flush(&mut self, _events: &Events<'_>, _cx: &mut ProcessContext<'_>) {}
}

/// An effect that scales its input by `LEVEL`.
struct Scale;

struct ScaleProcessor {
    level: f32,
}

impl Plugin for Scale {
    type Processor = ScaleProcessor;
    const INFO: PluginInfo =
        PluginInfo::new("com.example.scale", "Scale").audio(AudioLayout::effect(2));

    fn new(_host: Host) -> Self {
        Self
    }

    fn params(&self) -> Vec<ParamDescriptor> {
        level()
    }

    fn activate(
        &mut self,
        _config: &ProcessConfig,
        values: &ParamValues,
    ) -> Result<ScaleProcessor, PluginError> {
        Ok(ScaleProcessor {
            level: values.get(LEVEL).unwrap_or(0.0) as f32,
        })
    }
}

impl Processor for ScaleProcessor {
    fn process(
        &mut self,
        audio: &mut Buffers<'_>,
        events: &Events<'_>,
        _cx: &mut ProcessContext<'_>,
    ) -> ProcessStatus {
        for event in events.iter() {
            if let Event::Param { value, .. } = event {
                self.level = value as f32;
            }
        }
        for c in 0..audio.channels() {
            audio
                .channel_mut(c)
                .iter_mut()
                .for_each(|s| *s *= self.level);
        }
        ProcessStatus::ContinueIfNotQuiet
    }

    fn flush(&mut self, _events: &Events<'_>, _cx: &mut ProcessContext<'_>) {}
}

fn devices() -> Vec<DeviceInfo> {
    vec![
        FakeBackend::device("built-in", true),
        FakeBackend::device("interface", false),
    ]
}

fn app<P: Plugin>(fake: &FakeBackend, settings: Settings) -> Standalone<P> {
    let mut app = Standalone::<P>::new(Box::new(fake.clone()), settings, None).expect("an app");
    app.set_timing(Duration::from_millis(50), Duration::from_millis(50));
    app
}

fn wait() {
    std::thread::sleep(Duration::from_millis(60));
}

#[test]
fn an_instrument_plays_midi_and_edits_from_the_main_thread() {
    let fake = FakeBackend::new(devices());
    let mut app = app::<Drone>(&fake, Settings::default());
    app.start();
    let info = fake.running().expect("a stream on the default device");
    assert_eq!(info.output_id, "built-in");
    assert_eq!((info.sample_rate, info.input_channels), (48_000, 0));
    assert!(matches!(app.status(), Status::Running(_)));

    assert!(fake.render(64).expect("a buffer").iter().all(|s| *s == 0.0));
    app.midi().send([0x90, 60, 100]);
    let out = fake.render(64).expect("a buffer");
    assert!(
        out.iter().all(|s| *s == 0.5),
        "the held note plays at the level"
    );
    assert_eq!(app.meter().take(), 0.5);

    app.host().edit(LEVEL, 0.25).expect("an edit");
    assert!(
        fake.render(64)
            .expect("a buffer")
            .iter()
            .all(|s| *s == 0.25)
    );

    app.midi().send([0x80, 60, 0]);
    assert!(fake.render(64).expect("a buffer").iter().all(|s| *s == 0.0));
}

#[test]
fn the_audio_loop_stays_off_the_heap() {
    let fake = FakeBackend::new(devices());
    let mut app = app::<Drone>(&fake, Settings::default());
    app.start();
    let input = vec![0.0f32; 4096 * 2];
    let mut output = vec![0.0f32; 4096 * 2];
    // A first buffer outside the section, as a device's first callback does
    // its thread's set-up.
    assert!(fake.render_into(&input[..128], &mut output[..128]));
    app.midi().send([0x90, 64, 90]);
    app.host().edit(LEVEL, 0.75).expect("an edit");
    // Longer than the processor's blocks, so the loop splits it.
    let played = lspk_rt::rt_test!({ fake.render_into(&input, &mut output) });
    assert!(played);
    assert!(output.iter().all(|s| *s == 0.75));
}

#[test]
fn an_effect_processes_the_input_device() {
    let fake = FakeBackend::new(devices());
    let mut app = app::<Scale>(&fake, Settings::default());
    app.start();
    let info = fake.running().expect("a stream");
    assert_eq!(info.input_channels, 2);
    let input: Vec<f32> = (0..128).map(|i| (i as f32 / 64.0) - 1.0).collect();
    let mut output = vec![0.0; 128];
    assert!(fake.render_into(&input, &mut output));
    for (out, inp) in output.iter().zip(&input) {
        assert_eq!(*out, inp * 0.5);
    }
}

#[test]
fn a_lost_device_falls_back_to_the_default_and_returns_when_it_does() {
    let fake = FakeBackend::new(devices());
    let settings = Settings {
        output: Some("interface".into()),
        ..Settings::default()
    };
    let mut app = app::<Drone>(&fake, settings);
    app.start();
    assert_eq!(fake.running().expect("a stream").output_id, "interface");
    app.midi().send([0x90, 60, 100]);
    assert!(fake.render(32).expect("a buffer")[0] > 0.0);

    // Unplugged: the stream fails, and the app reopens on the default.
    fake.set_devices(vec![FakeBackend::device("built-in", true)]);
    assert!(fake.render(32).is_none());
    app.poll();
    let info = fake.running().expect("a stream on the fallback");
    assert_eq!((info.output_id.as_str(), info.fallback), ("built-in", true));
    // A new processor plays on, through the same queues.
    app.midi().send([0x90, 62, 100]);
    assert!(fake.render(32).expect("a buffer")[0] > 0.0);
    assert_eq!(app.settings().output.as_deref(), Some("interface"));

    // Plugged back in: once the app looks again, it moves back.
    fake.set_devices(devices());
    let _ = fake.render(32);
    wait();
    let _ = fake.render(32);
    app.poll();
    let info = fake.running().expect("a stream");
    assert_eq!(
        (info.output_id.as_str(), info.fallback),
        ("interface", false)
    );
    assert_eq!(fake.opened(), 3);
}

#[test]
fn a_changed_or_stalled_device_is_reopened() {
    let fake = FakeBackend::new(devices());
    let mut app = app::<Drone>(&fake, Settings::default());
    app.start();
    fake.fail(Failure::Invalidated);
    app.poll();
    assert_eq!(fake.opened(), 2);
    assert!(fake.render(16).is_some());

    // No callbacks for longer than the stall limit.
    app.poll();
    wait();
    app.poll();
    assert_eq!(fake.opened(), 3);
    assert!(matches!(app.status(), Status::Running(_)));
}

#[test]
fn with_no_device_the_app_waits_and_starts_when_one_appears() {
    let fake = FakeBackend::new(Vec::new());
    let mut app = app::<Drone>(&fake, Settings::default());
    app.start();
    assert!(matches!(app.status(), Status::Waiting(why) if why.contains("no output device")));
    app.host().edit(LEVEL, 1.0).expect("an edit while stopped");
    app.poll();
    fake.set_devices(devices());
    wait();
    app.poll();
    assert!(matches!(app.status(), Status::Running(_)));
    app.midi().send([0x90, 60, 100]);
    assert_eq!(fake.render(8).expect("a buffer")[0], 1.0);
}

#[test]
fn new_settings_reopen_the_device_and_reactivate_the_plugin() {
    let fake = FakeBackend::new(devices());
    let mut app = app::<Drone>(&fake, Settings::default());
    app.start();
    let mut settings = app.settings().clone();
    settings.sample_rate = Some(96_000);
    settings.buffer_size = Some(256);
    app.set_settings(settings);
    let info = fake.running().expect("a stream");
    assert_eq!((info.sample_rate, info.max_frames), (96_000, 256));
    let last = ACTIVATED.with(|a| *a.borrow().last().expect("activated"));
    assert_eq!((last.sample_rate, last.max_frames), (96_000.0, 256));

    // A rate the device can't run: the app says why and waits.
    let mut settings = app.settings().clone();
    settings.sample_rate = Some(12_345);
    app.set_settings(settings);
    assert!(matches!(app.status(), Status::Waiting(why) if why.contains("12345")));
}

#[test]
fn state_round_trips_through_a_file_and_bad_files_change_nothing() {
    let dir = std::env::temp_dir().join(format!("lspk-standalone-state-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let fake = FakeBackend::new(devices());

    let mut first = Standalone::<Drone>::new(
        Box::new(fake.clone()),
        Settings::default(),
        Some(dir.clone()),
    )
    .expect("an app");
    first.start();
    first.host().edit(LEVEL, 0.125).expect("an edit");
    first.plugin().own = 7;
    let file = dir.join("patch.lspk");
    first.save_state_file(&file).expect("saved");
    first.set_settings(Settings {
        buffer_size: Some(128),
        ..Settings::default()
    });
    first.shutdown();
    drop(first);

    // The next run finds its settings and state where it left them.
    let settings = Settings::load(&dir);
    assert_eq!(settings.buffer_size, Some(128));
    let mut second = Standalone::<Drone>::new(Box::new(fake.clone()), settings, Some(dir.clone()))
        .expect("an app");
    second.restore().expect("restored");
    assert_eq!(second.values().get(LEVEL), Some(0.125));
    assert_eq!(second.plugin().own, 7);

    // A file the plug-in refuses leaves everything as it was.
    second.host().edit(LEVEL, 0.5).expect("an edit");
    let rogue = Standalone::<Scale>::new(Box::new(fake.clone()), Settings::default(), None)
        .expect("an app")
        .save_state();
    assert!(second.load_state(&rogue).is_err());
    assert_eq!(second.values().get(LEVEL), Some(0.5));
    assert!(second.open_state_file(&dir.join("missing.lspk")).is_err());

    // While playing, a loaded state reaches the processor.
    second.start();
    second.open_state_file(&file).expect("opened");
    second.midi().send([0x90, 60, 100]);
    assert_eq!(fake.render(8).expect("a buffer")[0], 0.125);
    drop(second);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_plugin_asks_for_callbacks_and_restarts() {
    struct Asker {
        host: Host,
        calls: Rc<RefCell<u32>>,
    }
    struct Quiet;
    impl Processor for Quiet {
        fn process(
            &mut self,
            _audio: &mut Buffers<'_>,
            _events: &Events<'_>,
            _cx: &mut ProcessContext<'_>,
        ) -> ProcessStatus {
            ProcessStatus::Sleep
        }
        fn flush(&mut self, _events: &Events<'_>, _cx: &mut ProcessContext<'_>) {}
    }
    thread_local! {
        static CALLS: Rc<RefCell<u32>> = Rc::new(RefCell::new(0));
    }
    impl Plugin for Asker {
        type Processor = Quiet;
        const INFO: PluginInfo = PluginInfo::new("com.example.asker", "Asker");
        fn new(host: Host) -> Self {
            Self {
                host,
                calls: CALLS.with(Rc::clone),
            }
        }
        fn params(&self) -> Vec<ParamDescriptor> {
            Vec::new()
        }
        fn activate(&mut self, _: &ProcessConfig, _: &ParamValues) -> Result<Quiet, PluginError> {
            Ok(Quiet)
        }
        fn on_main_thread(&mut self) {
            *self.calls.borrow_mut() += 1;
            self.host.request_restart();
        }
    }

    let fake = FakeBackend::new(devices());
    let mut app = app::<Asker>(&fake, Settings::default());
    app.start();
    app.plugin().host.request_callback();
    app.poll();
    assert_eq!(CALLS.with(|c| *c.borrow()), 1);
    app.poll();
    assert_eq!(fake.opened(), 2, "the restart reopened the stream");

    // The waker asks for the same callback from any thread.
    let waker = app.plugin().host.waker().expect("a waker");
    std::thread::spawn(move || waker())
        .join()
        .expect("a thread");
    app.poll();
    assert_eq!(CALLS.with(|c| *c.borrow()), 2);
    assert_eq!(app.plugin().host.host_name().as_deref(), Some("Standalone"));
}

#[test]
fn the_editor_opens_floating_and_its_changes_reach_the_processor() {
    use lspk_plugin::{Editor, EditorContext, EditorSize, ParentWindow, ResizeHints, WindowApi};

    thread_local! {
        static CONTEXT: RefCell<Option<EditorContext>> = const { RefCell::new(None) };
        static CALLS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }
    fn call(name: &'static str) {
        CALLS.with(|c| c.borrow_mut().push(name));
    }
    struct Window;
    impl Editor for Window {
        fn supports(&self, api: WindowApi, floating: bool) -> bool {
            Some(api) == WindowApi::native() && floating
        }
        fn create(&mut self, _: WindowApi, _: bool, cx: EditorContext) -> Result<(), PluginError> {
            call("create");
            CONTEXT.with(|c| *c.borrow_mut() = Some(cx));
            Ok(())
        }
        fn destroy(&mut self) {
            call("destroy");
        }
        fn set_scale(&mut self, _: f64) -> bool {
            false
        }
        fn size(&self) -> EditorSize {
            EditorSize::new(100, 100)
        }
        fn resize_hints(&self) -> Option<ResizeHints> {
            None
        }
        fn adjust_size(&self, size: EditorSize) -> EditorSize {
            size
        }
        fn set_size(&mut self, _: EditorSize) -> bool {
            false
        }
        fn set_parent(&mut self, _: ParentWindow) -> Result<(), PluginError> {
            Err(PluginError::new("floating only"))
        }
        fn show(&mut self) -> Result<(), PluginError> {
            call("show");
            Ok(())
        }
        fn hide(&mut self) -> Result<(), PluginError> {
            Ok(())
        }
        fn on_main_thread(&mut self) {
            call("callback");
        }
    }
    struct Edited;
    impl Plugin for Edited {
        type Processor = DroneProcessor;
        const INFO: PluginInfo = PluginInfo::new("com.example.edited", "Edited")
            .audio(AudioLayout::instrument(2))
            .notes(NotePorts::INPUT);
        fn new(_host: Host) -> Self {
            Self
        }
        fn params(&self) -> Vec<ParamDescriptor> {
            level()
        }
        fn activate(
            &mut self,
            _: &ProcessConfig,
            values: &ParamValues,
        ) -> Result<DroneProcessor, PluginError> {
            Ok(DroneProcessor {
                level: values.get(LEVEL).unwrap_or(0.0) as f32,
                held: 0,
            })
        }
        fn editor(&mut self) -> Option<Box<dyn Editor>> {
            Some(Box::new(Window))
        }
    }

    let fake = FakeBackend::new(devices());
    let mut app = app::<Edited>(&fake, Settings::default());
    assert!(app.has_editor() && !app.editor_is_open());
    app.start();
    app.open_editor().expect("a floating window");
    assert!(app.editor_is_open());

    // A drag in the editor's window, as on X11 from its own thread.
    let cx = CONTEXT.with(|c| c.borrow().clone()).expect("a context");
    std::thread::spawn(move || {
        cx.begin_edit(LEVEL).expect("a parameter");
        cx.set_param(LEVEL, 0.875).expect("a parameter");
        cx.end_edit(LEVEL).expect("a parameter");
        cx.request_callback();
    })
    .join()
    .expect("the window's thread");
    app.poll();
    app.midi().send([0x90, 60, 100]);
    assert_eq!(fake.render(8).expect("a buffer")[0], 0.875);

    // The window closes itself; the app destroys the editor.
    CONTEXT
        .with(|c| c.borrow().clone())
        .expect("a context")
        .window_closed();
    app.poll();
    assert!(!app.editor_is_open());
    assert_eq!(
        CALLS.with(|c| c.borrow().clone()),
        ["create", "show", "callback", "destroy"]
    );
}
