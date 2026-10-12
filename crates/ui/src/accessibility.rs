//! The editor's accessibility tree, handed to the platform's screen
//! readers: VoiceOver on macOS and Narrator, NVDA and JAWS on Windows,
//! through AccessKit's adapters on the editor's own view.
//!
//! egui builds the AccessKit tree every frame (each control's role, name
//! and value, from [`controls`](crate::controls)); a small egui plug-in
//! takes the tree out after each frame and brings the screen reader's
//! requests (press, step, set a value) in before the next. On Linux the
//! tree is built and tested the same way, but no adapter hands it to
//! AT-SPI yet: AccessKit's Linux adapter brings an async D-Bus stack into
//! every plug-in, which wants a decision of its own.

use std::sync::{Arc, Mutex, PoisonError};

use baseview::WindowWaker;
use egui::accesskit::{ActionHandler, ActionRequest, ActivationHandler, TreeUpdate};

/// What the egui plug-in, the adapter and the screen reader share.
#[derive(Default)]
struct Shared {
    /// The newest tree, not yet handed to the adapter.
    update: Mutex<Option<TreeUpdate>>,
    /// The newest tree handed over, for a screen reader that starts
    /// listening later.
    last: Mutex<Option<TreeUpdate>>,
    /// The screen reader's requests, for the next frame.
    requests: Mutex<Vec<ActionRequest>>,
}

/// The egui plug-in at the tree's two ends.
struct Bridge(Arc<Shared>);

impl egui::Plugin for Bridge {
    fn debug_name(&self) -> &'static str {
        "lspk-accessibility"
    }

    fn input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        let requests = std::mem::take(
            &mut *self
                .0
                .requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        input.events.extend(
            requests
                .into_iter()
                .map(egui::Event::AccessKitActionRequest),
        );
    }

    fn output_hook(&mut self, _ctx: &egui::Context, output: &mut egui::FullOutput) {
        if let Some(update) = output.platform_output.accesskit_update.take() {
            *self.0.update.lock().unwrap_or_else(PoisonError::into_inner) = Some(update);
        }
    }
}

/// Answers a screen reader that starts listening with the newest tree.
struct Activation(Arc<Shared>);

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.0
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Queues a screen reader's request and wakes the window to act on it.
struct Actions {
    shared: Arc<Shared>,
    waker: WindowWaker,
}

impl ActionHandler for Actions {
    fn do_action(&mut self, request: ActionRequest) {
        self.shared
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
        self.waker.request_redraw();
    }
}

/// An editor window's link to the platform's screen readers.
pub(crate) struct Accessibility {
    shared: Arc<Shared>,
    adapter: platform::Adapter,
}

// SAFETY: made in the window's build on the thread the window runs on, and
// used and dropped only there, from the window's own callbacks. It's `Send`
// only because egui-baseview's app must be, to reach that thread.
unsafe impl Send for Accessibility {}

impl Accessibility {
    /// Connects the window `window`, whose egui context is `ctx`, to the
    /// platform's screen readers, where the kit has an adapter.
    pub(crate) fn connect(ctx: &egui::Context, window: &baseview::WindowContext) -> Option<Self> {
        let shared = Arc::new(Shared::default());
        let adapter = platform::Adapter::new(
            window,
            Activation(shared.clone()),
            Actions {
                shared: shared.clone(),
                waker: window.waker(),
            },
        )?;
        ctx.add_plugin(Bridge(shared.clone()));
        Some(Self { shared, adapter })
    }

