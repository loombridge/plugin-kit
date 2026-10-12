//! The process-wide cache: one table, mipmaps and all, shared by every
//! instance that loads it, and never freed on the audio thread.

use std::sync::atomic::{AtomicUsize, Ordering};

use lspk_wavetable::{Cache, Interpolation, SharedTable, Wavetable};

lspk_rt::install_trap!();

fn table(seed: usize) -> Wavetable {
    Wavetable::from_fn(2, |f, n| ((n * (seed + f + 1)) % 97) as f32 / 97.0 - 0.5).unwrap()
}

#[test]
fn instances_sharing_a_table_share_one_copy() {
    let cache = Cache::new();
    let a = cache.share(table(1));
    let b = cache.share(table(1));
    assert!(a.ptr_eq(&b));
    assert_eq!(cache.len(), 1);

    // Different frames or a different interpolation are another table.
    let c = cache.share(table(2));
    let d = cache.share(table(1).with_interpolation(Interpolation::Spectral));
    assert!(!a.ptr_eq(&c) && !a.ptr_eq(&d));
    assert_eq!(cache.len(), 3);
}

#[test]
fn a_named_table_is_read_once_while_anyone_holds_it() {
    let cache = Cache::new();
    let reads = AtomicUsize::new(0);
    let load = || {
        reads.fetch_add(1, Ordering::Relaxed);
        Ok(table(3))
    };
    let first = cache.load("factory/basic", load).unwrap();
    let second = cache.load("factory/basic", load).unwrap();
    assert!(first.ptr_eq(&second));
    assert_eq!(reads.load(Ordering::Relaxed), 1);

    // The same frames under another name are still the one table.
    let renamed = cache.load("user/copy", load).unwrap();
    assert!(first.ptr_eq(&renamed));
    assert_eq!(reads.load(Ordering::Relaxed), 2);

    // Errors pass through and cache nothing.
    let failed = cache.load("missing", || Err(lspk_wavetable::Error::NoFrames));
    assert_eq!(failed.unwrap_err(), lspk_wavetable::Error::NoFrames);
    assert_eq!(cache.len(), 1);
}

#[test]
fn a_table_nobody_holds_is_freed_by_the_next_collection() {
    let cache = Cache::new();
    let held = cache.share(table(4));
    let dropped = cache.share(table(5));
    drop(dropped);
    assert_eq!(cache.len(), 2, "dropping a handle frees nothing");
    assert_eq!(cache.collect(), 1);
    assert_eq!(cache.len(), 1);
    drop(held);
    // Loading runs a collection first.
    let _other = cache.share(table(6));
    assert_eq!(cache.len(), 1);
}

#[test]
fn dropping_a_handle_on_the_audio_thread_never_frees_the_table() {
    let cache = Cache::new();
    let handle = cache.share(table(7));
    let on_audio_thread: SharedTable = handle.clone();
    drop(handle);
    // The audio thread swaps the last instance's table out and drops it:
    // the cache still holds the table, so nothing touches the heap.
    lspk_rt::rt_test!({
        let wave = on_audio_thread.wave(0, 0);
        assert!(wave.read(0).is_finite());
        drop(on_audio_thread);
    });
    assert_eq!(cache.collect(), 1);
}

#[test]
fn the_global_cache_shares_across_threads() {
    let handles: Vec<SharedTable> = std::thread::scope(|scope| {
        let spawned: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| Cache::global().share(table(8))))
            .collect();
        spawned.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(handles.windows(2).all(|w| w[0].ptr_eq(&w[1])));
    assert!(
        Cache::global()
            .load("global/eight", || Ok(table(8)))
            .unwrap()
            .ptr_eq(&handles[0])
    );
}
