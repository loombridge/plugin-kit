//! The engine: runs a compiled [`Program`] on the audio thread, per voice
//! and global, a block at a time.

use std::ops::Range;

use lspk_dsp::simd::{self, F32s, Kernel, Level};

use crate::CONTROL_PERIOD;
use crate::curve::lookup;
use crate::dest::{DestId, Destinations};
use crate::mailbox::ProgramReceiver;
use crate::matrix::SLOTS;
use crate::program::{Base, Config, Gain, Input, Plan, Program, Read, Route};
use crate::source::{MAX_SOURCES, Scope, SourceId, SourceKind, Sources};

/// The signals a block's sources rendered, bound by source for one
/// [`Engine::process`] call. Holds only references; making one allocates
/// nothing.
#[derive(Clone, Copy)]
pub struct Signals<'a> {
    slots: [Option<Signal<'a>>; MAX_SOURCES],
}

#[derive(Clone, Copy)]
struct Signal<'a> {
    data: &'a [f32],
    stride: usize,
}

impl Default for Signals<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Signals<'a> {
    /// No signals bound; an unbound signal reads 0.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [None; MAX_SOURCES],
        }
    }

    /// Binds `src` to `data`, frame by frame: a per-voice source's value
    /// for voice `v` at frame `k` of the block is `data[k × stride + v]`,
    /// the layout `EnvelopeBank` and `LfoBank` write (pass their stride, or
    /// their voice count); a global source's is `data[k]` (stride 1). A
    /// per-voice buffer with at least [`Engine::stride`] values a frame lets
    /// plain routes read whole rows of voices at once, which is quicker;
    /// the results are the same within rounding.
    pub fn bind(&mut self, src: SourceId, data: &'a [f32], stride: usize) {
        if let Some(slot) = self.slots.get_mut(src.index()) {
            *slot = Some(Signal {
                data,
                stride: stride.max(1),
            });
        }
    }
}

/// A stretch of a [`process`](Engine::process) call within one control
/// period.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// Its first frame, in the block.
    pub start: usize,
    /// How many frames.
    pub len: usize,
    /// How far into its control period it starts, 0 to
    /// [`CONTROL_PERIOD`] − 1.
    pub phase: usize,
    /// Whether control-rate routes were evaluated at its start: at every
    /// period's start, and where a note started a voice.
    pub tick: bool,
}

/// A note starting on a voice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteOn {
    /// The note number, 0 to 127 (fractions allowed).
    pub key: f32,
    /// The note-on velocity, 0 to 1.
    pub velocity: f32,
    /// Whether it's a legato note: the voice carries on from a note still
    /// held, so its route states aren't reset.
    pub legato: bool,
    /// The glide time, for the curves' glide (engine-core R100).
    pub glide_seconds: f32,
}

impl NoteOn {
    /// A new note, not legato.
    #[must_use]
    pub const fn new(key: f32, velocity: f32) -> Self {
        Self {
            key,
            velocity,
            legato: false,
            glide_seconds: 0.0,
        }
    }
}

/// A destination's modulated values from the latest
/// [`process`](Engine::process) call.
#[derive(Clone, Copy, Debug)]
pub enum Output<'a> {
    /// No route reaches it: use its own value.
    Unmodulated,
    /// A value per frame.
    Audio(AudioOut<'a>),
    /// A value per control tick.
    Control(ControlOut<'a>),
}

/// An audio-rate destination's values: one per frame of the block (and
/// per voice), each clamped to its range.
#[derive(Clone, Copy, Debug)]
pub struct AudioOut<'a> {
    data: &'a [f32],
    width: usize,
}

impl AudioOut<'_> {
    /// The value at frame `frame` of the block for voice `voice` (any voice
    /// for a global destination).
    #[must_use]
    pub fn at(&self, frame: usize, voice: usize) -> f32 {
        let v = if self.width == 1 { 0 } else { voice };
        self.data[frame * self.width + v]
    }

    /// Every voice's value at frame `frame`, padded to the engine's stride
    /// (one value for a global destination).
    #[must_use]
    pub fn frame(&self, frame: usize) -> &[f32] {
        &self.data[frame * self.width..(frame + 1) * self.width]
    }
}

/// A control-rate destination's values for the latest call: per
/// [`Chunk`], the tick value in effect (its target) and the value its ramp
/// started from.
///
/// A consumer that updates per tick (a filter recomputing coefficients)
/// reads [`value`](Self::value); one that wants every sample reads
/// [`ramp`](Self::ramp), which moves linearly from the previous tick's
/// value to the latest over the period.
#[derive(Clone, Copy, Debug)]
pub struct ControlOut<'a> {
    chunks: &'a [Chunk],
    from: &'a [f32],
    to: &'a [f32],
    width: usize,
}

impl ControlOut<'_> {
    /// The call's chunks.
    #[must_use]
    pub fn chunks(&self) -> &[Chunk] {
        self.chunks
    }

    /// The tick value in effect in chunk `chunk` for voice `voice`.
    #[must_use]
    pub fn value(&self, chunk: usize, voice: usize) -> f32 {
        let v = if self.width == 1 { 0 } else { voice };
        self.to[chunk * self.width + v]
    }

    /// Fills `out` with voice `voice`'s smoothed value at each frame of the
    /// call, the call's first frame first.
    ///
    /// # Panics
    ///
    /// If `out` is shorter than the call.
    pub fn ramp(&self, voice: usize, out: &mut [f32]) {
        let v = if self.width == 1 { 0 } else { voice };
        let mut i = 0;
        for (c, chunk) in self.chunks.iter().enumerate() {
            let from = self.from[c * self.width + v];
            let to = self.to[c * self.width + v];
            let step = (to - from) / CONTROL_PERIOD as f32;
            for j in 0..chunk.len {
                out[i] = from + step * (chunk.phase + j) as f32;
                i += 1;
            }
        }
    }
}

