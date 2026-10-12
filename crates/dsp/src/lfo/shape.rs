//! The drawn shape and the XY path an LFO plays: fixed-capacity point lists,
//! so editing and playing them never allocates.

use super::Lane;

/// The most points a shape or a path holds.
pub const MAX_POINTS: usize = 64;

/// How far a segment's curve bends: at ±1 the curve's exponent is
/// 2^±[`CURVE_RANGE`].
pub const CURVE_RANGE: f32 = 4.0;

/// A point-modulation input on one coordinate of a point: the coordinate
/// moves by `depth × value` of point-modulation bus `bus` (0 to 15).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointMod {
    /// Which of the [`POINT_BUSES`](super::POINT_BUSES) buses drives it.
    pub bus: u8,
    /// How far a bus value of 1 moves the coordinate, in shape units (the
    /// shape spans 0 to 1 on both axes).
    pub depth: f32,
}

/// One point of a drawn shape, and the segment that starts at it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// Where in the cycle, 0 to 1.
    pub x: f32,
    /// The value there, 0 (bottom) to 1 (top).
    pub y: f32,
    /// The bend of the segment from this point to the next, −1 to 1; 0 is a
    /// straight line, positive starts slowly and finishes fast.
    pub curve: f32,
    /// The segment from this point holds its value and jumps at the next
    /// point (a step).
    pub step: bool,
    /// Modulation of `x`.
    pub x_mod: Option<PointMod>,
    /// Modulation of `y`.
    pub y_mod: Option<PointMod>,
}

impl Point {
    /// A point at `x`, `y` with a straight segment after it.
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            curve: 0.0,
            step: false,
            x_mod: None,
            y_mod: None,
        }
    }

    /// The same point with the segment after it bent by `curve`.
    #[must_use]
    pub const fn curve(mut self, curve: f32) -> Self {
        self.curve = curve;
        self
    }

    /// The same point with a step after it.
    #[must_use]
    pub const fn step(mut self) -> Self {
        self.step = true;
        self
    }
}

/// A drawn LFO shape: up to [`MAX_POINTS`] points in order of `x`, with a
/// curve or a step between each pair. Before the first point the shape
/// holds the first point's value, and after the last it holds the last's:
/// the shape never wraps back to its first point (engine-core R92).
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    points: [Point; MAX_POINTS],
    len: usize,
    loopback: Option<usize>,
}

impl Default for Shape {
    /// The init shape: a triangle from 0 up to 1 at the middle and back.
    fn default() -> Self {
        Self::new(&[
            Point::new(0.0, 0.0),
            Point::new(0.5, 1.0),
            Point::new(1.0, 0.0),
        ])
    }
}

impl Shape {
    /// A shape of `points` (at most [`MAX_POINTS`]; any more are dropped),
    /// clamped to the unit square and sorted by `x`.
    #[must_use]
    pub fn new(points: &[Point]) -> Self {
        let mut shape = Self {
            points: [Point::new(0.0, 0.0); MAX_POINTS],
            len: 0,
            loopback: None,
        };
        shape.set_points(points);
        shape
    }

    /// Its points, in order of `x`.
    #[must_use]
    pub fn points(&self) -> &[Point] {
        &self.points[..self.len]
    }

    /// Replaces its points, as [`new`](Self::new) takes them. A loopback
    /// point beyond the new points is cleared.
    pub fn set_points(&mut self, points: &[Point]) {
        self.len = points.len().min(MAX_POINTS);
        for (slot, point) in self.points.iter_mut().zip(points) {
            *slot = Point {
                x: point.x.clamp(0.0, 1.0),
                y: point.y.clamp(0.0, 1.0),
                curve: point.curve.clamp(-1.0, 1.0),
                ..*point
            };
        }
        // Insertion sort: stable, allocation-free and quick on the few
        // points a shape holds.
        for i in 1..self.len {
            let mut j = i;
            while j > 0 && self.points[j - 1].x > self.points[j].x {
                self.points.swap(j - 1, j);
                j -= 1;
            }
        }
        if self.loopback.is_some_and(|i| i >= self.len) {
            self.loopback = None;
        }
    }

    /// The point One-shot mode loops back to, by index.
    #[must_use]
    pub const fn loopback(&self) -> Option<usize> {
        self.loopback
    }

    /// Sets the point One-shot mode loops back to (engine-core R83), or
    /// none; an index past the last point means none.
    pub fn set_loopback(&mut self, point: Option<usize>) {
        self.loopback = point.filter(|i| *i < self.len);
    }

