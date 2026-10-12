//! The compiled matrix: a flat list of the routes in use, in the order they
//! run, with every buffer they need, made off the audio thread.

use crate::curve::{Curve, LUT_SIZE};
use crate::dest::{DestId, Destinations, Feed, Rate};
use crate::matrix::{Matrix, Polarity, SLOTS};
use crate::source::{Scope, SourceId, SourceKind, SourceRange, Sources};
use crate::{CONTROL_PERIOD, MAX_LANES};

/// What an engine and its programs are sized for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    /// The sample rate, in hertz.
    pub sample_rate: f32,
    /// The most frames a block holds.
    pub max_frames: usize,
    /// How many voices.
    pub voices: usize,
}

impl Config {
    /// Values between one voice's and the next's in per-voice buffers:
    /// the voice count rounded up to the widest SIMD vector.
    #[must_use]
    pub fn stride(&self) -> usize {
        self.voices.div_ceil(MAX_LANES).max(1) * MAX_LANES
    }

    /// The most chunks a block splits into: a period each, and a part
    /// period at either end.
    #[must_use]
    pub fn max_chunks(&self) -> usize {
        self.max_frames / CONTROL_PERIOD + 2
    }
}

/// Where an input's value comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Read {
    /// A bound signal; `voice` if it has a value per voice.
    Signal { src: u16, voice: bool },
    /// A held value (the plug-in's or the engine's).
    Held { src: u16, voice: bool },
    /// A feeding destination's latest tick, at `at` in the tick values.
    Fed { at: u32, voice: bool },
    /// 1.
    Constant,
    /// This route's own random value per voice, at `at`.
    RouteRandom { at: u32 },
}

impl Read {
    pub(crate) fn is_voice(self) -> bool {
        match self {
            Self::Signal { voice, .. } | Self::Held { voice, .. } | Self::Fed { voice, .. } => {
                voice
            }
            Self::Constant => false,
            Self::RouteRandom { .. } => true,
        }
    }
}

/// A curve's slew: its state at `at`, and the most it moves per sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Slew {
    pub at: u32,
    pub up: f32,
    pub down: f32,
}

/// One input of a route: where it reads, its range, curve and slew.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Input {
    pub read: Read,
    pub bipolar: bool,
    /// Its curve's table, at this offset in the program's tables.
    pub curve: Option<u32>,
    pub slew: Option<Slew>,
}

/// Where a route's amount or output comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Gain {
    /// The slot's live value.
    Live,
    /// A feeding destination's tick values.
    Fed { at: u32, voice: bool },
}

/// One route, ready to run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Route {
    pub slot: u8,
    pub source: Input,
    pub aux: Option<Input>,
    pub aux_invert: bool,
    pub bipolar: bool,
    pub amount: Gain,
    pub output: Gain,
    /// Its destination's index in the program's plans.
    pub dest: u16,
    /// Whether it has a value per voice.
    pub voice: bool,
    /// `(a, b)` with the source, uncurved and unslewed, giving `x × a + b`
    /// after its polarity.
    pub affine: (f32, f32),
    /// Whether its value can't change within a control period: it reads
    /// no signal and slews nothing.
    pub steady: bool,
    /// Whether it's plain: a per-voice source into a per-voice
    /// destination, uncurved, unslewed, without aux, at the slot's live
    /// gains. Plain routes run across a row of voices at once.
    pub simple: bool,
}

/// Where a destination's base value comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Base {
    /// What the plug-in set.
    Set,
    /// A held global source's value (the macro it controls).
    Held(u16),
    /// The slot's live amount.
    Amount(u8),
    /// The slot's live output.
    Output(u8),
}

/// One destination in use.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Plan {
    pub id: DestId,
    pub voice: bool,
    pub audio: bool,
    pub min: f32,
    pub max: f32,
    pub base: Base,
    /// Its routes, in `routes`.
    pub routes: std::ops::Range<usize>,
    /// Its accumulator, latest tick and the tick before, at this offset.
    pub at: usize,
    /// Its per-chunk values (control) or per-frame values (audio), at this
    /// offset in `chunk_from` and `chunk_to`, or `audio`.
    pub out: usize,
}