/// A curve output gliding to a new note's value (engine-core R100).
#[derive(Clone, Copy, Debug, Default)]
struct Glide {
    target: f32,
    /// Per sample; 0 when not gliding.
    step: f32,
}

/// Everything the engine keeps but the program.
#[derive(Debug)]
struct State {
    config: Config,
    stride: usize,
    voice_src: Vec<bool>,
    /// Engine-made sources by what they hold.
    velocity: Vec<u16>,
    key: Vec<u16>,
    release: Vec<u16>,
    random: Vec<u16>,
    alternate: Vec<(u16, bool)>,
    load: Vec<u16>,
    held: Vec<f32>,
    held_global: Vec<f32>,
    base: Vec<f32>,
    amounts: [f32; SLOTS],
    outputs: [f32; SLOTS],
    active: Vec<bool>,
    fresh: Vec<f32>,
    any_fresh: bool,
    renote: Vec<bool>,
    any_renote: bool,
    order: Vec<u64>,
    counter: u64,
    newest: Option<usize>,
    /// The velocity and key curves' outputs per voice, and their glides.
    note_value: [Vec<f32>; 2],
    glide: [Vec<Glide>; 2],
    gliding: usize,
    alternate_on: bool,
    rng: u64,
    voice_limit: usize,
    clock: u64,
    chunks: Vec<Chunk>,
    groups: Vec<usize>,
}

/// The modulation engine.
///
/// Make it off the audio thread with the plug-in's registries; then on the
/// audio thread, each block:
///
/// 1. [`begin_block`](Self::begin_block) takes a newly compiled program
///    from the [mailbox](crate::mailbox()), if one is waiting;
/// 2. set what changed: held values ([`set_held`](Self::set_held)), bases
///    ([`set_base`](Self::set_base)), slot amounts
///    ([`set_amount`](Self::set_amount)), notes
///    ([`note_on`](Self::note_on));
/// 3. [`process`](Self::process) a range of frames with the block's
///    rendered signals, splitting the block at each event for sample
///    accuracy;
/// 4. read each destination's [`output`](Self::output).
///
/// Nothing but [`new`](Self::new) allocates, locks or blocks.
#[derive(Debug)]
pub struct Engine {
    st: State,
    program: Box<Program>,
    mailbox: Option<ProgramReceiver>,
}

impl std::fmt::Debug for ProgramReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProgramReceiver")
    }
}

impl Engine {
    /// An engine for `sources` and `destinations`, sized by `config`, with
    /// an empty program. Allocates; make it off the audio thread.
    #[must_use]
    pub fn new(sources: &Sources, destinations: &Destinations, config: Config) -> Self {
        let stride = config.stride();
        let n = sources.len();
        let mut st = State {
            config,
            stride,
            voice_src: vec![false; n],
            velocity: Vec::new(),
            key: Vec::new(),
            release: Vec::new(),
            random: Vec::new(),
            alternate: Vec::new(),
            load: Vec::new(),
            held: vec![0.0; n * stride],
            held_global: vec![0.0; n],
            base: vec![0.0; destinations.len()],
            amounts: [0.0; SLOTS],
            outputs: [1.0; SLOTS],
            active: vec![false; stride],
            fresh: vec![0.0; stride],
            any_fresh: false,
            renote: vec![false; stride],
            any_renote: false,
            order: vec![0; stride],
            counter: 0,
            newest: None,
            note_value: [vec![0.0; stride], vec![0.0; stride]],
            glide: [
                vec![Glide::default(); stride],
                vec![Glide::default(); stride],
            ],
            gliding: 0,
            alternate_on: false,
            rng: 0x9E37_79B9_7F4A_7C15,
            voice_limit: config.voices.max(1),
            clock: 0,
            chunks: Vec::with_capacity(config.max_chunks()),
            groups: Vec::with_capacity(stride),
        };
        for (id, info) in sources.iter() {
            let i = id.0;
            st.voice_src[usize::from(i)] = info.scope == Scope::Voice;
            match info.kind {
                SourceKind::Velocity => st.velocity.push(i),
                SourceKind::Key => st.key.push(i),
                SourceKind::ReleaseVelocity => st.release.push(i),
                SourceKind::Random => st.random.push(i),
                SourceKind::Alternate { inverse } => st.alternate.push((i, inverse)),
                SourceKind::VoiceLoad => st.load.push(i),
                _ => {}
            }
        }
        let _ = simd::level();
        Self {
            st,
            program: Box::new(Program::empty(config)),
            mailbox: None,
        }
    }

    /// Takes programs from `receiver` at each [`begin_block`](Self::begin_block).
    pub fn attach(&mut self, receiver: ProgramReceiver) {
        self.mailbox = Some(receiver);
    }

    /// Starts a block: takes the program waiting in the mailbox, if any, and
    /// hands the one it replaces back to the sender.
    pub fn begin_block(&mut self) {
        let Some(mailbox) = self.mailbox.as_mut() else {
            return;
        };
        if let Some(program) = mailbox.take() {
            let old = Self::install(&mut self.st, &mut self.program, program);
            if let Some(mailbox) = self.mailbox.as_mut() {
                mailbox.retire(old);
            }
        }
    }

    /// Replaces the program with `program` and returns the one no longer
    /// used: the old program, or `program` itself if it was compiled for a
    /// different [`Config`]. The new program's states start where its
    /// sources are now, without gliding there.
    pub fn swap(&mut self, program: Box<Program>) -> Box<Program> {
        Self::install(&mut self.st, &mut self.program, program)
    }

    fn install(st: &mut State, current: &mut Box<Program>, mut new: Box<Program>) -> Box<Program> {
        if new.config != st.config {
            return new;
        }
        st.amounts = new.amounts;
        st.outputs = new.outputs;
        for v in 0..st.stride {
            if st.active[v] {
                st.fresh[v] = 1.0;
                st.renote[v] = true;
                st.any_fresh = true;
                st.any_renote = true;
            }
        }
        new.primed = false;
        std::mem::replace(current, new)
    }