    /// The shape's value at `x` (0 to 1), unmodulated, as an editor draws it.
    #[must_use]
    pub fn value(&self, x: f32) -> f32 {
        let (xs, ys) = self.coordinates();
        let seg = drawn_segment(self, &xs[..self.len], &ys[..self.len], x);
        seg.eval(x)
    }

    /// Fills `out` with one cycle of the shape, sample `i` at x = i / len:
    /// an LFO copied to a wavetable frame (inventory LFO-009), scaled to
    /// ±1.
    pub fn render(&self, out: &mut [f32]) {
        let (xs, ys) = self.coordinates();
        let n = out.len() as f32;
        for (i, sample) in out.iter_mut().enumerate() {
            let x = i as f32 / n;
            let seg = drawn_segment(self, &xs[..self.len], &ys[..self.len], x);
            *sample = seg.eval(x) * 2.0 - 1.0;
        }
    }

    /// A shape of straight segments through `samples` (a wavetable frame,
    /// ±1), point `i` of `count` at x = i / count as [`render`](Self::render)
    /// spaces them, taking at most [`MAX_POINTS`] evenly spaced samples; the
    /// last value holds to the end of the cycle (inventory LFO-009).
    #[must_use]
    pub fn from_samples(samples: &[f32]) -> Self {
        let mut points = [Point::new(0.0, 0.0); MAX_POINTS];
        let count = samples.len().min(MAX_POINTS);
        for (i, point) in points[..count].iter_mut().enumerate() {
            let at = i * samples.len() / count;
            *point = Point::new(i as f32 / count as f32, (samples[at] + 1.0) * 0.5);
        }
        Self::new(&points[..count])
    }

    /// Converts a stored Y that puts 0 at the top of the graph, as some
    /// preset formats do (engine-core R98), to the shape's Y, where 0 is the
    /// bottom; the conversion is its own inverse.
    #[must_use]
    pub fn y_from_top(y: f32) -> f32 {
        1.0 - y
    }

    /// Its points' coordinates, unmodulated.
    fn coordinates(&self) -> ([f32; MAX_POINTS], [f32; MAX_POINTS]) {
        let mut xs = [0.0; MAX_POINTS];
        let mut ys = [0.0; MAX_POINTS];
        for (i, p) in self.points().iter().enumerate() {
            xs[i] = p.x;
            ys[i] = p.y;
        }
        (xs, ys)
    }

    /// Moves each modulated coordinate by its bus, keeping the points in
    /// order and inside the unit square, into `xs` and `ys`.
    pub(crate) fn modulated(&self, buses: &[f32], xs: &mut [f32], ys: &mut [f32]) {
        let mut floor = 0.0f32;
        for (i, p) in self.points().iter().enumerate() {
            let x = p.x + offset(p.x_mod, buses);
            let y = p.y + offset(p.y_mod, buses);
            // Each point stays at or after the one before it.
            floor = x.clamp(floor, 1.0);
            xs[i] = floor;
            ys[i] = y.clamp(0.0, 1.0);
        }
    }

    pub(crate) const fn len(&self) -> usize {
        self.len
    }
}

/// How far `m` moves a coordinate, given the bus values.
fn offset(m: Option<PointMod>, buses: &[f32]) -> f32 {
    m.map_or(0.0, |m| {
        buses.get(usize::from(m.bus)).copied().unwrap_or(0.0) * m.depth
    })
}

/// One segment of a drawn shape, ready to evaluate:
/// `y = y0 + dy × u / (u + (1 − u) × e)` with `u = (x − x0) × inv_w`,
/// valid for `lo ≤ x < hi`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Segment {
    pub lo: f32,
    pub hi: f32,
    pub x0: f32,
    pub inv_w: f32,
    pub y0: f32,
    pub dy: f32,
    pub e: f32,
}

impl Segment {
    /// A constant `y` over `lo..hi`.
    pub fn hold(lo: f32, hi: f32, y: f32) -> Self {
        Self {
            lo,
            hi,
            x0: 0.0,
            inv_w: 0.0,
            y0: y,
            dy: 0.0,
            e: 1.0,
        }
    }

    pub fn contains(&self, x: f32) -> bool {
        self.lo <= x && x < self.hi
    }

    /// Where `x` falls in it, 0 to 1.
    pub fn u(&self, x: f32) -> f32 {
        (x - self.x0) * self.inv_w
    }

    pub fn eval(&self, x: f32) -> f32 {
        curve(self.u(x), self.y0, self.dy, self.e)
    }
}