impl Plan {
    pub(crate) fn width(&self, stride: usize) -> usize {
        if self.voice { stride } else { 1 }
    }
}

/// A slot the compiler left out, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Skipped {
    /// The slot.
    pub slot: usize,
    /// Why.
    pub reason: SkipReason,
}

/// Why a slot was left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// Its source isn't in the registry.
    UnknownSource,
    /// Its aux source isn't in the registry.
    UnknownAux,
    /// Its destination isn't in the registry.
    UnknownDestination,
}

/// A compiled matrix: the routes in use as a flat list in the order they
/// run, the destinations they reach and the state they keep.
///
/// Compile with [`Program::compile`] off the audio thread, then hand the
/// program to the [`Engine`](crate::Engine), directly with
/// [`Engine::swap`](crate::Engine::swap) or through a
/// [`mailbox`](crate::mailbox()), and it takes over at the next block.
#[derive(Clone, Debug)]
pub struct Program {
    pub(crate) config: Config,
    pub(crate) stride: usize,
    pub(crate) routes: Vec<Route>,
    pub(crate) plans: Vec<Plan>,
    /// How many of `plans` are control rate; they come first.
    pub(crate) control: usize,
    /// For each registry destination, its plan.
    pub(crate) by_dest: Vec<Option<u16>>,
    pub(crate) luts: Vec<f32>,
    pub(crate) velocity_lut: usize,
    pub(crate) key_lut: usize,
    pub(crate) glide: bool,
    pub(crate) velocity_legato: bool,
    pub(crate) amounts: [f32; SLOTS],
    pub(crate) outputs: [f32; SLOTS],
    // State and scratch, sized here so the audio thread never allocates.
    pub(crate) slew: Vec<f32>,
    pub(crate) random: Vec<f32>,
    pub(crate) gains: Vec<f32>,
    /// Each plain route's `(a, b)` times its gain, for the row passes.
    pub(crate) fast: Vec<(f32, f32)>,
    pub(crate) acc: Vec<f32>,
    pub(crate) from: Vec<f32>,
    pub(crate) cur: Vec<f32>,
    pub(crate) chunk_from: Vec<f32>,
    pub(crate) chunk_to: Vec<f32>,
    pub(crate) audio: Vec<f32>,
    /// Whether its control destinations have ticked once.
    pub(crate) primed: bool,
    skipped: Vec<Skipped>,
}

impl Program {
    /// A program with no routes, and the diagonal velocity and key curves.
    #[must_use]
    pub fn empty(config: Config) -> Self {
        Self::compile(
            &Matrix::new(),
            &Sources::new(),
            &Destinations::new(),
            config,
        )
    }

    /// Compiles `matrix` against the plug-in's `sources` and
    /// `destinations`: only slots with a source and a destination, not
    /// bypassed, become routes. Allocates; run it off the audio thread.
    #[must_use]
    pub fn compile(
        matrix: &Matrix,
        sources: &Sources,
        destinations: &Destinations,
        config: Config,
    ) -> Self {
        Compiler::new(sources, destinations, config).run(matrix)
    }

    /// How many routes it runs.
    #[must_use]
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }

    /// The slots it left out, and why.
    #[must_use]
    pub fn skipped(&self) -> &[Skipped] {
        &self.skipped
    }

    /// The rate `dest` runs at in this program, [`Rate::Audio`] or
    /// [`Rate::Control`], or none if no route reaches it.
    #[must_use]
    pub fn rate(&self, dest: DestId) -> Option<Rate> {
        let plan = &self.plans[usize::from((*self.by_dest.get(dest.index())?)?)];
        Some(if plan.audio {
            Rate::Audio
        } else {
            Rate::Control
        })
    }

    /// What it was compiled for.
    #[must_use]
    pub fn config(&self) -> Config {
        self.config
    }
}