    /// Values between one voice's and the next's in per-voice buffers. A
    /// signal bound with at least this stride runs its plain routes a whole
    /// row of voices at a time.
    #[must_use]
    pub fn stride(&self) -> usize {
        self.st.stride
    }

    /// The program running.
    #[must_use]
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// Sets a held source's value: for a global source, its value; for a
    /// per-voice one, every voice's. Engine-made sources ignore it.
    pub fn set_held(&mut self, src: SourceId, value: f32) {
        let i = src.index();
        if i >= self.st.held_global.len() {
            return;
        }
        if self.st.voice_src[i] {
            let s = self.st.stride;
            self.st.held[i * s..(i + 1) * s].fill(value);
        } else {
            self.st.held_global[i] = value;
        }
    }

    /// Sets a per-voice held source's value for `voice`.
    pub fn set_voice_held(&mut self, src: SourceId, voice: usize, value: f32) {
        let i = src.index();
        if i < self.st.voice_src.len() && voice < self.st.stride {
            self.st.held[i * self.st.stride + voice] = value;
        }
    }

    /// Sets a destination's base: its value before modulation (a
    /// parameter's position on its travel, 0 to 1). Routes add to it and
    /// the sum is clamped to the destination's range (engine-core R108). A
    /// macro's, amount's or output's control takes its base from the
    /// macro's held value or the slot's live value instead.
    pub fn set_base(&mut self, dest: DestId, value: f32) {
        if let Some(b) = self.st.base.get_mut(dest.index()) {
            *b = value;
        }
    }

    /// Sets slot `slot`'s amount (−1 to 1), as automation does, without
    /// recompiling.
    pub fn set_amount(&mut self, slot: usize, amount: f32) {
        if let Some(a) = self.st.amounts.get_mut(slot) {
            *a = amount.clamp(-1.0, 1.0);
        }
    }

    /// Sets slot `slot`'s output scaler (0 to 1), as automation does.
    pub fn set_output(&mut self, slot: usize, output: f32) {
        if let Some(o) = self.st.outputs.get_mut(slot) {
            *o = output.clamp(0.0, 1.0);
        }
    }

    /// Sets the voice limit Voice load divides by.
    pub fn set_voice_limit(&mut self, voices: usize) {
        self.st.voice_limit = voices.max(1);
    }

    /// A note starts on `voice` before the next frame processed: the
    /// velocity and key curves take its values (engine-core R99 to R101),
    /// and the note-on sources take new ones and hold them (engine-core
    /// R110).
    pub fn note_on(&mut self, voice: usize, note: NoteOn) {
        let st = &mut self.st;
        let p = &self.program;
        if voice >= st.config.voices {
            return;
        }
        st.active[voice] = true;
        st.counter += 1;
        st.order[voice] = st.counter;
        st.newest = Some(voice);
        if !note.legato {
            st.fresh[voice] = 1.0;
            st.any_fresh = true;
        }
        st.renote[voice] = true;
        st.any_renote = true;
        let glide =
            (note.legato && p.glide && note.glide_seconds > 0.0).then_some(note.glide_seconds);
        let velocity = lookup(&p.luts[p.velocity_lut..], note.velocity);
        let key = lookup(&p.luts[p.key_lut..], note.key / 127.0);
        if !note.legato || p.velocity_legato {
            st.set_note_value(0, voice, velocity, glide);
        }
        st.set_note_value(1, voice, key, glide);
        for i in 0..st.random.len() {
            let r = st.next_random();
            st.held[usize::from(st.random[i]) * st.stride + voice] = r;
        }
        st.alternate_on = !st.alternate_on;
        for &(src, inverse) in &st.alternate {
            let on = st.alternate_on != inverse;
            st.held[usize::from(src) * st.stride + voice] = if on { 1.0 } else { 0.0 };
        }
        for &src in &st.release {
            st.held[usize::from(src) * st.stride + voice] = 0.0;
        }
    }

    /// The note on `voice` is released with `release_velocity` (0 to 1),
    /// which Release velocity takes and holds (engine-core R110).
    pub fn note_off(&mut self, voice: usize, release_velocity: f32) {
        let st = &mut self.st;
        if voice >= st.config.voices {
            return;
        }
        for &src in &st.release {
            st.held[usize::from(src) * st.stride + voice] = release_velocity;
        }
    }

    /// `voice` has stopped sounding: its routes stop costing anything.
    pub fn voice_ended(&mut self, voice: usize) {
        let st = &mut self.st;
        if voice >= st.config.voices {
            return;
        }
        st.active[voice] = false;
        st.fresh[voice] = 0.0;
        st.renote[voice] = false;
        for j in 0..2 {
            if st.glide[j][voice].step > 0.0 {
                st.glide[j][voice].step = 0.0;
                st.gliding -= 1;
            }
        }
        if st.newest == Some(voice) {
            st.newest = (0..st.config.voices)
                .filter(|v| st.active[*v])
                .max_by_key(|v| st.order[*v]);
        }
    }

    /// Whether `voice` is playing.
    #[must_use]
    pub fn is_active(&self, voice: usize) -> bool {
        self.st.active.get(voice).copied().unwrap_or(false)
    }

    /// Evaluates every route for frames `range` of the block, reading
    /// `signals`. Control-rate routes tick on a grid of
    /// [`CONTROL_PERIOD`] frames counted from the engine's start, so the
    /// results don't depend on how blocks are split.
    ///
    /// # Panics
    ///
    /// If `range` reaches past the configured block size.
    pub fn process(&mut self, signals: &Signals<'_>, range: Range<usize>) {
        self.process_at(simd::level(), signals, range);
    }

