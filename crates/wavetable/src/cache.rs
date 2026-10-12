//! The process-wide table cache: every instance in a process that loads the
//! same table shares one [`Table`], mipmaps and all.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Deref;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use crate::{Error, Table, Wavetable};

/// A table shared through the [`Cache`]: cheap to clone and to send to the
/// audio thread.
///
/// Dropping one, on any thread, never frees the table: the cache holds a
/// reference of its own until [`Cache::collect`] runs, off the audio thread,
/// and finds no handle left. So an instance may drop its handle on the audio
/// thread when it swaps tables.
#[derive(Clone, Debug)]
pub struct SharedTable(Arc<Table>);

impl SharedTable {
    /// Whether `self` and `other` are the same table in memory.
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Deref for SharedTable {
    type Target = Table;

    fn deref(&self) -> &Table {
        &self.0
    }
}

#[derive(Default)]
struct Entries {
    /// Every table, by a hash of its drawn frames and interpolation. The
    /// cache's own reference to each table is here.
    by_content: HashMap<u64, Vec<Arc<Table>>>,
    /// Tables loaded by name (a path, or a factory table's name).
    by_name: HashMap<String, Weak<Table>>,
}

/// Shares built tables between instances in a process.
///
/// Every method may lock, allocate and build tables, so none of them may
/// run on the audio thread; the [`SharedTable`]s it returns are read there.
/// A table no instance holds any more is freed by [`collect`](Self::collect),
/// which every load runs first.
#[derive(Default)]
pub struct Cache {
    entries: Mutex<Entries>,
}

impl Cache {
    /// An empty cache. Most callers want [`Cache::global`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The process-wide cache.
    pub fn global() -> &'static Self {
        static GLOBAL: OnceLock<Cache> = OnceLock::new();
        GLOBAL.get_or_init(Self::new)
    }

    fn lock(&self) -> MutexGuard<'_, Entries> {
        // A panic while the lock was held left the maps whole: each change
        // is one insert or retain.
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The shared table for `source`: the one already built from equal
    /// frames and interpolation, or a new one built from it.
    pub fn share(&self, source: Wavetable) -> SharedTable {
        let hash = content_hash(&source);
        self.collect();
        if let Some(found) = self.find(hash, &source) {
            return found;
        }
        // Built without the lock, so other loads go on meanwhile.
        let table = Arc::new(Table::build(source));
        let mut entries = self.lock();
        let bucket = entries.by_content.entry(hash).or_default();
        if let Some(found) = bucket.iter().find(|t| t.source() == table.source()) {
            // Another thread built it first; use that one.
            return SharedTable(found.clone());
        }
        bucket.push(table.clone());
        SharedTable(table)
    }

    /// The shared table loaded under `name`, or the one `load` reads, built
    /// and shared as [`share`](Self::share) does and remembered under
    /// `name`.
    ///
    /// # Errors
    ///
    /// Whatever `load` returns.
    pub fn load(
        &self,
        name: &str,
        load: impl FnOnce() -> Result<Wavetable, Error>,
    ) -> Result<SharedTable, Error> {
        if let Some(found) = self.lock().by_name.get(name).and_then(Weak::upgrade) {
            return Ok(SharedTable(found));
        }
        let shared = self.share(load()?);
        self.lock()
            .by_name
            .insert(name.to_owned(), Arc::downgrade(&shared.0));
        Ok(shared)
    }

    /// Frees every table no [`SharedTable`] holds, and returns how many it
    /// freed. Never call it on the audio thread.
    pub fn collect(&self) -> usize {
        let mut entries = self.lock();
        let mut freed = 0;
        // Only the cache clones from its own references, under this lock, so
        // a count of one can't rise while it's held.
        entries.by_content.retain(|_, bucket| {
            let before = bucket.len();
            bucket.retain(|t| Arc::strong_count(t) > 1);
            freed += before - bucket.len();
            !bucket.is_empty()
        });
        entries.by_name.retain(|_, t| t.strong_count() > 0);
        freed
    }

    /// How many tables it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().by_content.values().map(Vec::len).sum()
    }

    /// Whether it holds no table.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn find(&self, hash: u64, source: &Wavetable) -> Option<SharedTable> {
        self.lock()
            .by_content
            .get(&hash)?
            .iter()
            .find(|t| t.source() == source)
            .map(|t| SharedTable(t.clone()))
    }
}

/// A hash of what makes two tables play the same: their drawn frames and
/// their interpolation.
fn content_hash(source: &Wavetable) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.interpolation().hash(&mut hasher);
    for frame in source.frames() {
        for x in frame.iter() {
            x.to_bits().hash(&mut hasher);
        }
    }
    hasher.finish()
}