/// A slot that will become a route.
struct Candidate {
    slot: usize,
    source: SourceId,
    aux: Option<SourceId>,
    dest: DestId,
}

struct Compiler<'a> {
    sources: &'a Sources,
    dests: &'a Destinations,
    config: Config,
    stride: usize,
}

impl<'a> Compiler<'a> {
    fn new(sources: &'a Sources, dests: &'a Destinations, config: Config) -> Self {
        Self {
            sources,
            dests,
            config,
            stride: config.stride(),
        }
    }

    fn run(&self, matrix: &Matrix) -> Program {
        let (candidates, skipped) = self.candidates(matrix);
        let n_dests = self.dests.len();

        // The destinations in use, and which of them feed what.
        let mut used = vec![false; n_dests];
        for c in &candidates {
            used[c.dest.index()] = true;
        }
        let feed_of = |d: usize| -> Option<Feed> {
            let info = self.dests.get(DestId(d as u16))?;
            match info.feed? {
                Feed::Source(src) => {
                    let s = self.sources.get(src)?;
                    (s.kind == SourceKind::Held && s.scope == Scope::Global)
                        .then_some(Feed::Source(src))
                }
                feed => Some(feed),
            }
        };
        let mut source_fed = vec![None; self.sources.len()];
        let mut amount_fed = [None; SLOTS];
        let mut output_fed = [None; SLOTS];
        for d in (0..n_dests).filter(|d| used[*d]) {
            match feed_of(d) {
                Some(Feed::Source(src)) => source_fed[src.index()] = Some(d),
                Some(Feed::Amount(slot)) => amount_fed[usize::from(slot)] = Some(d),
                Some(Feed::Output(slot)) => output_fed[usize::from(slot)] = Some(d),
                None => {}
            }
        }
        let audio_source = |src: SourceId| {
            self.sources
                .get(src)
                .is_some_and(|s| s.kind == SourceKind::Audio)
        };
        let is_audio = |d: usize| -> bool {
            let info = &self.dests.get(DestId(d as u16)).expect("in the registry");
            if feed_of(d).is_some() {
                return false;
            }
            match info.rate {
                Rate::Audio => true,
                Rate::Control => false,
                Rate::Adaptive => candidates.iter().any(|c| {
                    c.dest.index() == d
                        && (audio_source(c.source) || c.aux.is_some_and(audio_source))
                }),
            }
        };

        // Control destinations in an order where each runs after those it
        // reads, so a tick's routes see this tick's macros and amounts; a
        // cycle reads the previous tick. Then the audio destinations.
        let feeds_into = |a: usize, b: usize| -> bool {
            candidates.iter().filter(|c| c.dest.index() == b).any(|c| {
                source_fed[c.source.index()] == Some(a)
                    || c.aux.is_some_and(|x| source_fed[x.index()] == Some(a))
                    || amount_fed[c.slot] == Some(a)
                    || output_fed[c.slot] == Some(a)
            })
        };
        let control: Vec<usize> = (0..n_dests).filter(|d| used[*d] && !is_audio(*d)).collect();
        let mut order = Vec::with_capacity(control.len());
        let mut left = control.clone();
        while !left.is_empty() {
            let ready = left
                .iter()
                .position(|b| !left.iter().any(|a| a != b && feeds_into(*a, *b)))
                .unwrap_or(0);
            order.push(left.remove(ready));
        }
        let n_control = order.len();
        order.extend((0..n_dests).filter(|d| used[*d] && is_audio(*d)));

        // Plans, with their offsets.
        let stride = self.stride;
        let chunks = self.config.max_chunks();
        let mut by_dest = vec![None; n_dests];
        let mut plans = Vec::with_capacity(order.len());
        let (mut at, mut ctl_out, mut audio_out) = (0, 0, 0);
        for (i, &d) in order.iter().enumerate() {
            let info = self.dests.get(DestId(d as u16)).expect("in the registry");
            let voice = info.scope == Scope::Voice;
            let audio = i >= n_control;
            let width = if voice { stride } else { 1 };
            let base = match feed_of(d) {
                Some(Feed::Source(src)) => Base::Held(src.0),
                Some(Feed::Amount(slot)) => Base::Amount(slot),
                Some(Feed::Output(slot)) => Base::Output(slot),
                None => Base::Set,
            };
            let out = if audio { audio_out } else { ctl_out };
            plans.push(Plan {
                id: DestId(d as u16),
                voice,
                audio,
                min: info.min,
                max: info.max,
                base,
                routes: 0..0,
                at,
                out,
            });
            by_dest[d] = Some(i as u16);
            at += width;
            if audio {
                audio_out += width * self.config.max_frames;
            } else {
                ctl_out += width * chunks;
            }
        }

        // Routes, destination by destination, each in slot order.
        let mut luts = Vec::new();
        let velocity_lut = tabulate_shape(&mut luts, &matrix.note_curves.velocity);
        let key_lut = tabulate_shape(&mut luts, &matrix.note_curves.key);
        let mut slew_len = 0u32;
        let mut random_len = 0u32;
        let mut routes = Vec::with_capacity(candidates.len());
        let placed: Vec<Option<(u32, bool)>> = by_dest
            .iter()
            .map(|p| p.map(|p: u16| (plans[usize::from(p)].at as u32, plans[usize::from(p)].voice)))
            .collect();
        let fed_read = |d: Option<usize>| -> Option<(u32, bool)> { placed[d?] };
        for (pi, &d) in order.iter().enumerate() {
            let start = routes.len();
            for c in candidates.iter().filter(|c| c.dest.index() == d) {
                let slot = &matrix.slots()[c.slot];
                let read = |src: SourceId, random_len: &mut u32| -> (Read, bool) {
                    let info = self.sources.get(src).expect("checked");
                    let voice = info.scope == Scope::Voice;
                    let read = if let Some((at, voice)) = fed_read(source_fed[src.index()]) {
                        Read::Fed { at, voice }
                    } else {
                        match info.kind {
                            SourceKind::Signal | SourceKind::Audio => {
                                Read::Signal { src: src.0, voice }
                            }
                            SourceKind::Constant => Read::Constant,
                            SourceKind::RandomPerRoute => {
                                let at = *random_len;
                                *random_len += stride as u32;
                                Read::RouteRandom { at }
                            }
                            _ => Read::Held { src: src.0, voice },
                        }
                    };
                    (read, info.range == SourceRange::Bipolar)
                };
                let (source_read, source_bipolar) = read(c.source, &mut random_len);
                let aux_read = c.aux.map(|a| read(a, &mut random_len));
                let gain = |fed: Option<usize>| match fed_read(fed) {
                    Some((at, voice)) => Gain::Fed { at, voice },
                    None => Gain::Live,
                };
                let amount = gain(amount_fed[c.slot]);
                let output = gain(output_fed[c.slot]);
                let voice = source_read.is_voice()
                    || aux_read.is_some_and(|(r, _)| r.is_voice())
                    || matches!(amount, Gain::Fed { voice: true, .. })
                    || matches!(output, Gain::Fed { voice: true, .. });
                // A slew keeps a state per voice lane even for a global
                // route, which evaluates a whole lane group and takes one.
                let width = stride as u32;
                let mut input = |(read, bipolar): (Read, bool), curve: &Curve| -> Input {
                    let lut = (!curve.is_linear()).then(|| {
                        let at = luts.len() as u32;
                        curve.tabulate(&mut luts);
                        at
                    });
                    let (rise, fall) = curve.slew();
                    let slew = (rise > 0.0 || fall > 0.0).then(|| {
                        let s = Slew {
                            at: slew_len,
                            up: per_sample(rise, self.config.sample_rate),
                            down: per_sample(fall, self.config.sample_rate),
                        };
                        slew_len += width;
                        s
                    });
                    Input {
                        read,
                        bipolar,
                        curve: lut,
                        slew,
                    }
                };
                let source = input((source_read, source_bipolar), &slot.source_curve);
                let aux = aux_read.map(|r| input(r, &slot.aux_curve));
                let bipolar = slot.polarity == Polarity::Bipolar;
                let affine = match (source.bipolar, bipolar) {
                    (false, false) | (true, true) => (1.0, 0.0),
                    (false, true) => (2.0, -1.0),
                    (true, false) => (0.5, 0.5),
                };
                let is_signal = |i: &Input| matches!(i.read, Read::Signal { .. });
                let steady = std::iter::once(&source)
                    .chain(aux.as_ref())
                    .all(|i| !is_signal(i) && i.slew.is_none());
                let dest_voice = plans[pi].voice;
                let simple = source.read.is_voice()
                    && dest_voice
                    && source.curve.is_none()
                    && source.slew.is_none()
                    && aux.is_none()
                    && amount == Gain::Live
                    && output == Gain::Live;
                routes.push(Route {
                    slot: c.slot as u8,
                    source,
                    aux,
                    aux_invert: slot.aux_invert,
                    bipolar,
                    affine,
                    steady,
                    simple,
                    amount,
                    output,
                    dest: pi as u16,
                    voice,
                });
            }
            plans[pi].routes = start..routes.len();
        }

        let mut amounts = [0.0; SLOTS];
        let mut outputs = [1.0; SLOTS];
        for (n, slot) in matrix.slots().iter().enumerate() {
            amounts[n] = slot.amount;
            outputs[n] = slot.output;
        }
        let width_sum = at;
        Program {
            config: self.config,
            stride,
            gains: vec![0.0; routes.len()],
            fast: vec![(0.0, 0.0); routes.len()],
            routes,
            control: n_control,
            by_dest,
            luts,
            velocity_lut,
            key_lut,
            glide: matrix.note_curves.glide,
            velocity_legato: matrix.note_curves.velocity_legato,
            amounts,
            outputs,
            slew: vec![0.0; slew_len as usize],
            random: vec![0.0; random_len as usize],
            acc: vec![0.0; width_sum],
            from: vec![0.0; width_sum],
            cur: vec![0.0; width_sum],
            chunk_from: vec![0.0; ctl_out],
            chunk_to: vec![0.0; ctl_out],
            audio: vec![0.0; audio_out],
            primed: false,
            plans,
            skipped,
        }
    }

