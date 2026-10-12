//! A lock-free logger for real-time threads.
//!
//! A real-time thread can't print: writing to a terminal or file blocks, and
//! formatting allocates. Instead, [`rt_log!`](crate::rt_log) copies a
//! fixed-size [`Record`] (a level, a `'static` message with `{}`
//! placeholders, and up to [`MAX_ARGS`] numbers or `'static` strings) into a
//! wait-free queue that belongs to the calling thread. A drain on an ordinary
//! thread ([`Drain`], or [`drain`] called by hand) takes the records out,
//! formats them, and hands them to a sink such as standard error.
//!
//! Each thread registers once, during set-up, which allocates its queue
//! ([`register_current_thread`]; [`prepare_current_thread`] does it). After
//! that, logging is a thread-local lookup, a clock read, and a queue push. A
//! full queue drops the record and counts it ([`dropped`]), so a thread that
//! logs too much loses lines rather than time.
//!
//! A thread an audio API owns can't register: the program's first code on it
//! is a device callback, already under a deadline. Code there logs to a
//! [`Writer`] made beforehand, which needs nothing from the thread.
//!
//! [`prepare_current_thread`]: crate::thread::prepare_current_thread

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer, RingBuffer};

/// The most arguments a record carries; more are left out.
pub const MAX_ARGS: usize = 4;

/// How many records each thread's queue holds before it drops new ones.
pub const QUEUE_CAPACITY: usize = 256;

/// How much a record matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// Something failed.
    Error,
    /// Something went wrong but the work carried on.
    Warn,
    /// Something worth knowing.
    Info,
    /// Detail for whoever is debugging.
    Debug,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Error => "error",
            Self::Warn => "warning",
            Self::Info => "info",
            Self::Debug => "debug",
        })
    }
}

/// One argument of a record: a value that can be copied without allocating.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Arg {
    /// An unsigned integer.
    U64(u64),
    /// A signed integer.
    I64(i64),
    /// A float.
    F64(f64),
    /// A flag.
    Bool(bool),
    /// Fixed text.
    Str(&'static str),
    /// A length of time, shown in milliseconds.
    Duration(Duration),
}

impl fmt::Display for Arg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::U64(v) => write!(f, "{v}"),
            Self::I64(v) => write!(f, "{v}"),
            Self::F64(v) => write!(f, "{v}"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::Str(v) => f.write_str(v),
            Self::Duration(v) => write!(f, "{:.3} ms", v.as_secs_f64() * 1000.0),
        }
    }
}

macro_rules! arg_from {
    ($variant:ident: $($t:ty),*) => {$(
        impl From<$t> for Arg {
            fn from(value: $t) -> Self {
                Self::$variant(value.into())
            }
        }
    )*};
}
arg_from!(U64: u8, u16, u32, u64);
arg_from!(I64: i8, i16, i32, i64);
arg_from!(F64: f32, f64);
arg_from!(Bool: bool);
arg_from!(Str: &'static str);
arg_from!(Duration: Duration);

impl From<usize> for Arg {
    fn from(value: usize) -> Self {
        Self::U64(value as u64)
    }
}

impl From<isize> for Arg {
    fn from(value: isize) -> Self {
        Self::I64(value as i64)
    }
}

/// One log line, before formatting.
#[derive(Debug, Clone, Copy)]
pub struct Record {
    /// How much it matters.
    pub level: Level,
    /// Where it came from: the module path of the [`rt_log!`](crate::rt_log)
    /// call.
    pub target: &'static str,
    /// The message, with a `{}` for each argument.
    pub message: &'static str,
    /// The [thread serial](crate::thread::serial) of the logging thread.
    pub thread: u64,
    /// When it was logged.
    pub at: Instant,
    args: [Arg; MAX_ARGS],
    len: u8,
}

impl Record {
    /// A record logged now, on this thread. Real-time safe.
    pub fn new(level: Level, target: &'static str, message: &'static str, args: &[Arg]) -> Self {
        let mut stored = [Arg::Bool(false); MAX_ARGS];
        let len = args.len().min(MAX_ARGS);
        stored[..len].copy_from_slice(&args[..len]);
        Self {
            level,
            target,
            message,
            thread: crate::thread::serial(),
            at: Instant::now(),
            args: stored,
            len: len as u8,
        }
    }

    /// The arguments.
    pub fn args(&self) -> &[Arg] {
        &self.args[..usize::from(self.len)]
    }

    /// The message with its arguments filled in. `{{` and `}}` stand for
    /// braces; a placeholder without an argument stays as `{}`.
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.message.len() + 16);
        let mut args = self.args().iter();
        let mut rest = self.message;
        while let Some(at) = rest.find(['{', '}']) {
            out.push_str(&rest[..at]);
            let tail = &rest[at..];
            if let Some(after) = tail.strip_prefix("{{").or_else(|| tail.strip_prefix("}}")) {
                out.push_str(&tail[..1]);
                rest = after;
            } else if let Some(after) = tail.strip_prefix("{}") {
                match args.next() {
                    Some(arg) => out.push_str(&arg.to_string()),
                    None => out.push_str("{}"),
                }
                rest = after;
            } else {
                out.push_str(&tail[..1]);
                rest = &tail[1..];
            }
        }
        out.push_str(rest);
        out
    }
}