    /// Hands the newest tree to the adapter.
    pub(crate) fn update(&mut self) {
        let update = self
            .shared
            .update
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(update) = update {
            *self
                .shared
                .last
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(update.clone());
            self.adapter.update(update);
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! AccessKit's macOS adapter answers three accessibility questions a
    //! view is asked: its children, its focused element, and the element at
    //! a point. AccessKit's own subclassing adapter swaps the view's class
    //! for a subclass it keeps for the process's life, which breaks
    //! baseview's view classes: baseview makes one per window and disposes
    //! of it when the window closes. Instead the three methods are added to
    //! that per-window class itself, so they go when it goes, and look the
    //! adapter up by view.

    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use std::rc::Rc;

    use egui::accesskit::{ActionHandler, ActivationHandler, TreeUpdate};
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::sel;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    /// One window's adapter and the handler it asks for a first tree.
    struct State {
        adapter: accesskit_macos::Adapter,
        activation: Box<dyn ActivationHandler>,
    }

    thread_local! {
        /// Every open editor view on this (the main) thread, by address.
        static VIEWS: RefCell<Vec<(usize, Rc<RefCell<State>>)>> = const { RefCell::new(Vec::new()) };
    }

    fn state(view: *mut c_void) -> Option<Rc<RefCell<State>>> {
        VIEWS.with(|views| {
            views
                .borrow()
                .iter()
                .find(|(address, _)| *address == view as usize)
                .map(|(_, state)| state.clone())
        })
    }

    extern "C-unwind" fn children(view: *mut c_void, _cmd: Sel) -> *mut c_void {
        let Some(state) = state(view) else {
            return null_mut();
        };
        let Ok(mut state) = state.try_borrow_mut() else {
            return null_mut();
        };
        let State {
            adapter,
            activation,
        } = &mut *state;
        adapter.view_children(activation.as_mut()).cast()
    }

    extern "C-unwind" fn focused(view: *mut c_void, _cmd: Sel) -> *mut c_void {
        let Some(state) = state(view) else {
            return null_mut();
        };
        let Ok(mut state) = state.try_borrow_mut() else {
            return null_mut();
        };
        let State {
            adapter,
            activation,
        } = &mut *state;
        adapter.focus(activation.as_mut()).cast()
    }

    extern "C-unwind" fn hit_test(
        view: *mut c_void,
        _cmd: Sel,
        point: accesskit_macos::NSPoint,
    ) -> *mut c_void {
        let Some(state) = state(view) else {
            return null_mut();
        };
        let Ok(mut state) = state.try_borrow_mut() else {
            return null_mut();
        };
        let State {
            adapter,
            activation,
        } = &mut *state;
        adapter.hit_test(point, activation.as_mut()).cast()
    }

    pub(super) struct Adapter {
        view: usize,
        state: Rc<RefCell<State>>,
    }

    impl Adapter {
        pub(super) fn new(
            window: &baseview::WindowContext,
            activation: impl ActivationHandler + 'static,
            actions: impl ActionHandler + Send + 'static,
        ) -> Option<Self> {
            let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
                return None;
            };
            let view = handle.ns_view.as_ptr();
            // SAFETY: `view` is the editor's own NSView, alive while its
            // window is open; its class is the one baseview made for this
            // window alone, so the methods added here leave with it.
            unsafe {
                let object = &*view.cast::<AnyObject>();
                let class = std::ptr::from_ref::<AnyClass>(object.class()).cast_mut();
                let methods: [(Sel, Imp, &std::ffi::CStr); 3] = [
                    (
                        sel!(accessibilityChildren),
                        std::mem::transmute::<
                            extern "C-unwind" fn(*mut c_void, Sel) -> *mut c_void,
                            Imp,
                        >(children),
                        c"@@:",
                    ),
                    (
                        sel!(accessibilityFocusedUIElement),
                        std::mem::transmute::<
                            extern "C-unwind" fn(*mut c_void, Sel) -> *mut c_void,
                            Imp,
                        >(focused),
                        c"@@:",
                    ),
                    (
                        sel!(accessibilityHitTest:),
                        std::mem::transmute::<
                            extern "C-unwind" fn(
                                *mut c_void,
                                Sel,
                                accesskit_macos::NSPoint,
                            ) -> *mut c_void,
                            Imp,
                        >(hit_test),
                        c"@@:{CGPoint=dd}",
                    ),
                ];
                for (selector, imp, types) in methods {
                    objc2::ffi::class_addMethod(class, selector, imp, types.as_ptr());
                }
            }
            // SAFETY: as above; the adapter holds the view weakly.
            let adapter = unsafe { accesskit_macos::Adapter::new(view, false, actions) };
            let state = Rc::new(RefCell::new(State {
                adapter,
                activation: Box::new(activation),
            }));
            VIEWS.with(|views| views.borrow_mut().push((view as usize, state.clone())));
            Some(Self {
                view: view as usize,
                state,
            })
        }

        pub(super) fn update(&mut self, update: TreeUpdate) {
            // The borrow ends before the events are raised, as AppKit may
            // ask the view for its children while they are.
            let events = match self.state.try_borrow_mut() {
                Ok(mut state) => state.adapter.update_if_active(|| update),
                Err(_) => None,
            };
            if let Some(events) = events {
                events.raise();
            }
        }
    }

    impl Drop for Adapter {
        fn drop(&mut self) {
            VIEWS.with(|views| views.borrow_mut().retain(|(view, _)| *view != self.view));
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use egui::accesskit::{ActionHandler, ActivationHandler, TreeUpdate};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub(super) struct Adapter(accesskit_windows::SubclassingAdapter);

    impl Adapter {
        pub(super) fn new(
            window: &baseview::WindowContext,
            activation: impl ActivationHandler + 'static,
            actions: impl ActionHandler + Send + 'static,
        ) -> Option<Self> {
            let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
                return None;
            };
            let hwnd = accesskit_windows::HWND(handle.hwnd.get() as *mut std::ffi::c_void);
            // The adapter must subclass the window before it's first shown,
            // and panics otherwise; a window already shown goes without.
            // SAFETY: `hwnd` is the editor's own window, alive while this
            // runs on its thread.
            if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd.0) } != 0
            {
                return None;
            }
            Some(Self(accesskit_windows::SubclassingAdapter::new(
                hwnd, activation, actions,
            )))
        }

        pub(super) fn update(&mut self, update: TreeUpdate) {
            if let Some(events) = self.0.update_if_active(|| update) {
                events.raise();
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use egui::accesskit::{ActionHandler, ActivationHandler, TreeUpdate};

    pub(super) struct Adapter;

    impl Adapter {
        pub(super) fn new(
            _window: &baseview::WindowContext,
            _activation: impl ActivationHandler + 'static,
            _actions: impl ActionHandler + Send + 'static,
        ) -> Option<Self> {
            None
        }

        pub(super) fn update(&mut self, _update: TreeUpdate) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bridge_takes_the_tree_out_and_requests_in() {
        let shared = Arc::new(Shared::default());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        ctx.add_plugin(Bridge(shared.clone()));
        let id = egui::Id::new("button");
        shared
            .requests
            .lock()
            .expect("unlocked")
            .push(ActionRequest {
                action: egui::accesskit::Action::Click,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: id.accesskit_id(),
                data: None,
            });
        let mut clicked = false;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let response = ui.interact(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0)),
                id,
                egui::Sense::click(),
            );
            clicked = response.clicked();
        });
        output.textures_delta.clear();
        assert!(clicked, "the request reached the frame");
        assert!(output.platform_output.accesskit_update.is_none());
        assert!(shared.update.lock().expect("unlocked").is_some());
        assert!(shared.requests.lock().expect("unlocked").is_empty());
    }
}