    /// The slots with a source and a destination, not bypassed, that the
    /// registries know; and those they don't.
    fn candidates(&self, matrix: &Matrix) -> (Vec<Candidate>, Vec<Skipped>) {
        let mut candidates = Vec::new();
        let mut skipped = Vec::new();
        for (n, slot) in matrix.slots().iter().enumerate() {
            let (Some(source), Some(target)) = (slot.source, slot.destination) else {
                continue;
            };
            if slot.bypass {
                continue;
            }
            let skip = |reason| Skipped { slot: n, reason };
            if self.sources.get(source).is_none() {
                skipped.push(skip(SkipReason::UnknownSource));
                continue;
            }
            if slot.aux.is_some_and(|a| self.sources.get(a).is_none()) {
                skipped.push(skip(SkipReason::UnknownAux));
                continue;
            }
            let Some(dest) = self.dests.find(target) else {
                skipped.push(skip(SkipReason::UnknownDestination));
                continue;
            };
            candidates.push(Candidate {
                slot: n,
                source,
                aux: slot.aux,
                dest,
            });
        }
        (candidates, skipped)
    }
}

/// The most a slewed value moves per sample to cross full scale in
/// `seconds`; unlimited for 0.
fn per_sample(seconds: f32, sample_rate: f32) -> f32 {
    if seconds > 0.0 {
        1.0 / (seconds * sample_rate)
    } else {
        f32::INFINITY
    }
}

/// Appends `shape`'s table to `luts` and returns its offset.
fn tabulate_shape(luts: &mut Vec<f32>, shape: &lspk_dsp::lfo::Shape) -> usize {
    let at = luts.len();
    luts.extend((0..=LUT_SIZE).map(|i| shape.value(i as f32 / LUT_SIZE as f32)));
    at
}
