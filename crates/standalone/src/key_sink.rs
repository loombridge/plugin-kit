//! The end of the app window's key handling on macOS.
//!
//! In a host, a kit editor passes on the keys it doesn't use, so the
//! host's own shortcuts work; the app's window does the same. With no host
//! behind the app, a key nothing takes reaches the end of AppKit's
//! responder chain, and AppKit sounds the system alert for it, on every
//! key the computer keyboard plays. The sink sits after the window's view
//! in the chain and takes every key that gets that far, silently. Key
//! equivalents (the menus' shortcuts) don't go through the chain, so they
//! still work.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSResponder, NSView};

define_class!(
    // SAFETY: NSResponder may be subclassed; the sink adds no ivars and
    // overrides only the key methods, which it may.
    #[unsafe(super(NSResponder))]
    #[thread_kind = MainThreadOnly]
    #[name = "LspkStandaloneKeySink"]
    pub struct KeySink;

    impl KeySink {
        #[unsafe(method(keyDown:))]
        fn key_down(&self, _event: &NSEvent) {}

        #[unsafe(method(keyUp:))]
        fn key_up(&self, _event: &NSEvent) {}
    }
);

impl KeySink {
    /// Puts a sink after `view` in the responder chain, before whatever
    /// came after it. Keep the sink while the view lives: the chain doesn't
    /// hold it.
    ///
    /// # Safety
    ///
    /// `view` is an open `NSView`, and this runs on the main thread.
    pub unsafe fn install(view: NonNull<c_void>) -> Option<Retained<Self>> {
        let mtm = MainThreadMarker::new()?;
        // SAFETY: the caller promises `view` is a live NSView.
        let view: &NSView = unsafe { view.cast::<NSView>().as_ref() };
        let sink = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSResponder's designated initialiser.
        let sink: Retained<Self> = unsafe { msg_send![super(sink), init] };
        // SAFETY: the chain is the view's own, changed on the main thread;
        // the sink outlives it, as the caller keeps it while the view lives.
        unsafe {
            let next = view.nextResponder();
            sink.setNextResponder(next.as_deref());
            view.setNextResponder(Some(&sink));
        }
        Some(sink)
    }
}