impl fmt::Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} ({})", self.level, self.text(), self.target)
    }
}

/// Logs a [`Record`] from any thread without blocking or allocating.
///
/// ```
/// use lspk_rt::log::Level;
///
/// lspk_rt::log::register_current_thread();
/// let frames = 128u32;
/// lspk_rt::rt_log!(Level::Warn, "block of {} frames ran late by {}", frames, std::time::Duration::from_micros(250));
/// ```
///
/// Evaluates to `true` if the record was queued, `false` if it was dropped
/// because the thread isn't registered or its queue is full.
///
/// Code that may run on more than one thread over its life, such as an audio
/// callback a backend can move, logs to a [`Writer`] it owns instead, with
/// `to:`. That needs no registration on the thread, so it works on a thread
/// an audio API owns from its first block:
///
/// ```
/// use lspk_rt::log::{Level, Writer};
///
/// let mut log = Writer::new();
/// lspk_rt::rt_log!(to: log, Level::Info, "{} frames", 128u32);
/// ```
#[macro_export]
macro_rules! rt_log {
    (to: $writer:expr, $level:expr, $message:literal $(, $arg:expr)* $(,)?) => {
        $writer.log($crate::log::Record::new(
            $level,
            ::core::module_path!(),
            $message,
            &[$($crate::log::Arg::from($arg)),*],
        ))
    };
    ($level:expr, $message:literal $(, $arg:expr)* $(,)?) => {
        $crate::log::log($crate::log::Record::new(
            $level,
            ::core::module_path!(),
            $message,
            &[$($crate::log::Arg::from($arg)),*],
        ))
    };
}

struct Queue {
    consumer: Consumer<Record>,
}

static QUEUES: Mutex<Vec<Queue>> = Mutex::new(Vec::new());
static DROPPED: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static WRITER: RefCell<Option<Producer<Record>>> = const { RefCell::new(None) };
}

/// Gives the calling thread its queue, if it hasn't one. Allocates and takes
/// a lock, so call it during thread set-up, not on the real-time path.
pub fn register_current_thread() {
    let registered = WRITER
        .try_with(|writer| writer.borrow().is_some())
        .unwrap_or(true);
    if registered {
        return;
    }
    let producer = new_queue();
    let _ = WRITER.try_with(|writer| *writer.borrow_mut() = Some(producer));
}

/// Makes a queue and gives its reading end to the drain.
fn new_queue() -> Producer<Record> {
    let (producer, consumer) = RingBuffer::new(QUEUE_CAPACITY);
    let mut queues = QUEUES.lock().unwrap_or_else(PoisonError::into_inner);
    // Forget queues whose writers have gone and that hold nothing.
    queues.retain(|queue| !(queue.consumer.is_abandoned() && queue.consumer.is_empty()));
    queues.push(Queue { consumer });
    producer
}

