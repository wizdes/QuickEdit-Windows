//! The timers behind `--timings` and `--bench-keys`, as in `qe-editor`'s `editor` example.
use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

fn file_time_100ns(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}
/// The moment Windows created this process, on the clock the timers use.
pub fn process_creation() -> Result<Instant, String> {
    let empty = FILETIME::default();
    let (mut created, mut exited, mut kernel, mut user) = (empty, empty, empty, empty);
    // SAFETY: the pseudo handle needs no closing, and the four values live through the call.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .map_err(|e| format!("could not read the process creation time: {e}"))?;
    // SAFETY: a plain query.
    let now = file_time_100ns(unsafe { GetSystemTimeAsFileTime() });
    let age = now.saturating_sub(file_time_100ns(created));
    Instant::now()
        .checked_sub(Duration::from_nanos(age.saturating_mul(100)))
        .ok_or_else(|| "the process creation time is out of range".to_string())
}

/// Timers that end at the present call, which only the renderer can see.
#[derive(Default)]
pub struct Timing {
    pub keys_waiting: RefCell<Vec<Instant>>,
    to_present: RefCell<Vec<f64>>,
    pub first_frame: Cell<Option<Instant>>,
    pub first_text: Cell<Option<Instant>>,
    /// The frame being painted holds real text.
    pub frame_has_text: Cell<bool>,
    /// The frame being painted shows the text as it is now.
    pub frame_is_current: Cell<bool>,
    pub presents: Cell<u64>,
}
impl Timing {
    pub fn after_present(&self) {
        let now = Instant::now();
        self.presents.set(self.presents.get() + 1);
        if self.first_frame.get().is_none() {
            self.first_frame.set(Some(now));
        }
        if self.frame_has_text.get() && self.first_text.get().is_none() {
            self.first_text.set(Some(now));
        }
        if self.frame_is_current.get() {
            for started in self.keys_waiting.borrow_mut().drain(..) {
                self.to_present
                    .borrow_mut()
                    .push((now - started).as_secs_f64() * 1000.0);
            }
        }
    }
    pub fn note_key(&self, started: Instant) {
        self.keys_waiting.borrow_mut().push(started);
    }
    pub fn report(&self, label: &str, process_start: Instant) {
        if let Some(at) = self.first_frame.get() {
            eprintln!(
                "first frame: {:.3} ms from process creation",
                (at - process_start).as_secs_f64() * 1000.0
            );
        }
        if let Some(at) = self.first_text.get() {
            eprintln!(
                "first text frame: {:.3} ms from process creation",
                (at - process_start).as_secs_f64() * 1000.0
            );
        }
        let mut samples = self.to_present.borrow_mut();
        if samples.is_empty() {
            return;
        }
        samples.sort_by(f64::total_cmp);
        let at = |part: f64| {
            samples[((samples.len() as f64 * part).ceil() as usize)
                .saturating_sub(1)
                .min(samples.len() - 1)]
        };
        eprintln!(
            "input to present, {label}: {} keys, p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms",
            samples.len(),
            at(0.5),
            at(0.99),
            at(1.0)
        );
    }
}
