//! Device pixel rectangles, the rule that turns dips into pixels, and region lists.
use qe_api::Rect;

/// Results of `f32` sums can land a hair under a half pixel. Without this nudge the
/// same content could round differently after a whole-pixel scroll.
const NUDGE: f64 = 1.0 / 1024.0;

/// The one rounding rule for edges and baselines. It rounds up at exact halves for
/// positive and negative values alike, so adding whole pixels never changes the
/// result by anything other than those pixels.
#[inline]
pub(crate) fn snap(device: f64) -> i32 {
    (device + 0.5 + NUDGE).floor() as i32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Area {
    pub x: i32,
    pub y: i32,
    pub r: i32,
    pub b: i32,
}
impl Area {
    pub const EMPTY: Self = Self {
        x: 0,
        y: 0,
        r: 0,
        b: 0,
    };
    pub fn full(w: u32, h: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            r: w as i32,
            b: h as i32,
        }
    }
    /// Edges round to the nearest pixel, so rectangles that touch in dips touch in
    /// pixels and never overlap. A rectangle with any size stays at least 1 pixel.
    pub fn snapped(v: Rect, scale: f64) -> Self {
        if v.width.0 <= 0.0 || v.height.0 <= 0.0 {
            return Self::EMPTY;
        }
        let x = snap(f64::from(v.x.0) * scale);
        let y = snap(f64::from(v.y.0) * scale);
        let r = snap((f64::from(v.x.0) + f64::from(v.width.0)) * scale);
        let b = snap((f64::from(v.y.0) + f64::from(v.height.0)) * scale);
        Self {
            x,
            y,
            r: if r == x { x.saturating_add(1) } else { r },
            b: if b == y { y.saturating_add(1) } else { b },
        }
    }
    /// Every pixel that the rectangle touches. This is for damage, where extra
    /// pixels only cost time.
    pub fn outward(v: Rect, scale: f64) -> Self {
        if v.width.0 <= 0.0 || v.height.0 <= 0.0 {
            return Self::EMPTY;
        }
        Self {
            x: (f64::from(v.x.0) * scale + NUDGE).floor() as i32,
            y: (f64::from(v.y.0) * scale + NUDGE).floor() as i32,
            r: ((f64::from(v.x.0) + f64::from(v.width.0)) * scale - NUDGE).ceil() as i32,
            b: ((f64::from(v.y.0) + f64::from(v.height.0)) * scale - NUDGE).ceil() as i32,
        }
    }
    pub fn clip(self, other: Self) -> Self {
        Self {
            x: self.x.max(other.x),
            y: self.y.max(other.y),
            r: self.r.min(other.r),
            b: self.b.min(other.b),
        }
    }
    pub fn valid(self) -> bool {
        self.x < self.r && self.y < self.b
    }
    pub fn pixels(self) -> u64 {
        if self.valid() {
            (self.r - self.x) as u64 * (self.b - self.y) as u64
        } else {
            0
        }
    }
    fn union(self, other: Self) -> Self {
        Self {
            x: self.x.min(other.x),
            y: self.y.min(other.y),
            r: self.r.max(other.r),
            b: self.b.max(other.b),
        }
    }
    fn overlaps(self, other: Self) -> bool {
        self.clip(other).valid()
    }
}

const MOST_REGIONS: usize = 16;

/// A short list of rectangles that never overlap. If two overlapped, a translucent
/// patch inside both would blend twice.
#[derive(Default)]
pub(crate) struct Regions {
    pub list: Vec<Area>,
}
impl Regions {
    pub fn clear(&mut self) {
        self.list.clear();
    }
    pub fn add(&mut self, area: Area) {
        if !area.valid() {
            return;
        }
        let mut area = area;
        let mut i = 0;
        while i < self.list.len() {
            let other = self.list[i];
            if area.overlaps(other) || cheap_to_merge(area, other) {
                area = area.union(other);
                self.list.swap_remove(i);
                i = 0;
            } else {
                i += 1;
            }
        }
        self.list.push(area);
        while self.list.len() > MOST_REGIONS {
            self.merge_cheapest_pair();
        }
    }
    fn merge_cheapest_pair(&mut self) {
        let mut best = (0, 1, u64::MAX);
        for a in 0..self.list.len() {
            for b in a + 1..self.list.len() {
                let (p, q) = (self.list[a], self.list[b]);
                let waste = p.union(q).pixels() - p.pixels() - q.pixels();
                if waste < best.2 {
                    best = (a, b, waste);
                }
            }
        }
        let other = self.list.swap_remove(best.1);
        let first = self.list.swap_remove(best.0);
        // Adding again folds in anything that the new, larger rectangle now overlaps.
        self.add(first.union(other));
    }
    /// Moves what is waiting to be redrawn along with a scroll of `scrolled`. A part
    /// outside the scrolled rectangle stays where it is.
    pub fn scroll(&mut self, scrolled: Area, shift: i32) {
        let old = std::mem::take(&mut self.list);
        for area in old {
            if !area.overlaps(scrolled) {
                self.add(area);
                continue;
            }
            let inside = area.clip(scrolled);
            self.add(
                Area {
                    y: inside.y.saturating_add(shift),
                    b: inside.b.saturating_add(shift),
                    ..inside
                }
                .clip(scrolled),
            );
            self.add(Area {
                b: scrolled.y,
                ..area
            });
            self.add(Area {
                y: scrolled.b,
                ..area
            });
            self.add(Area {
                r: scrolled.x,
                y: inside.y,
                b: inside.b,
                ..area
            });
            self.add(Area {
                x: scrolled.r,
                y: inside.y,
                b: inside.b,
                ..area
            });
        }
    }
}
/// Rectangles that touch or nearly touch are cheaper to paint as one, when the
/// union adds few pixels that no one asked for.
fn cheap_to_merge(a: Area, b: Area) -> bool {
    // The caller has ruled out overlap, so this cannot go below zero.
    let union = a.union(b).pixels();
    union - a.pixels() - b.pixels() <= union / 8
}