    /// [`process`](Self::process) with SIMD lanes at `level`, or the
    /// portable level if this CPU doesn't have it: for tests and benchmarks
    /// that compare paths, which give the same results to the bit.
    ///
    /// # Panics
    ///
    /// As [`process`](Self::process).
    pub fn process_at(&mut self, level: Level, signals: &Signals<'_>, range: Range<usize>) {
        assert!(
            range.end <= self.st.config.max_frames,
            "the block is longer than the engine was configured for"
        );
        if range.is_empty() {
            return;
        }
        let st = &mut self.st;
        let len = range.len();
        st.chunks.clear();
        let p = &mut *self.program;
        if p.routes.is_empty() {
            // An empty matrix: nothing to evaluate, only time to keep.
            if st.gliding > 0 {
                st.advance_glides(len);
            }
        } else {
            let mut k = range.start;
            let mut phase = (st.clock % CONTROL_PERIOD as u64) as usize;
            let mut first = true;
            while k < range.end {
                let n = (CONTROL_PERIOD - phase).min(range.end - k);
                st.chunks.push(Chunk {
                    start: k,
                    len: n,
                    phase,
                    tick: phase == 0 || (first && st.any_fresh),
                });
                k += n;
                phase = 0;
                first = false;
            }
            let count = st.active[..st.config.voices].iter().filter(|a| **a).count();
            let load = count as f32 / st.voice_limit as f32;
            for &src in &st.load {
                st.held_global[usize::from(src)] = load.min(1.0);
            }
            if st.any_renote {
                st.regenerate_route_randoms(p);
            }
            for ((gain, fast), route) in p.gains.iter_mut().zip(&mut p.fast).zip(&p.routes) {
                let s = usize::from(route.slot);
                *gain = st.amounts[s] * st.outputs[s];
                *fast = (route.affine.0 * *gain, route.affine.1 * *gain);
            }
            let run = Run {
                st: &mut *st,
                p,
                signals,
            };
            if let Err(run) = simd::dispatch_at(level, run) {
                let _ = simd::dispatch_at(Level::Portable, run);
            }
        }
        st.clock += len as u64;
        if st.any_fresh {
            st.fresh.fill(0.0);
            st.any_fresh = false;
        }
        if st.any_renote {
            st.renote.fill(false);
            st.any_renote = false;
        }
    }

    /// `dest`'s values from the latest [`process`](Self::process) call.
    #[must_use]
    pub fn output(&self, dest: DestId) -> Output<'_> {
        let p = &*self.program;
        let Some(Some(i)) = p.by_dest.get(dest.index()) else {
            return Output::Unmodulated;
        };
        let plan = &p.plans[usize::from(*i)];
        let w = plan.width(p.stride);
        if plan.audio {
            Output::Audio(AudioOut {
                data: &p.audio[plan.out..plan.out + w * p.config.max_frames],
                width: w,
            })
        } else {
            let n = self.st.chunks.len() * w;
            Output::Control(ControlOut {
                chunks: &self.st.chunks,
                from: &p.chunk_from[plan.out..plan.out + n],
                to: &p.chunk_to[plan.out..plan.out + n],
                width: w,
            })
        }
    }

    /// The latest tick value of control-rate `dest` for `voice`, or its
    /// value at the last frame processed for an audio-rate one; none if
    /// no route reaches it.
    #[must_use]
    pub fn latest(&self, dest: DestId, voice: usize) -> Option<f32> {
        match self.output(dest) {
            Output::Unmodulated => None,
            Output::Audio(a) => self
                .st
                .chunks
                .last()
                .map(|c| a.at(c.start + c.len - 1, voice)),
            Output::Control(c) => {
                (!c.chunks.is_empty()).then(|| c.value(c.chunks.len() - 1, voice))
            }
        }
    }
}

impl Engine {
    /// Fills `out`, `voices` values for each destination, with
    /// [`latest`](Self::latest): `out[dest.index() × voices + voice]`. A
    /// value is NaN where no route reaches the destination or the voice
    /// isn't playing.
    ///
    /// # Panics
    ///
    /// If `out` holds fewer than `voices` values for every destination of
    /// the engine's program.
    pub fn latest_all(&self, voices: usize, out: &mut [f32]) {
        for (d, row) in out
            .chunks_exact_mut(voices)
            .take(self.program.by_dest.len())
            .enumerate()
        {
            // Most destinations have no route: skip them without asking.
            if self.program.by_dest[d].is_none() {
                row.fill(f32::NAN);
                continue;
            }
            for (v, slot) in row.iter_mut().enumerate() {
                *slot = if self.is_active(v) {
                    self.latest(DestId(d as u16), v).unwrap_or(f32::NAN)
                } else {
                    f32::NAN
                };
            }
        }
    }
}

impl State {
    /// A random value from 0 to 1 (xorshift64*).
    fn next_random(&mut self) -> f32 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let x = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (x >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Sets curve output `j` (0 velocity, 1 key) of `voice` to `target`, at
    /// once or gliding over `glide` seconds.
    fn set_note_value(&mut self, j: usize, voice: usize, target: f32, glide: Option<f32>) {
        let g = &mut self.glide[j][voice];
        let was = g.step > 0.0;
        match glide {
            Some(seconds) if target != self.note_value[j][voice] => {
                g.target = target;
                g.step = (target - self.note_value[j][voice]).abs()
                    / (seconds * self.config.sample_rate);
                if !was {
                    self.gliding += 1;
                }
            }
            _ => {
                g.step = 0.0;
                if was {
                    self.gliding -= 1;
                }
                self.note_value[j][voice] = target;
                self.write_note_value(j, voice);
            }
        }
    }

    fn write_note_value(&mut self, j: usize, voice: usize) {
        let value = self.note_value[j][voice];
        let srcs = if j == 0 { &self.velocity } else { &self.key };
        for &src in srcs {
            self.held[usize::from(src) * self.stride + voice] = value;
        }
    }

    /// Moves every gliding curve output on by `frames`.
    fn advance_glides(&mut self, frames: usize) {
        for j in 0..2 {
            for voice in 0..self.config.voices {
                let g = self.glide[j][voice];
                if g.step <= 0.0 {
                    continue;
                }
                let value = &mut self.note_value[j][voice];
                let delta = g.step * frames as f32;
                if (g.target - *value).abs() <= delta {
                    *value = g.target;
                    self.glide[j][voice].step = 0.0;
                    self.gliding -= 1;
                } else if g.target > *value {
                    *value += delta;
                } else {
                    *value -= delta;
                }
                self.write_note_value(j, voice);
            }
        }
    }

    /// New per-route random values for the voices that just took a note.
    fn regenerate_route_randoms(&mut self, p: &mut Program) {
        for r in 0..p.routes.len() {
            let route = p.routes[r];
            for input in std::iter::once(route.source).chain(route.aux) {
                if let Read::RouteRandom { at } = input.read {
                    for v in 0..self.config.voices {
                        if self.renote[v] {
                            p.random[at as usize + v] = self.next_random();
                        }
                    }
                }
            }
        }
    }

    fn base(&self, plan: &Plan) -> f32 {
        match plan.base {
            Base::Set => self.base[plan.id.index()],
            Base::Held(src) => self.held_global[usize::from(src)],
            Base::Amount(slot) => self.amounts[usize::from(slot)],
            Base::Output(slot) => self.outputs[usize::from(slot)],
        }
    }
}

/// What a route reads, for one evaluation.
struct Env<'a> {
    signals: &'a Signals<'a>,
    held: &'a [f32],
    held_global: &'a [f32],
    cur: &'a [f32],
    random: &'a [f32],
    luts: &'a [f32],
    gains: &'a [f32],
    amounts: &'a [f32; SLOTS],
    outputs: &'a [f32; SLOTS],
    fresh: &'a [f32],
    stride: usize,
}

