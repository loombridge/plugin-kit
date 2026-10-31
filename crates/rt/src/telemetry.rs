//! A wait-free channel from an audio thread to an editor, for the live
//! values an interface draws: modulation rings, meters, scopes.
//!
//! The audio thread writes into a [`Publisher`] and an editor reads the
//! latest value from the [`Watcher`], through a [triple buffer](crate::triple):
//! neither waits for the other, and nothing allocates after [`channel`]. The
//! audio thread writes only while an editor is watching
//! ([`Watcher::watch`]), so a closed window costs one atomic load per block
//! and nothing else.
//!
//! ```
//! use lspk_rt::telemetry::channel;
//!
//! let (mut publisher, mut watcher) = channel(&0.0f32, 512);
//! assert!(!publisher.due(512), "nobody is watching");
//! watcher.watch(true);
//! assert!(publisher.due(512));
//! publisher.publish(|level| *level = 0.5);
//! assert_eq!(*watcher.latest(), 0.5);
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::triple::{Input, Output, triple_buffer};

/// A channel for values of type `T`, every slot starting as a copy of
/// `initial`, published at most every `interval` frames (see
/// [`Publisher::due`]). Allocates; make it off the audio thread.
#[must_use]
pub fn channel<T: Clone>(initial: &T, interval: usize) -> (Publisher<T>, Watcher<T>) {
    let (input, output) = triple_buffer(initial);
    let watched = Arc::new(AtomicBool::new(false));
    let publisher = Publisher {
        input,
        watched: Arc::clone(&watched),
        interval,
        // The first block after a window opens publishes at once.
        since: interval,
    };
    (publisher, Watcher { output, watched })
}

/// The audio thread's end of a [`channel`].
pub struct Publisher<T> {
    input: Input<T>,
    watched: Arc<AtomicBool>,
    interval: usize,
    since: usize,
}

impl<T> Publisher<T> {
    /// Whether an editor is watching. Everything that only the editor needs
    /// (the scope's samples, the meters' peaks) is skipped while it isn't.
    #[must_use]
    pub fn watched(&self) -> bool {
        self.watched.load(Ordering::Relaxed)
    }

    /// Counts a block of `frames` frames and says whether to
    /// [publish](Self::publish) after it: an editor is watching and at
    /// least the channel's interval has passed since the last time this
    /// said so. Call it once per block.
    pub fn due(&mut self, frames: usize) -> bool {
        if !self.watched() {
            // A window that opens later is shown a value at once.
            self.since = self.interval;
            return false;
        }
        self.since = self.since.saturating_add(frames);
        if self.since < self.interval {
            return false;
        }
        self.since = 0;
        true
    }

    /// Publishes the value `fill` makes in a slot that holds an older one,
    /// so `fill` sets everything an editor reads. Wait-free; it neither
    /// allocates nor drops a `T`.
    pub fn publish(&mut self, fill: impl FnOnce(&mut T)) {
        self.input.write_with(fill);
    }
}

/// An editor's end of a [`channel`].
pub struct Watcher<T> {
    output: Output<T>,
    watched: Arc<AtomicBool>,
}

impl<T> Watcher<T> {
    /// Tells the audio thread whether an editor is open. An editor calls it
    /// with `true` when its window shows and `false` when it hides.
    pub fn watch(&self, on: bool) {
        self.watched.store(on, Ordering::Relaxed);
    }

    /// The latest value published, or the initial value before the first.
    pub fn latest(&mut self) -> &T {
        self.output.read()
    }
}

impl<T> Drop for Watcher<T> {
    fn drop(&mut self) {
        self.watch(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_due_until_an_editor_watches() {
        let (mut publisher, watcher) = channel(&0u32, 512);
        assert!(!publisher.due(64));
        watcher.watch(true);
        assert!(publisher.due(64), "a new window is shown a value at once");
        watcher.watch(false);
        assert!(!publisher.due(64));
    }

    #[test]
    fn a_watched_channel_is_due_once_per_interval() {
        let (mut publisher, watcher) = channel(&0u32, 512);
        watcher.watch(true);
        assert!(publisher.due(128));
        let due = (0..8).filter(|_| publisher.due(128)).count();
        assert_eq!(due, 2, "1024 frames at an interval of 512");
    }

    #[test]
    fn the_editor_reads_the_latest_value() {
        let (mut publisher, mut watcher) = channel(&vec![0.0f32; 4], 1);
        assert_eq!(watcher.latest(), &[0.0; 4]);
        publisher.publish(|v| v.fill(1.0));
        publisher.publish(|v| v.fill(2.0));
        assert_eq!(watcher.latest(), &[2.0; 4]);
    }

    #[test]
    fn dropping_the_watcher_stops_the_writes() {
        let (publisher, watcher) = channel(&0u32, 1);
        watcher.watch(true);
        assert!(publisher.watched());
        drop(watcher);
        assert!(!publisher.watched());
    }

    #[test]
    fn publishing_a_buffer_stays_off_the_heap() {
        let (mut publisher, mut watcher) = channel(&vec![0.0f32; 1024], 1);
        watcher.watch(true);
        crate::rt_test!({
            for i in 0..8 {
                if publisher.due(64) {
                    publisher.publish(|v| v.fill(i as f32));
                }
            }
        });
        assert_eq!(watcher.latest()[0], 7.0);
    }
}
