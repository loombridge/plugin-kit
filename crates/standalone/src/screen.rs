//! The room the screen leaves the app's window, on macOS.
//!
//! AppKit keeps a window on its screen: asked for more than fits, it
//! takes what fits, but baseview goes on reporting the size it asked for.
//! So the window asks for no more than the screen holds, and the editor is
//! fitted to that.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_app_kit::NSView;

/// The largest content size, in points, the window holding `view` can
/// have on its screen, if it's on one.
///
/// # Safety
///
/// `view` is an open `NSView`, and this runs on the main thread.
pub unsafe fn room(view: NonNull<c_void>) -> Option<(f64, f64)> {
    // SAFETY: the caller promises `view` is a live NSView.
    let view: &NSView = unsafe { view.cast::<NSView>().as_ref() };
    let window = view.window()?;
    let screen = window.screen()?;
    let content = window.contentRectForFrameRect(screen.visibleFrame());
    Some((content.size.width, content.size.height))
}