/// One `process` call's evaluation, compiled for each SIMD level.
struct Run<'a, 'b> {
    st: &'a mut State,
    p: &'a mut Program,
    signals: &'a Signals<'b>,
}

impl Kernel for Run<'_, '_> {
    type Output = ();

    #[inline(always)]
    #[allow(clippy::too_many_lines, reason = "one block's loop, kept in one place")]
    fn run<S: F32s>(self) {
        let Run { st, p, signals } = self;
        let stride = p.stride;
        st.groups.clear();
        for v0 in (0..stride).step_by(S::LANES) {
            if st.active[v0..v0 + S::LANES].iter().any(|a| *a) {
                st.groups.push(v0);
            }
        }
        let Program {
            routes,
            plans,
            control,
            luts,
            slew,
            random,
            gains,
            acc,
            from,
            cur,
            chunk_from,
            chunk_to,
            audio,
            primed,
            fast,
            ..
        } = p;
        let control = *control;
        // The voices playing, from the first lane group with one to the
        // last; plain routes run across it in whole rows.
        let span = match (st.groups.first(), st.groups.last()) {
            (Some(lo), Some(hi)) => *lo..*hi + S::LANES,
            _ => 0..0,
        };
        for ci in 0..st.chunks.len() {
            let chunk = st.chunks[ci];
            let snap = ci == 0 && st.any_fresh;
            if chunk.tick {
                let forced = chunk.phase != 0;
                let steps = if forced { 0.0 } else { CONTROL_PERIOD as f32 };
                for plan in &plans[..control] {
                    let w = plan.width(stride);
                    let at = plan.at;
                    acc[at..at + w].fill(st.base(plan));
                    {
                        let env = Env {
                            signals,
                            held: &st.held,
                            held_global: &st.held_global,
                            cur,
                            random,
                            luts,
                            gains,
                            amounts: &st.amounts,
                            outputs: &st.outputs,
                            fresh: &st.fresh,
                            stride,
                        };
                        let k = chunk.start;
                        let mask = if plan.voice && !span.is_empty() {
                            plains::<S>(
                                plan,
                                routes,
                                fast,
                                &env,
                                k..k + 1,
                                &span,
                                &mut acc[at..at + w],
                                st.base(plan),
                                None,
                            )
                        } else {
                            0
                        };
                        for (i, ri) in plan.routes.clone().enumerate() {
                            if mask & (1 << i) != 0 {
                                continue;
                            }
                            accumulate::<S>(
                                &routes[ri],
                                ri,
                                &env,
                                slew,
                                k,
                                steps,
                                snap,
                                plan.voice,
                                &st.groups,
                                st.newest,
                                &mut acc[at..at + w],
                            );
                        }
                    }
                    if !forced && !snap && *primed {
                        // Every lane moves on a tick: the latest becomes the
                        // ramp's start.
                        from[at..at + w].copy_from_slice(&cur[at..at + w]);
                        let (cur, acc) = (&mut cur[at..at + w], &acc[at..at + w]);
                        if w == 1 {
                            cur[0] = acc[0].max(plan.min).min(plan.max);
                        } else {
                            let (lo, hi) = (S::splat(plan.min), S::splat(plan.max));
                            for (c, a) in cur
                                .chunks_exact_mut(S::LANES)
                                .zip(acc.chunks_exact(S::LANES))
                            {
                                S::load(a).max(lo).min(hi).store(c);
                            }
                        }
                        continue;
                    }
                    for i in 0..w {
                        let v = acc[at + i].max(plan.min).min(plan.max);
                        let fresh = snap && plan.voice && st.fresh[i] != 0.0;
                        if forced && !fresh && *primed {
                            continue;
                        }
                        from[at + i] = if fresh || !*primed { v } else { cur[at + i] };
                        cur[at + i] = v;
                    }
                }
                *primed = true;
            }
            for plan in &plans[..control] {
                let w = plan.width(stride);
                let o = plan.out + ci * w;
                chunk_from[o..o + w].copy_from_slice(&from[plan.at..plan.at + w]);
                chunk_to[o..o + w].copy_from_slice(&cur[plan.at..plan.at + w]);
            }
            let env = Env {
                signals,
                held: &st.held,
                held_global: &st.held_global,
                cur,
                random,
                luts,
                gains,
                amounts: &st.amounts,
                outputs: &st.outputs,
                fresh: &st.fresh,
                stride,
            };
            for plan in &plans[control..] {
                let w = plan.width(stride);
                let frames = chunk.start..chunk.start + chunk.len;
                let region = &mut audio[plan.out + frames.start * w..plan.out + frames.end * w];
                let base = st.base(plan);
                let bounds = (plan.min > f32::NEG_INFINITY || plan.max < f32::INFINITY)
                    .then_some((plan.min, plan.max));
                // Plain routes, a row of voices at a time; the clamp folds
                // into their sweep when every route is plain.
                let voices = plan.voice && !span.is_empty();
                let all_plain = voices
                    && plan
                        .routes
                        .clone()
                        .all(|ri| plain_of(&routes[ri], &env, frames.end).is_some());
                let mask = if voices {
                    plains::<S>(
                        plan,
                        routes,
                        fast,
                        &env,
                        frames.clone(),
                        &span,
                        region,
                        base,
                        bounds.filter(|_| all_plain),
                    )
                } else {
                    0
                };
                if mask == 0 {
                    region.fill(base);
                }
                for (i, ri) in plan.routes.clone().enumerate() {
                    if mask & (1 << i) == 0 {
                        audio_route::<S>(
                            &routes[ri],
                            ri,
                            &env,
                            slew,
                            frames.clone(),
                            snap,
                            plan.voice,
                            &st.groups,
                            st.newest,
                            region,
                        );
                    }
                }
                if let Some((lo, hi)) = bounds
                    && !all_plain
                {
                    clamp::<S>(region, lo, hi);
                }
            }
            if st.gliding > 0 {
                st.advance_glides(chunk.len);
            }
        }
    }
}