/// Queues `record` on this thread's queue. Real-time safe: wait-free, no
/// allocation. Returns `false`, and counts the record as dropped, if this
/// thread isn't registered or its queue is full.
pub fn log(record: Record) -> bool {
    let queued = WRITER
        .try_with(|writer| match writer.try_borrow_mut() {
            Ok(mut writer) => writer
                .as_mut()
                .is_some_and(|producer| producer.push(record).is_ok()),
            Err(_) => false,
        })
        .unwrap_or(false);
    if !queued {
        DROPPED.fetch_add(1, Relaxed);
    }
    queued
}

/// A queue of its own, for code that may run on different threads over its
/// life, one at a time: an audio callback, which a backend can move to
/// another thread. Unlike [`log`], it uses no thread-local, so it works on a
/// thread nobody registered, from that thread's first instruction.
///
/// Make it off the real-time path (it allocates and takes the drain's lock);
/// [`log`](Self::log) is then real-time safe. [`rt_log!`](crate::rt_log)
/// writes to one with `to:`.
#[derive(Debug)]
pub struct Writer {
    producer: Producer<Record>,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Writer {
    /// A new queue, which [`drain`] reads with the others.
    pub fn new() -> Self {
        Self {
            producer: new_queue(),
        }
    }

    /// Queues `record`. Real-time safe: wait-free, no allocation, no
    /// thread-local. Returns `false`, and counts the record as dropped, if
    /// the queue is full.
    pub fn log(&mut self, record: Record) -> bool {
        let queued = self.producer.push(record).is_ok();
        if !queued {
            DROPPED.fetch_add(1, Relaxed);
        }
        queued
    }
}

/// Records dropped since the process started.
pub fn dropped() -> u64 {
    DROPPED.load(Relaxed)
}

/// Takes every queued record from every thread, oldest first, and passes
/// each to `sink`. Returns how many there were. Takes a lock, so it must not
/// run on a real-time thread.
pub fn drain(mut sink: impl FnMut(&Record)) -> usize {
    let mut records = Vec::new();
    {
        let mut queues = QUEUES.lock().unwrap_or_else(PoisonError::into_inner);
        for queue in queues.iter_mut() {
            while let Ok(record) = queue.consumer.pop() {
                records.push(record);
            }
        }
        queues.retain(|queue| !queue.consumer.is_abandoned());
    }
    records.sort_by_key(|record| record.at);
    for record in &records {
        sink(record);
    }
    records.len()
}

/// A thread that drains the queues every so often until it's dropped, then
/// drains once more.
#[derive(Debug)]
pub struct Drain {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drain {
    /// Starts draining every `period` into `sink`.
    pub fn spawn(
        period: Duration,
        mut sink: impl FnMut(&Record) + Send + 'static,
    ) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("lspk-rt-log".into())
            .spawn(move || {
                while !stopping.load(Relaxed) {
                    drain(&mut sink);
                    thread::park_timeout(period);
                }
                drain(&mut sink);
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Drain {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Draining takes every thread's records, so tests that drain or count
    /// a queue take turns.
    static TURN: Mutex<()> = Mutex::new(());

    fn turn() -> std::sync::MutexGuard<'static, ()> {
        TURN.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Drains and keeps only this thread's records, since other tests log
    /// from their own threads into the same global queues.
    fn drain_mine(serial: u64) -> Vec<Record> {
        let mut mine = Vec::new();
        drain(|record| {
            if record.thread == serial {
                mine.push(*record);
            }
        });
        mine
    }

    #[test]
    fn records_format_their_arguments() {
        let record = Record::new(
            Level::Info,
            "here",
            "{} of {} {{ok}} {} {}",
            &[
                3u32.into(),
                (-4i64).into(),
                true.into(),
                Duration::from_micros(1500).into(),
            ],
        );
        assert_eq!(record.text(), "3 of -4 {ok} true 1.500 ms");
        let short = Record::new(Level::Warn, "here", "{} and {}", &[Arg::Str("one")]);
        assert_eq!(short.text(), "one and {}");
        assert_eq!(short.to_string(), "warning: one and {} (here)");
    }

    #[test]
    fn extra_arguments_are_left_out() {
        let args = [Arg::U64(1); MAX_ARGS + 2];
        let record = Record::new(Level::Debug, "", "", &args);
        assert_eq!(record.args().len(), MAX_ARGS);
    }

    #[test]
    fn logging_from_a_real_time_section_allocates_nothing() {
        let _turn = turn();
        let (report, records) = thread::spawn(|| {
            register_current_thread();
            let serial = crate::thread::serial();
            let ((), report) = crate::trap::watch(|| {
                crate::trap::forbid(|| {
                    for block in 0..10u64 {
                        assert!(rt_log!(Level::Info, "block {} at {} Hz", block, 48_000u32));
                    }
                })
            });
            (report.map(|r| r.to_string()), drain_mine(serial))
        })
        .join()
        .unwrap();
        assert_eq!(report, None);
        let texts: Vec<String> = records.iter().map(Record::text).collect();
        assert_eq!(texts.len(), 10);
        assert_eq!(texts[0], "block 0 at 48000 Hz");
        assert_eq!(texts[9], "block 9 at 48000 Hz");
        assert!(records.windows(2).all(|w| w[0].at <= w[1].at));
    }

    #[test]
    fn a_writer_logs_from_any_thread_without_registering_or_allocating() {
        let _turn = turn();
        let mut writer = Writer::new();
        let mut serials = Vec::new();
        for _ in 0..2 {
            let (back, serial, report) = thread::spawn(move || {
                let serial = crate::thread::serial();
                let (queued, report) = crate::trap::watch(|| {
                    crate::trap::forbid(|| rt_log!(to: writer, Level::Info, "from {}", serial))
                });
                assert!(queued);
                (writer, serial, report.map(|r| r.to_string()))
            })
            .join()
            .unwrap();
            writer = back;
            assert_eq!(report, None);
            serials.push(serial);
        }
        let mut texts = Vec::new();
        drain(|record| {
            if serials.contains(&record.thread) {
                texts.push(record.text());
            }
        });
        assert_eq!(
            texts,
            [
                format!("from {}", serials[0]),
                format!("from {}", serials[1])
            ]
        );
    }

    #[test]
    fn a_full_queue_drops_records_and_counts_them() {
        let _turn = turn();
        let (queued, serial) = thread::spawn(|| {
            register_current_thread();
            let queued = (0..QUEUE_CAPACITY + 10)
                .filter(|_| rt_log!(Level::Debug, "spam"))
                .count();
            (queued, crate::thread::serial())
        })
        .join()
        .unwrap();
        assert_eq!(queued, QUEUE_CAPACITY);
        assert!(dropped() >= 10);
        assert_eq!(
            drain_mine(serial).len(),
            QUEUE_CAPACITY,
            "an exited thread's records are kept"
        );
    }

    #[test]
    fn an_unregistered_thread_drops_its_records() {
        let queued = thread::spawn(|| rt_log!(Level::Error, "nobody listens"))
            .join()
            .unwrap();
        assert!(!queued);
    }

    #[test]
    fn a_drain_thread_delivers_records_to_its_sink() {
        let _turn = turn();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let serial = thread::spawn(|| {
            register_current_thread();
            rt_log!(Level::Info, "hello from {}", "a real-time thread");
            crate::thread::serial()
        })
        .join()
        .unwrap();
        let drain = Drain::spawn(Duration::from_millis(5), move |record| {
            if record.thread == serial {
                sink_seen.lock().unwrap().push(record.text());
            }
        })
        .unwrap();
        drop(drain); // drains once more on the way out
        assert_eq!(*seen.lock().unwrap(), ["hello from a real-time thread"]);
    }
}
