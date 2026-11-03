//! Every number of the app's small animations, and the pure math behind them.
//!
//! The curves and most lengths come from WinUI and from the Windows theme's own animation
//! table. The lengths are shorter where the user asked for very small motion. Nothing here
//! touches a window, so that each value can be checked in a plain test.
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

/// A cubic Bezier from (0, 0) to (1, 1), as CSS and XAML write it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve(pub f64, pub f64, pub f64, pub f64);

/// Windows 11's curve for things that slide.
pub const SLIDE_CURVE: Curve = Curve(0.1, 0.9, 0.2, 1.0);
/// WinUI's `ControlFastOutSlowInKeySpline`, for a menu that opens.
pub const MENU_CURVE: Curve = Curve(0.0, 0.0, 0.0, 1.0);

/// How long a tab and the + take to slide to a new place.
pub const SLIDE_MS: f64 = 167.0;
/// How long a closed tab takes to fade away.
pub const CLOSE_FADE_MS: f64 = 100.0;
/// How long a new tab's fill takes to fade in. Its text shows at once.
pub const OPEN_FADE_MS: f64 = 83.0;
/// How long a menu takes to reveal itself.
pub const MENU_OPEN_MS: f64 = 167.0;
/// How much of a bar menu shows on its first frame.
pub const BAR_MENU_START: f32 = 0.5;
/// How much of a submenu shows on its first frame.
pub const SUBMENU_START: f32 = 0.67;
/// How long a window button's hover fill takes to come, as Terminal's does.
pub const BUTTON_FADE_IN_MS: f64 = 150.0;
/// How long a window button's hover fill takes to go.
pub const BUTTON_FADE_OUT_MS: f64 = 100.0;
/// A tab list that changed more recently than this does not animate its next change. WinUI
/// says interrupted motion "looks very bad".
pub const LIST_QUIET_MS: f64 = 1000.0;
/// The animation timer's period. Windows' timers have a grain of about 16 ms.
pub const FRAME_MS: u32 = 16;

impl Curve {
    /// How far along the curve is at this part of its time. Past either end, it holds.
    pub fn at(self, part: f64) -> f64 {
        if part <= 0.0 {
            return 0.0;
        }
        if part >= 1.0 {
            return 1.0;
        }
        let Curve(x1, y1, x2, y2) = self;
        let bezier = |a: f64, b: f64, s: f64| {
            let rest = 1.0 - s;
            3.0 * rest * rest * s * a + 3.0 * rest * s * s * b + s * s * s
        };
        // x only grows with s, so that halving finds it. 24 steps are finer than a pixel.
        let (mut low, mut high) = (0.0, 1.0);
        for _ in 0..24 {
            let middle = (low + high) / 2.0;
            if bezier(x1, x2, middle) < part {
                low = middle;
            } else {
                high = middle;
            }
        }
        bezier(y1, y2, (low + high) / 2.0)
    }
}

/// The part of a length that has gone by, from 0 to 1.
pub fn part_of(now_ms: f64, start_ms: f64, length_ms: f64) -> f64 {
    ((now_ms - start_ms) / length_ms).clamp(0.0, 1.0)
}

/// The motion clock in milliseconds. A test sets the time, and a run reads the real one.
#[derive(Clone, Debug)]
pub struct Clock {
    set: Option<Rc<Cell<f64>>>,
    start: Instant,
}
impl Clock {
    pub fn real() -> Self {
        Self {
            set: None,
            start: Instant::now(),
        }
    }
    /// A clock that shows 0 until `set_ms`.
    pub fn manual() -> Self {
        Self {
            set: Some(Rc::new(Cell::new(0.0))),
            start: Instant::now(),
        }
    }
    /// For a manual clock. A real clock ignores it.
    pub fn set_ms(&self, ms: f64) {
        if let Some(set) = &self.set {
            set.set(ms);
        }
    }
    pub fn now_ms(&self) -> f64 {
        match &self.set {
            Some(set) => set.get(),
            None => self.start.elapsed().as_secs_f64() * 1000.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curves_hold_at_their_ends_and_match_the_research_table() {
        for curve in [SLIDE_CURVE, MENU_CURVE] {
            assert_eq!(curve.at(-1.0), 0.0);
            assert_eq!(curve.at(0.0), 0.0);
            assert_eq!(curve.at(1.0), 1.0);
            assert_eq!(curve.at(2.0), 1.0);
        }
        // This one has a closed form: y = 3 p^(2/3) - 2 p.
        for part in [0.01, 0.1, 0.2, 0.5, 0.9] {
            let exact = 3.0 * f64::powf(part, 2.0 / 3.0) - 2.0 * part;
            assert!((MENU_CURVE.at(part) - exact).abs() < 1e-4, "{part}");
        }
        // The research's table: this curve over 333 ms is at 89% after 100 ms.
        assert!((SLIDE_CURVE.at(100.0 / 333.0) - 0.89).abs() < 0.01);
    }

    #[test]
    fn a_curve_never_goes_back() {
        for curve in [SLIDE_CURVE, MENU_CURVE] {
            let mut last = 0.0;
            for i in 0..=1000 {
                let value = curve.at(f64::from(i) / 1000.0);
                assert!(value + 1e-6 >= last);
                last = value;
            }
        }
    }

    #[test]
    fn a_manual_clock_shows_what_it_was_given_and_a_real_one_moves() {
        let clock = Clock::manual();
        assert_eq!(clock.now_ms(), 0.0);
        let shared = clock.clone();
        shared.set_ms(42.5);
        assert_eq!(clock.now_ms(), 42.5);
        let real = Clock::real();
        real.set_ms(1e9);
        assert!(real.now_ms() < 1e8);
    }
}