/// Route `r`'s source as rows for frames up to `end`, if `r` is plain (see
/// [`Route::simple`]): its buffer, and how far one frame's row is from the
/// next (0 for a held row).
#[inline(always)]
fn plain_of<'a>(r: &Route, env: &Env<'a>, end: usize) -> Option<(&'a [f32], usize)> {
    if !r.simple {
        return None;
    }
    let w = env.stride;
    match r.source.read {
        Read::Signal { src, voice: true } => {
            let sig = env.signals.slots[usize::from(src)]?;
            (sig.stride >= w && end * sig.stride <= sig.data.len())
                .then_some((sig.data, sig.stride))
        }
        Read::Held { src, voice: true } => {
            let at = usize::from(src) * w;
            Some((&env.held[at..at + w], 0))
        }
        Read::Fed { at, voice: true } => Some((&env.cur[at as usize..at as usize + w], 0)),
        Read::RouteRandom { at } => Some((&env.random[at as usize..at as usize + w], 0)),
        _ => None,
    }
}

/// A plain route for one sweep: its rows and its gain-scaled `a`.
#[derive(Clone, Copy)]
struct Plain<'a> {
    data: &'a [f32],
    step: usize,
    a: f32,
}

/// Writes `plan`'s plain routes for `frames` into `region` (frame-major,
/// `stride` values a frame) across `span`: each value is `base` plus every
/// plain route's `b`, plus each route's `x × a` in order, four routes a
/// sweep in registers, clamped to `clamp` if given. Returns the routes it
/// took, as bits by their place in `plan`, or 0 if none is plain.
#[inline(always)]
#[allow(
    clippy::too_many_arguments,
    reason = "the evaluator's whole context, inlined"
)]
fn plains<S: F32s>(
    plan: &Plan,
    routes: &[Route],
    fast: &[(f32, f32)],
    env: &Env<'_>,
    frames: Range<usize>,
    span: &Range<usize>,
    region: &mut [f32],
    base: f32,
    clamp: Option<(f32, f32)>,
) -> u64 {
    let (mut mask, mut start, mut count) = (0u64, 0.0, 0);
    for (i, ri) in plan.routes.clone().enumerate() {
        if plain_of(&routes[ri], env, frames.end).is_some() {
            mask |= 1 << i;
            start += fast[ri].1;
            count += usize::from(fast[ri].0 != 0.0);
        }
    }
    if mask == 0 {
        return 0;
    }
    let start = base + start;
    let mut four = [Plain {
        data: &[],
        step: 0,
        a: 0.0,
    }; 4];
    let (mut m, mut seen, mut first) = (0, 0, true);
    for (i, ri) in plan.routes.clone().enumerate() {
        let a = fast[ri].0;
        if mask & (1 << i) == 0 || a == 0.0 {
            continue;
        }
        let (data, step) = plain_of(&routes[ri], env, frames.end).expect("plain above");
        four[m] = Plain { data, step, a };
        m += 1;
        seen += 1;
        if m == 4 || seen == count {
            let last = seen == count;
            sweep_frames::<S>(
                region,
                env.stride,
                frames.clone(),
                span,
                &four[..m],
                first.then_some(start),
                clamp.filter(|_| last),
            );
            first = false;
            m = 0;
        }
    }
    if count == 0 {
        sweep_frames::<S>(region, env.stride, frames, span, &[], Some(start), clamp);
    }
    mask
}