/// The curve every drawn segment follows: from `y0` at u = 0 towards
/// `y0 + dy` at u = 1, bent by `e` (1 is straight). The scalar and SIMD
/// paths both run this, so they agree to the bit.
#[inline(always)]
pub(crate) fn curve<L: Lane>(u: L, y0: L, dy: L, e: L) -> L {
    y0 + dy * (u / (u + (L::splat(1.0) - u) * e))
}

/// The segment of `shape`, with its points at `xs`, `ys`, that holds `x`.
pub(crate) fn drawn_segment(shape: &Shape, xs: &[f32], ys: &[f32], x: f32) -> Segment {
    let n = xs.len();
    if n == 0 {
        return Segment::hold(f32::NEG_INFINITY, f32::INFINITY, 0.0);
    }
    if x < xs[0] {
        return Segment::hold(f32::NEG_INFINITY, xs[0], ys[0]);
    }
    if x >= xs[n - 1] {
        return Segment::hold(xs[n - 1], f32::INFINITY, ys[n - 1]);
    }
    // The last point at or before `x`; segments of zero width are skipped,
    // as no `x` falls inside them.
    let i = xs.partition_point(|&p| p <= x) - 1;
    let (x0, x1) = (xs[i], xs[i + 1]);
    let point = shape.points[i];
    if point.step {
        return Segment::hold(x0, x1, ys[i]);
    }
    Segment {
        lo: x0,
        hi: x1,
        x0,
        inv_w: 1.0 / (x1 - x0),
        y0: ys[i],
        dy: ys[i + 1] - ys[i],
        e: (point.curve * CURVE_RANGE).exp2(),
    }
}

/// One point of an XY path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathPoint {
    /// The X output here, 0 to 1.
    pub x: f32,
    /// The Y output here, 0 to 1.
    pub y: f32,
    /// How far the segment to the next point bows to its left, −1 to 1, as
    /// a share of half its length; 0 is straight.
    pub bend: f32,
    /// Modulation of `x`.
    pub x_mod: Option<PointMod>,
    /// Modulation of `y`.
    pub y_mod: Option<PointMod>,
}

impl PathPoint {
    /// A point at `x`, `y` with a straight segment after it.
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            bend: 0.0,
            x_mod: None,
            y_mod: None,
        }
    }
}

/// An XY path: a two-dimensional path the LFO travels once a cycle, giving
/// two outputs, X and Y (engine-core R96). Each segment's share of the
/// cycle is its share of the path's length, measured between the points as
/// drawn (modulation moves the points, not the timing), so the LFO travels
/// at a steady speed. A closed path returns from its last point to its
/// first.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    points: [PathPoint; MAX_POINTS],
    len: usize,
    closed: bool,
    /// When in the cycle each point is reached; `times[len]` is 1 for a
    /// closed path.
    times: [f32; MAX_POINTS + 1],
}

impl Default for Path {
    /// The init path: a closed diamond, so X and Y are triangles a quarter
    /// cycle apart.
    fn default() -> Self {
        Self::new(
            &[
                PathPoint::new(0.5, 0.0),
                PathPoint::new(1.0, 0.5),
                PathPoint::new(0.5, 1.0),
                PathPoint::new(0.0, 0.5),
            ],
            true,
        )
    }
}

impl Path {
    /// A path through `points` (at most [`MAX_POINTS`]), clamped to the unit
    /// square; `closed` joins the last point back to the first.
    #[must_use]
    pub fn new(points: &[PathPoint], closed: bool) -> Self {
        let mut path = Self {
            points: [PathPoint::new(0.0, 0.0); MAX_POINTS],
            len: 0,
            closed,
            times: [0.0; MAX_POINTS + 1],
        };
        path.set_points(points, closed);
        path
    }

    /// Its points, in travel order.
    #[must_use]
    pub fn points(&self) -> &[PathPoint] {
        &self.points[..self.len]
    }

    /// Whether the last point joins back to the first.
    #[must_use]
    pub const fn closed(&self) -> bool {
        self.closed
    }

    /// Replaces its points, as [`new`](Self::new) takes them.
    pub fn set_points(&mut self, points: &[PathPoint], closed: bool) {
        self.len = points.len().min(MAX_POINTS);
        self.closed = closed;
        for (slot, p) in self.points.iter_mut().zip(points) {
            *slot = PathPoint {
                x: p.x.clamp(0.0, 1.0),
                y: p.y.clamp(0.0, 1.0),
                bend: p.bend.clamp(-1.0, 1.0),
                ..*p
            };
        }
        let segments = self.segments();
        let mut total = 0.0;
        self.times[0] = 0.0;
        for j in 0..segments {
            let (a, b) = (self.points[j], self.points[(j + 1) % self.len]);
            total += (b.x - a.x).hypot(b.y - a.y);
            self.times[j + 1] = total;
        }
        for (j, t) in self.times[..=segments].iter_mut().enumerate() {
            *t = if total > 0.0 {
                *t / total
            } else if segments > 0 {
                j as f32 / segments as f32
            } else {
                0.0
            };
        }
    }

