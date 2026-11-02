//! The map box's fade, as `DocumentMapView.stepFade` has it.

/// The time for a full fade from 0 to 1. A fade that starts part way takes its share.
pub const FADE_IN_SECONDS: f64 = 0.15;
pub const FADE_OUT_SECONDS: f64 = 0.25;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fade {
    alpha: f32,
    from: f32,
    target: f32,
    start: f64,
    active: bool,
}

impl Fade {
    /// How much of the box to draw now, 0 (none) to 1.
    pub fn alpha(&self) -> f32 {
        self.alpha
    }
    /// True while a timer must run. It is false whenever nothing moves.
    pub fn fading(&self) -> bool {
        self.active
    }
    /// Starts toward `show` from where the box is now. With `jump`, it goes there at once:
    /// Reduce Motion, or a map that is hidden.
    pub fn aim(&mut self, show: bool, now: f64, jump: bool) {
        let target = if show { 1.0 } else { 0.0 };
        self.step(now);
        self.from = self.alpha;
        self.target = target;
        self.start = now;
        if jump || self.alpha == target {
            self.alpha = target;
            self.active = false;
        } else {
            self.active = true;
        }
    }
    /// Moves the fade to `now` and returns the new alpha.
    pub fn step(&mut self, now: f64) -> f32 {
        if !self.active {
            return self.alpha;
        }
        let base = if self.target > self.from {
            FADE_IN_SECONDS
        } else {
            FADE_OUT_SECONDS
        };
        let duration = base * f64::from((self.target - self.from).abs());
        let mut progress = if duration > 0.0 {
            ((now - self.start) / duration).clamp(0.0, 1.0)
        } else {
            1.0
        };
        // `start + d - start` can come out a hair under `d`.
        if progress > 0.999 {
            progress = 1.0;
        }
        self.alpha = self.from + (self.target - self.from) * progress as f32;
        if progress >= 1.0 {
            self.alpha = self.target;
            self.active = false;
        }
        self.alpha
    }
}