#[inline(always)]
fn sweep_frames<S: F32s>(
    region: &mut [f32],
    stride: usize,
    frames: Range<usize>,
    span: &Range<usize>,
    four: &[Plain<'_>],
    start: Option<f32>,
    clamp: Option<(f32, f32)>,
) {
    for (j, k) in frames.enumerate() {
        let row = &mut region[j * stride + span.start..j * stride + span.end];
        match four.len() {
            0 => sweep::<S, 0>(row, four, k, span.start, start, clamp),
            1 => sweep::<S, 1>(row, four, k, span.start, start, clamp),
            2 => sweep::<S, 2>(row, four, k, span.start, start, clamp),
            3 => sweep::<S, 3>(row, four, k, span.start, start, clamp),
            _ => sweep::<S, 4>(row, four, k, span.start, start, clamp),
        }
    }
}

/// `out = start + Σ x × a` (or `out += Σ x × a` without a start) across
/// `out`, a whole number of lane groups, reading each route's row for
/// frame `k` from voice `from`.
#[inline(always)]
fn sweep<S: F32s, const N: usize>(
    out: &mut [f32],
    four: &[Plain<'_>],
    k: usize,
    from: usize,
    start: Option<f32>,
    clamp: Option<(f32, f32)>,
) {
    let len = out.len() / S::LANES * S::LANES;
    let out = &mut out[..len];
    // Each row is `len` long: the checked slicing here is the only bounds
    // check, once a sweep rather than once a load.
    let mut rows: [&[f32]; N] = [&[]; N];
    let mut a = [S::splat(0.0); N];
    for i in 0..N {
        let at = k * four[i].step + from;
        rows[i] = &four[i].data[at..at + len];
        a[i] = S::splat(four[i].a);
    }
    let begin = S::splat(start.unwrap_or(0.0));
    let (lo, hi) = clamp.map_or((S::splat(0.0), S::splat(0.0)), |(lo, hi)| {
        (S::splat(lo), S::splat(hi))
    });
    let mut at = 0;
    while at < len {
        let end = at + S::LANES;
        // SAFETY: `at` steps by whole lane groups up to `len`, which is a
        // whole number of them, so `at..end` lies within `out` and within
        // every row, each exactly `len` long.
        let o = unsafe { out.get_unchecked_mut(at..end) };
        let mut acc = if start.is_some() { begin } else { S::load(o) };
        for (row, a) in rows.iter().zip(&a) {
            // SAFETY: as above.
            let x = unsafe { row.get_unchecked(at..end) };
            acc += S::load(x) * *a;
        }
        if clamp.is_some() {
            acc = acc.max(lo).min(hi);
        }
        acc.store(o);
        at = end;
    }
}

/// Lane `i` of `x`.
#[inline(always)]
fn lane<S: F32s>(x: S, i: usize) -> f32 {
    let mut lanes = [0.0; crate::MAX_LANES];
    x.store(&mut lanes);
    lanes[i]
}

/// Clamps every value of `values` to `min` to `max`.
#[inline(always)]
fn clamp<S: F32s>(values: &mut [f32], min: f32, max: f32) {
    let (lo, hi) = (S::splat(min), S::splat(max));
    let mut chunks = values.chunks_exact_mut(S::LANES);
    for c in &mut chunks {
        S::load(c).max(lo).min(hi).store(c);
    }
    for v in chunks.into_remainder() {
        *v = v.max(min).min(max);
    }
}

/// Adds audio-rate route `r`'s contribution for `frames` to `region`, its
/// destination's values for those frames, frame-major with `out.len() /
/// frames.len()` values a frame.
#[inline(always)]
#[allow(
    clippy::too_many_arguments,
    reason = "the evaluator's whole context, inlined"
)]
fn audio_route<S: F32s>(
    r: &Route,
    ri: usize,
    env: &Env<'_>,
    slew: &mut [f32],
    frames: Range<usize>,
    snap: bool,
    dest_voice: bool,
    groups: &[usize],
    newest: Option<usize>,
    region: &mut [f32],
) {
    if matches!((r.amount, r.output), (Gain::Live, Gain::Live)) && env.gains[ri] == 0.0 {
        return;
    }
    let w = region.len() / frames.len();
    let add = |region: &mut [f32], j: usize, v0: usize, x: S| {
        let o = &mut region[j * w + v0..];
        (S::load(o) + x).store(o);
    };
    match (r.voice, dest_voice) {
        (true, true) => {
            let simple = match (r.simple, r.source.read) {
                (true, Read::Signal { src, .. }) => env.signals.slots[usize::from(src)],
                _ => None,
            };
            for &v0 in groups {
                if let Some(sig) = simple.filter(|sig| {
                    v0 + S::LANES <= sig.stride && frames.end * sig.stride <= sig.data.len()
                }) {
                    // x × a + b, times the gain: what `route_value` computes.
                    let (a, b) = (S::splat(r.affine.0), S::splat(r.affine.1));
                    let g = S::splat(env.gains[ri]);
                    for (j, k) in frames.clone().enumerate() {
                        let x = S::load(&sig.data[k * sig.stride + v0..]);
                        add(region, j, v0, (x * a + b) * g);
                    }
                } else if r.steady {
                    let x = route_value::<S>(r, ri, env, slew, frames.start, v0, 1.0, false);
                    for j in 0..frames.len() {
                        add(region, j, v0, x);
                    }
                } else {
                    for (j, k) in frames.clone().enumerate() {
                        let x = route_value::<S>(r, ri, env, slew, k, v0, 1.0, snap && j == 0);
                        add(region, j, v0, x);
                    }
                }
            }
        }
        (false, true) => {
            let mut x = S::splat(0.0);
            for (j, k) in frames.clone().enumerate() {
                if j == 0 || !r.steady {
                    x = S::splat(lane(
                        route_value::<S>(r, ri, env, slew, k, 0, 1.0, false),
                        0,
                    ));
                }
                for &v0 in groups {
                    add(region, j, v0, x);
                }
            }
        }
        (true, false) => {
            if let Some(n) = newest {
                let v0 = n / S::LANES * S::LANES;
                for (j, k) in frames.enumerate() {
                    let x = route_value::<S>(r, ri, env, slew, k, v0, 1.0, snap && j == 0);
                    region[j * w] += lane(x, n - v0);
                }
            }
        }
        (false, false) => {
            for (j, k) in frames.enumerate() {
                region[j * w] += lane(route_value::<S>(r, ri, env, slew, k, 0, 1.0, false), 0);
            }
        }
    }
}