    /// How many segments it has.
    fn segments(&self) -> usize {
        match (self.len, self.closed) {
            (0 | 1, _) => 0,
            (n, true) => n,
            (n, false) => n - 1,
        }
    }

    /// Its X and Y at time `t` in the cycle (0 to 1), unmodulated.
    #[must_use]
    pub fn value(&self, t: f32) -> (f32, f32) {
        let mut xs = [0.0; MAX_POINTS];
        let mut ys = [0.0; MAX_POINTS];
        self.modulated(&[], &mut xs, &mut ys);
        path_segment(self, &xs, &ys, t).eval(t)
    }

    /// Moves each modulated coordinate by its bus, inside the unit square.
    pub(crate) fn modulated(&self, buses: &[f32], xs: &mut [f32], ys: &mut [f32]) {
        for (i, p) in self.points().iter().enumerate() {
            xs[i] = (p.x + offset(p.x_mod, buses)).clamp(0.0, 1.0);
            ys[i] = (p.y + offset(p.y_mod, buses)).clamp(0.0, 1.0);
        }
    }

    pub(crate) const fn len(&self) -> usize {
        self.len
    }
}

/// One segment of an XY path, ready to evaluate: with
/// `u = (t − t0) × inv_w` and `h = 4u(1 − u)`, `X = ax + u·dx + h·nx` and
/// `Y = ay + u·dy + h·ny` (a quadratic Bézier whose middle sits `n` from
/// the chord's), valid for `lo ≤ t < hi`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PathSegment {
    pub lo: f32,
    pub hi: f32,
    pub t0: f32,
    pub inv_w: f32,
    pub c: [f32; 6],
}

impl PathSegment {
    fn hold(lo: f32, hi: f32, x: f32, y: f32) -> Self {
        Self {
            lo,
            hi,
            t0: 0.0,
            inv_w: 0.0,
            c: [x, 0.0, 0.0, y, 0.0, 0.0],
        }
    }

    pub fn contains(&self, t: f32) -> bool {
        self.lo <= t && t < self.hi
    }

    pub fn u(&self, t: f32) -> f32 {
        (t - self.t0) * self.inv_w
    }

    pub fn eval(&self, t: f32) -> (f32, f32) {
        path_point(self.u(t), self.c)
    }
}

/// A path segment's X and Y at `u`, from its coefficients; the scalar and
/// SIMD paths both run this.
#[inline(always)]
pub(crate) fn path_point<L: Lane>(u: L, c: [L; 6]) -> (L, L) {
    let h = L::splat(4.0) * u * (L::splat(1.0) - u);
    (c[0] + u * c[1] + h * c[2], c[3] + u * c[4] + h * c[5])
}

/// The segment of `path`, with its points at `xs`, `ys`, that holds time `t`.
pub(crate) fn path_segment(path: &Path, xs: &[f32], ys: &[f32], t: f32) -> PathSegment {
    let segments = path.segments();
    if path.len == 0 {
        return PathSegment::hold(f32::NEG_INFINITY, f32::INFINITY, 0.0, 0.0);
    }
    if segments == 0 {
        return PathSegment::hold(f32::NEG_INFINITY, f32::INFINITY, xs[0], ys[0]);
    }
    if t < 0.0 {
        return PathSegment::hold(f32::NEG_INFINITY, 0.0, xs[0], ys[0]);
    }
    let end = path.times[segments];
    if t >= end {
        let last = if path.closed { 0 } else { path.len - 1 };
        return PathSegment::hold(end, f32::INFINITY, xs[last], ys[last]);
    }
    let times = &path.times[..=segments];
    let j = times.partition_point(|&p| p <= t) - 1;
    let (t0, t1) = (times[j], times[j + 1]);
    let (a, b) = (j, (j + 1) % path.len);
    let (dx, dy) = (xs[b] - xs[a], ys[b] - ys[a]);
    let bend = path.points[j].bend * 0.5;
    PathSegment {
        lo: t0,
        hi: t1,
        t0,
        inv_w: 1.0 / (t1 - t0),
        c: [xs[a], dx, -bend * dy, ys[a], dy, bend * dx],
    }
}