/// Adds route `r`'s contribution at frame `k` to `out`, its destination's
/// values (one per voice, or one).
#[inline(always)]
#[allow(
    clippy::too_many_arguments,
    reason = "the evaluator's whole context, inlined"
)]
fn accumulate<S: F32s>(
    r: &Route,
    ri: usize,
    env: &Env<'_>,
    slew: &mut [f32],
    k: usize,
    steps: f32,
    snap: bool,
    dest_voice: bool,
    groups: &[usize],
    newest: Option<usize>,
    out: &mut [f32],
) {
    if matches!((r.amount, r.output), (Gain::Live, Gain::Live)) && env.gains[ri] == 0.0 {
        return;
    }
    match (r.voice, dest_voice) {
        (true, true) => {
            for &v0 in groups {
                let x = route_value::<S>(r, ri, env, slew, k, v0, steps, snap);
                (S::load(&out[v0..]) + x).store(&mut out[v0..]);
            }
        }
        (false, true) => {
            let x = S::splat(lane(
                route_value::<S>(r, ri, env, slew, k, 0, steps, false),
                0,
            ));
            for &v0 in groups {
                (S::load(&out[v0..]) + x).store(&mut out[v0..]);
            }
        }
        // A per-voice route into a global destination: the newest voice's
        // value (engine-core R113, a first rule until observed), from the
        // lane group that holds it.
        (true, false) => {
            if let Some(n) = newest {
                let v0 = n / S::LANES * S::LANES;
                let x = route_value::<S>(r, ri, env, slew, k, v0, steps, snap);
                out[0] += lane(x, n - v0);
            }
        }
        (false, false) => {
            out[0] += lane(route_value::<S>(r, ri, env, slew, k, 0, steps, false), 0);
        }
    }
}

/// Route `r`'s value for the lanes from voice `v0` at frame `k`:
/// curve(source) × amount × output × curve(aux) (engine-core R103), the
/// source one way or both ways (R105).
#[inline(always)]
#[allow(
    clippy::too_many_arguments,
    reason = "the evaluator's whole context, inlined"
)]
fn route_value<S: F32s>(
    r: &Route,
    ri: usize,
    env: &Env<'_>,
    slew: &mut [f32],
    k: usize,
    v0: usize,
    steps: f32,
    snap: bool,
) -> S {
    let snap = if snap {
        S::load(&env.fresh[v0..])
    } else {
        S::splat(0.0)
    };
    let mut p = if r.source.curve.is_none() && r.source.slew.is_none() {
        read::<S>(r.source.read, env, k, v0) * S::splat(r.affine.0) + S::splat(r.affine.1)
    } else {
        let u = input::<S>(&r.source, env, slew, k, v0, steps, snap);
        if r.bipolar {
            u * S::splat(2.0) - S::splat(1.0)
        } else {
            u
        }
    };
    if let Some(aux) = &r.aux {
        let mut a = input::<S>(aux, env, slew, k, v0, steps, snap);
        if r.aux_invert {
            a = S::splat(1.0) - a;
        }
        p *= a;
    }
    let g = match (r.amount, r.output) {
        (Gain::Live, Gain::Live) => S::splat(env.gains[ri]),
        (amount, output) => {
            let slot = usize::from(r.slot);
            gain::<S>(amount, env.amounts[slot], env, v0)
                * gain::<S>(output, env.outputs[slot], env, v0)
        }
    };
    p * g
}

#[inline(always)]
fn gain<S: F32s>(g: Gain, live: f32, env: &Env<'_>, v0: usize) -> S {
    match g {
        Gain::Live => S::splat(live),
        Gain::Fed { at, voice: true } => S::load(&env.cur[at as usize + v0..]),
        Gain::Fed { at, voice: false } => S::splat(env.cur[at as usize]),
    }
}

/// An input's unipolar value, curved and slewed.
#[inline(always)]
fn input<S: F32s>(
    inp: &Input,
    env: &Env<'_>,
    slew: &mut [f32],
    k: usize,
    v0: usize,
    steps: f32,
    snap: S,
) -> S {
    let mut u = read::<S>(inp.read, env, k, v0);
    if inp.bipolar {
        u = u * S::splat(0.5) + S::splat(0.5);
    }
    if let Some(at) = inp.curve {
        let table = &env.luts[at as usize..];
        let mut lanes = [0.0; crate::MAX_LANES];
        u.store(&mut lanes);
        for x in &mut lanes[..S::LANES] {
            *x = lookup(table, *x);
        }
        u = S::load(&lanes);
    }
    if let Some(s) = inp.slew {
        let state = &mut slew[s.at as usize + v0..];
        let prev = S::load(state);
        let (up, down) = if steps == 0.0 {
            (0.0, 0.0)
        } else {
            (s.up * steps, s.down * steps)
        };
        let moved = prev + (u - prev).max(S::splat(-down)).min(S::splat(up));
        let y = moved + (u - moved) * snap;
        y.store(state);
        u = y;
    }
    u
}

#[inline(always)]
fn read<S: F32s>(read: Read, env: &Env<'_>, k: usize, v0: usize) -> S {
    match read {
        Read::Signal { src, voice } => match env.signals.slots[usize::from(src)] {
            None => S::splat(0.0),
            Some(sig) if voice => {
                let base = k * sig.stride + v0;
                let room = sig.stride.saturating_sub(v0).min(S::LANES);
                if room == S::LANES && base + S::LANES <= sig.data.len() {
                    S::load(&sig.data[base..])
                } else {
                    let mut lanes = [0.0; crate::MAX_LANES];
                    for (i, lane) in lanes[..room].iter_mut().enumerate() {
                        *lane = sig.data.get(base + i).copied().unwrap_or(0.0);
                    }
                    S::load(&lanes)
                }
            }
            Some(sig) => S::splat(sig.data.get(k * sig.stride).copied().unwrap_or(0.0)),
        },
        Read::Held { src, voice: true } => S::load(&env.held[usize::from(src) * env.stride + v0..]),
        Read::Held { src, voice: false } => S::splat(env.held_global[usize::from(src)]),
        Read::Fed { at, voice: true } => S::load(&env.cur[at as usize + v0..]),
        Read::Fed { at, voice: false } => S::splat(env.cur[at as usize]),
        Read::Constant => S::splat(1.0),
        Read::RouteRandom { at } => S::load(&env.random[at as usize + v0..]),
    }
}
