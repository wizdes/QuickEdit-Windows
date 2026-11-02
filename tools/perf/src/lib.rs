//! The tested logic of `qe-perf`. Everything that touches Windows is in the
//! binary, so this file runs with no window and no GPU.

/// The first rounds warm the caches. Their rows are labeled and not counted.
pub const WARMUP_ROUNDS: usize = 2;

/// Thread pairs that share input while a background caller raises a window.
pub fn foreground_thread_links(caller: u32, foreground: u32, target: u32) -> Vec<(u32, u32)> {
    if foreground == 0 {
        return Vec::new();
    }
    let mut links = Vec::new();
    for thread in [caller, target] {
        if thread != 0 && thread != foreground && !links.contains(&(thread, foreground)) {
            links.push((thread, foreground));
        }
    }
    links
}

#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub exe: String,
    pub args: String,
}

impl Target {
    /// The name in report rows. The arms share one exe, so the exe alone cannot
    /// tell them apart.
    pub fn label(&self) -> String {
        if self.args.is_empty() {
            self.exe.clone()
        } else {
            format!("{} {}", self.exe, self.args)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Launch {
        targets: Vec<Target>,
        runs: usize,
        flush_standby: bool,
    },
    Typing {
        target: Target,
        keys: usize,
    },
    /// Opens Desktop Duplication and reads some frames. It shows no window and
    /// sends no keys, so it can run while the user works.
    DupCheck,
}

pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let mode = args
        .first()
        .ok_or("expected launch, typing or dup-check")?
        .as_str();
    if mode == "dup-check" {
        return if args.len() == 1 {
            Ok(Command::DupCheck)
        } else {
            Err("dup-check takes no options".into())
        };
    }
    let mut targets: Vec<Target> = Vec::new();
    let mut runs = None;
    let mut keys = None;
    let mut flush_standby = false;
    let mut i = 1;
    while i < args.len() {
        let option = args[i].as_str();
        if option == "--flush-standby" {
            flush_standby = true;
            i += 1;
            continue;
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("missing value for {option}"))?;
        match option {
            "--exe" => targets.push(Target {
                exe: value.clone(),
                args: String::new(),
            }),
            "--args" => {
                targets
                    .last_mut()
                    .ok_or("--args needs a preceding --exe")?
                    .args = value.clone()
            }
            "--runs" => runs = Some(positive_count(value)?),
            "--keys" => keys = Some(positive_count(value)?),
            _ => return Err(format!("unknown option {option}")),
        }
        i += 2;
    }
    if targets.is_empty() {
        return Err("at least one --exe is required".into());
    }
    match mode {
        "launch" if keys.is_none() => Ok(Command::Launch {
            targets,
            runs: runs.ok_or("--runs is required")?,
            flush_standby,
        }),
        "typing" if runs.is_none() && !flush_standby && targets.len() == 1 => Ok(Command::Typing {
            target: targets.remove(0),
            keys: keys.ok_or("--keys is required")?,
        }),
        _ => Err("invalid mode or options for mode".into()),
    }
}

fn positive_count(value: &str) -> Result<usize, String> {
    let count = value
        .parse::<usize>()
        .map_err(|_| format!("invalid count: {value}"))?;
    if count == 0 {
        Err("count must be positive".into())
    } else {
        Ok(count)
    }
}

/// The launch order: every round runs every target once, in the same order, so
/// slow drift in the machine hits all targets alike.
pub fn launch_schedule(runs: usize, targets: usize) -> Vec<(usize, usize)> {
    let mut order = Vec::with_capacity((runs + WARMUP_ROUNDS) * targets);
    for round in 0..runs + WARMUP_ROUNDS {
        for target in 0..targets {
            order.push((round, target));
        }
    }
    order
}

pub fn percentile(values: &[u64], percent: f64) -> Option<u64> {
    if values.is_empty() || !(0.0..=100.0).contains(&percent) {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = ((percent / 100.0 * sorted.len() as f64).ceil() as usize).max(1);
    Some(sorted[rank - 1])
}

pub fn csv_field(field: &str) -> String {
    if field.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.into()
    }
}

pub fn quote_windows_arg(s: &str) -> String {
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in s.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        if c == '"' {
            out.push_str(&"\\".repeat(backslashes * 2 + 1));
            out.push('"');
            backslashes = 0;
            continue;
        }
        out.push_str(&"\\".repeat(backslashes));
        backslashes = 0;
        out.push(c);
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

pub fn parse_launch_log(data: &str) -> Result<(i64, i64), String> {
    let record = parse_launch_record(data)?;
    Ok((record.start, record.first_frame))
}

/// One line of a spike's launch log, in QueryPerformanceCounter readings.
#[derive(Debug, PartialEq, Eq)]
pub struct LaunchRecord {
    pub start: i64,
    pub first_frame: i64,
    /// After the first frame, when DwmFlush returned.
    pub dwm_flush: Option<i64>,
    /// Arm d only: when the GPU path took over.
    pub takeover: Option<i64>,
}

pub fn parse_launch_record(data: &str) -> Result<LaunchRecord, String> {
    let mut fields = data.trim().split(',');
    let mut required = |what: &str| -> Result<i64, String> {
        fields
            .next()
            .ok_or_else(|| "invalid launch log".to_owned())?
            .parse::<i64>()
            .map_err(|_| format!("invalid {what} QPC"))
    };
    let start = required("first")?;
    let first_frame = required("second")?;
    if first_frame < start {
        return Err("launch log QPC order is reversed".into());
    }
    let mut optional = |what: &str| -> Result<Option<i64>, String> {
        match fields.next() {
            None | Some("") => Ok(None),
            Some(value) => {
                let value = value
                    .parse::<i64>()
                    .map_err(|_| format!("invalid {what} QPC"))?;
                if value < first_frame {
                    Err(format!("{what} comes before the first frame"))
                } else {
                    Ok(Some(value))
                }
            }
        }
    };
    let dwm_flush = optional("DWM flush")?;
    let takeover = optional("takeover")?;
    Ok(LaunchRecord {
        start,
        first_frame,
        dwm_flush,
        takeover,
    })
}

/// Small random numbers for key gaps and letters. It need not be strong; it
/// only stops keys from locking onto the display's refresh rate.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// A gap between keys, from 80 to 200 ms.
    pub fn gap_ms(&mut self) -> u64 {
        80 + self.next() % 121
    }
    /// The virtual-key code of a letter. A letter always draws pixels, and a
    /// space would not.
    pub fn letter_key(&mut self) -> u16 {
        0x41 + (self.next() % 26) as u16
    }
}

/// A rectangle on the virtual desktop, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl ScreenRect {
    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
    pub fn intersects(&self, other: &ScreenRect) -> bool {
        !self.is_empty()
            && !other.is_empty()
            && self.left < other.right
            && other.left < self.right
            && self.top < other.bottom
            && other.top < self.bottom
    }
    pub fn is_inside(&self, other: &ScreenRect) -> bool {
        self.left >= other.left
            && self.top >= other.top
            && self.right <= other.right
            && self.bottom <= other.bottom
    }
}

/// A window above the target in the z-order.
#[derive(Debug)]
pub struct WindowAbove {
    pub name: String,
    pub visible: bool,
    pub cloaked: bool,
    pub minimized: bool,
    pub rect: ScreenRect,
}

/// The first window that puts pixels over the target's region.
pub fn first_cover<'a>(region: &ScreenRect, above: &'a [WindowAbove]) -> Option<&'a WindowAbove> {
    above
        .iter()
        .find(|w| w.visible && !w.cloaked && !w.minimized && w.rect.intersects(region))
}

/// What must hold before every key and before any result is printed.
#[derive(Debug)]
pub struct GateFacts {
    pub window_alive: bool,
    pub foreground: bool,
    pub minimized: bool,
    pub covered_by: Option<String>,
    pub region_moved: bool,
    pub modifier_held: bool,
}

/// The reason to refuse, or `Ok` when the measurement can go on.
pub fn gate(facts: &GateFacts) -> Result<(), String> {
    if !facts.window_alive {
        return Err("the target window is gone".into());
    }
    if facts.minimized {
        return Err("the target window is minimized".into());
    }
    if !facts.foreground {
        return Err("the target window is not the foreground window".into());
    }
    if let Some(name) = &facts.covered_by {
        return Err(format!("another window covers the target: {name}"));
    }
    if facts.region_moved {
        return Err("the target window moved or changed size".into());
    }
    if facts.modifier_held {
        return Err("a Shift, Ctrl, Alt or Windows key is held down".into());
    }
    Ok(())
}

/// Screen pixels are not steady on every machine. On this PC the same still
/// pixel reads 17 and then 16 from one captured frame to the next, because
/// something between the desktop and the capture dithers the image. An exact
/// compare would call every frame a change. Black text on white moves a channel
/// by far more than this.
pub const NOISE_TOLERANCE: u8 = 16;

/// Whether two BGRA images differ by more than noise. Alpha is ignored, because
/// the desktop image has no meaningful alpha.
pub fn pixels_differ(old: &[u8], new: &[u8]) -> bool {
    old.as_chunks::<4>()
        .0
        .iter()
        .zip(new.as_chunks::<4>().0)
        .any(|(a, b)| (0..3).any(|c| a[c].abs_diff(b[c]) > NOISE_TOLERANCE))
}

/// One frame in which the watched pixels changed, in QPC readings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    /// The frame's LastPresentTime from Desktop Duplication.
    pub present: i64,
    /// When the capture thread got the frame.
    pub acquired: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Matched {
    /// One entry per key. `None` is a lost key.
    pub keys: Vec<Option<Change>>,
    /// Pixel changes that no key explains.
    pub unattributed: usize,
}

/// Gives each key the first unused change at or after its send time and within
/// `timeout` ticks. Keys and changes are both in time order.
///
/// Two keys in one frame would shift later matches by one key and show as a
/// lost key at the end. The 80 ms minimum gap is far longer than a frame, so
/// this means something is badly wrong, and the lost count reports it.
pub fn match_keys(keys: &[i64], changes: &[Change], timeout: i64) -> Matched {
    let mut next = 0;
    let mut unattributed = 0;
    let mut matched = Vec::with_capacity(keys.len());
    for key in keys {
        while next < changes.len() && changes[next].present < *key {
            unattributed += 1;
            next += 1;
        }
        if next < changes.len() && changes[next].present <= key + timeout {
            matched.push(Some(changes[next]));
            next += 1;
        } else {
            matched.push(None);
        }
    }
    unattributed += changes.len() - next;
    Matched {
        keys: matched,
        unattributed,
    }
}

/// `received,after_present,character` lines from a spike's `--type-log`.
pub fn parse_type_log(data: &str) -> Result<Vec<(i64, i64)>, String> {
    let mut rows = Vec::new();
    for line in data.lines().filter(|l| !l.is_empty()) {
        let mut fields = line.split(',');
        let mut number = || -> Result<i64, String> {
            fields
                .next()
                .and_then(|f| f.parse::<i64>().ok())
                .ok_or_else(|| format!("bad type log line: {line}"))
        };
        let (received, after) = (number()?, number()?);
        if after < received {
            return Err(format!("type log QPC order is reversed: {line}"));
        }
        rows.push((received, after));
    }
    Ok(rows)
}

pub fn ticks_to_ms(ticks: u64, hz: i64) -> f64 {
    ticks as f64 * 1000.0 / hz as f64
}

/// The final numbers of a typing run.
#[derive(Debug, PartialEq)]
pub struct TypingReport {
    pub keys: usize,
    pub lost: usize,
    pub unattributed: usize,
    pub screen_p50_ms: f64,
    pub screen_p95_ms: f64,
    pub captured_p50_ms: f64,
    pub captured_p95_ms: f64,
    pub app_p50_ms: Option<f64>,
    pub app_p95_ms: Option<f64>,
}

impl TypingReport {
    pub fn meets_budget(&self) -> bool {
        self.lost == 0 && self.screen_p50_ms < 25.0 && self.screen_p95_ms < 33.0
    }
}

/// Turns the raw times into numbers, or refuses.
///
/// `app_log` is the spike's own log. A spike has no caret or animation, so any
/// unexplained pixel change or a wrong key count means something else was at
/// work, and the run proves nothing. Other programs may blink a caret, so for
/// them the unexplained count is printed for the reader to judge.
pub fn typing_report(
    key_times: &[i64],
    changes: &[Change],
    app_log: Option<&[(i64, i64)]>,
    timeout: i64,
    hz: i64,
) -> Result<TypingReport, String> {
    if hz <= 0 {
        return Err("the QPC frequency is not positive".into());
    }
    let matched = match_keys(key_times, changes, timeout);
    let mut screen = Vec::new();
    let mut captured = Vec::new();
    for (key, change) in key_times.iter().zip(&matched.keys) {
        if let Some(change) = change {
            screen.push((change.present - key) as u64);
            captured.push((change.acquired - key).max(0) as u64);
        }
    }
    if screen.is_empty() {
        return Err("no key changed the screen".into());
    }
    if let Some(log) = app_log {
        if log.len() != key_times.len() {
            return Err(format!(
                "the target logged {} keys but {} were sent",
                log.len(),
                key_times.len()
            ));
        }
        if matched.unattributed > 0 {
            return Err(format!(
                "{} pixel changes came from no key",
                matched.unattributed
            ));
        }
    }
    let app: Option<Vec<u64>> =
        app_log.map(|log| log.iter().map(|(r, a)| (a - r) as u64).collect());
    let ms = |values: &[u64], p| {
        percentile(values, p)
            .map(|t| ticks_to_ms(t, hz))
            .ok_or_else(|| "no samples".to_owned())
    };
    Ok(TypingReport {
        keys: key_times.len(),
        lost: matched.keys.iter().filter(|k| k.is_none()).count(),
        unattributed: matched.unattributed,
        screen_p50_ms: ms(&screen, 50.0)?,
        screen_p95_ms: ms(&screen, 95.0)?,
        captured_p50_ms: ms(&captured, 50.0)?,
        captured_p95_ms: ms(&captured, 95.0)?,
        app_p50_ms: app.as_deref().map(|a| ms(a, 50.0)).transpose()?,
        app_p95_ms: app.as_deref().map(|a| ms(a, 95.0)).transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).into()).collect()
    }

    #[test]
    fn parse_interleaved_targets_and_warmup_count() {
        let command = parse_args(&words(&[
            "launch",
            "--exe",
            "a.exe",
            "--args",
            "--arm a",
            "--exe",
            "b.exe",
            "--runs",
            "3",
            "--flush-standby",
        ]))
        .unwrap();
        assert_eq!(
            command,
            Command::Launch {
                targets: vec![
                    Target {
                        exe: "a.exe".into(),
                        args: "--arm a".into()
                    },
                    Target {
                        exe: "b.exe".into(),
                        args: "".into()
                    }
                ],
                runs: 3,
                flush_standby: true
            }
        );
    }

    #[test]
    fn reject_missing_and_zero_counts() {
        assert!(parse_args(&words(&["launch", "--runs", "3"])).is_err());
        assert!(parse_args(&words(&["typing", "--exe", "a.exe", "--keys", "0"])).is_err());
        assert!(parse_args(&words(&["launch", "--exe", "a.exe", "--runs", "no"])).is_err());
    }

    #[test]
    fn nearest_rank_percentiles() {
        assert_eq!(percentile(&[4, 1, 3, 2], 50.0), Some(2));
        assert_eq!(percentile(&[4, 1, 3, 2], 95.0), Some(4));
        assert_eq!(percentile(&[], 95.0), None);
    }

    #[test]
    fn csv_quotes_special_fields() {
        assert_eq!(csv_field("simple"), "simple");
        assert_eq!(csv_field("a,\"b\n"), "\"a,\"\"b\n\"");
    }

    #[test]
    fn windows_argument_quoting_keeps_spaces_and_trailing_slashes() {
        assert_eq!(
            quote_windows_arg("C:\\Program Files\\"),
            "\"C:\\Program Files\\\\\""
        );
        assert_eq!(quote_windows_arg("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn launch_log_rejects_reversed_counters() {
        assert_eq!(parse_launch_log("10,20\n").unwrap(), (10, 20));
        assert!(parse_launch_log("20,10\n").is_err());
        assert!(parse_launch_log("bad\n").is_err());
    }

    #[test]
    fn launch_record_reads_the_optional_stamps() {
        assert_eq!(
            parse_launch_record("10,20,25,\n").unwrap(),
            LaunchRecord {
                start: 10,
                first_frame: 20,
                dwm_flush: Some(25),
                takeover: None
            }
        );
        assert_eq!(
            parse_launch_record("10,20,25,90\n").unwrap().takeover,
            Some(90)
        );
        assert_eq!(parse_launch_record("10,20").unwrap().dwm_flush, None);
        assert!(parse_launch_record("10,20,5,").is_err());
        assert!(parse_launch_record("10,20,25,5").is_err());
        assert!(parse_launch_record("10,20,x,").is_err());
    }

    #[test]
    fn launch_runs_interleave_all_four_arms_in_every_round() {
        let order = launch_schedule(3, 4);
        assert_eq!(order.len(), (3 + WARMUP_ROUNDS) * 4);
        for (position, (round, target)) in order.iter().enumerate() {
            assert_eq!(*round, position / 4);
            assert_eq!(*target, position % 4);
        }
    }

    #[test]
    fn four_arm_command_line_keeps_each_arm_apart_in_reports() {
        let mut items = vec!["launch".to_owned(), "--runs".to_owned(), "30".to_owned()];
        for arm in ["a", "b", "c", "d"] {
            items.extend(words(&["--exe", "spike-renderers.exe", "--args"]));
            items.push(format!("--arm {arm}"));
        }
        let Command::Launch { targets, .. } = parse_args(&items).unwrap() else {
            panic!("expected launch");
        };
        let labels: Vec<String> = targets.iter().map(Target::label).collect();
        assert_eq!(labels.len(), 4);
        assert_eq!(labels[2], "spike-renderers.exe --arm c");
        for (i, label) in labels.iter().enumerate() {
            assert!(!labels[i + 1..].contains(label), "{label} repeats");
        }
    }

    #[test]
    fn dup_check_takes_no_options() {
        assert_eq!(parse_args(&words(&["dup-check"])), Ok(Command::DupCheck));
        assert!(parse_args(&words(&["dup-check", "--exe", "a.exe"])).is_err());
    }

    #[test]
    fn key_gaps_stay_from_80_to_200_ms_and_reach_both_ends() {
        let mut rng = Rng::new(12345);
        let gaps: Vec<u64> = (0..5000).map(|_| rng.gap_ms()).collect();
        assert!(gaps.iter().all(|g| (80..=200).contains(g)));
        assert_eq!(gaps.iter().min(), Some(&80));
        assert_eq!(gaps.iter().max(), Some(&200));
    }

    #[test]
    fn keys_are_always_letters() {
        let mut rng = Rng::new(0);
        for _ in 0..1000 {
            assert!((0x41..=0x5a).contains(&rng.letter_key()));
        }
    }

    #[test]
    fn rectangles_that_only_touch_do_not_intersect() {
        let a = ScreenRect {
            left: 0,
            top: 0,
            right: 10,
            bottom: 10,
        };
        let touching = ScreenRect {
            left: 10,
            top: 0,
            right: 20,
            bottom: 10,
        };
        let overlapping = ScreenRect {
            left: 9,
            top: 9,
            right: 20,
            bottom: 20,
        };
        assert!(!a.intersects(&touching));
        assert!(a.intersects(&overlapping));
        assert!(a.is_inside(&a));
        assert!(!overlapping.is_inside(&a));
    }

    fn above(name: &str, visible: bool, cloaked: bool, minimized: bool, left: i32) -> WindowAbove {
        WindowAbove {
            name: name.into(),
            visible,
            cloaked,
            minimized,
            rect: ScreenRect {
                left,
                top: 0,
                right: left + 10,
                bottom: 10,
            },
        }
    }

    #[test]
    fn only_a_shown_window_over_the_region_counts_as_a_cover() {
        let region = ScreenRect {
            left: 0,
            top: 0,
            right: 10,
            bottom: 10,
        };
        let windows = [
            above("hidden", false, false, false, 0),
            above("cloaked", false, true, false, 0),
            above("cloaked and visible", true, true, false, 0),
            above("minimized", true, false, true, 0),
            above("beside", true, false, false, 50),
            above("over", true, false, false, 5),
            above("also over", true, false, false, 1),
        ];
        assert_eq!(first_cover(&region, &windows).unwrap().name, "over");
        assert!(first_cover(&region, &windows[..5]).is_none());
    }

    fn clear_gate() -> GateFacts {
        GateFacts {
            window_alive: true,
            foreground: true,
            minimized: false,
            covered_by: None,
            region_moved: false,
            modifier_held: false,
        }
    }

    #[test]
    fn the_gate_passes_only_when_all_is_clear() {
        assert!(gate(&clear_gate()).is_ok());
    }

    #[test]
    fn the_gate_refuses_a_covered_or_background_target() {
        let mut covered = clear_gate();
        covered.covered_by = Some("Notepad".into());
        assert!(gate(&covered).unwrap_err().contains("Notepad"));
        let mut background = clear_gate();
        background.foreground = false;
        assert!(gate(&background).unwrap_err().contains("foreground"));
    }

    #[test]
    fn the_gate_refuses_every_other_bad_state() {
        let mut gone = clear_gate();
        gone.window_alive = false;
        assert!(gate(&gone).is_err());
        let mut minimized = clear_gate();
        minimized.minimized = true;
        assert!(gate(&minimized).is_err());
        let mut moved = clear_gate();
        moved.region_moved = true;
        assert!(gate(&moved).is_err());
        let mut modifier = clear_gate();
        modifier.modifier_held = true;
        assert!(gate(&modifier).is_err());
    }

    #[test]
    fn dither_noise_is_not_a_change_but_a_drawn_glyph_is() {
        // Numbers seen on this PC: a still pixel moves between 17 and 16.
        let still = [17, 19, 21, 255, 17, 19, 21, 255];
        let dithered = [16, 19, 20, 255, 17, 18, 21, 255];
        assert!(!pixels_differ(&still, &dithered));
        let white = [255, 255, 255, 255, 255, 255, 255, 255];
        let one_glyph_pixel = [255, 255, 255, 255, 0, 0, 0, 255];
        assert!(pixels_differ(&white, &one_glyph_pixel));
        // A faint ClearType edge on one channel still counts.
        let faint_edge = [255, 255, 255, 255, 255, 200, 255, 255];
        assert!(pixels_differ(&white, &faint_edge));
    }

    #[test]
    fn a_change_in_alpha_alone_is_not_a_change() {
        assert!(!pixels_differ(&[9, 9, 9, 255], &[9, 9, 9, 0]));
    }

    fn change(present: i64) -> Change {
        Change {
            present,
            acquired: present + 1,
        }
    }

    #[test]
    fn each_key_takes_the_first_change_after_it() {
        let matched = match_keys(&[100, 200], &[change(110), change(230)], 50);
        assert_eq!(matched.keys, vec![Some(change(110)), Some(change(230))]);
        assert_eq!(matched.unattributed, 0);
    }

    #[test]
    fn a_change_before_any_key_or_after_all_keys_is_unattributed() {
        let matched = match_keys(&[100], &[change(50), change(110), change(120)], 50);
        assert_eq!(matched.keys, vec![Some(change(110))]);
        assert_eq!(matched.unattributed, 2);
    }

    #[test]
    fn a_key_with_no_change_in_time_is_lost() {
        let matched = match_keys(&[100, 200], &[change(210)], 50);
        assert_eq!(matched.keys, vec![None, Some(change(210))]);
        let late = match_keys(&[100], &[change(151)], 50);
        assert_eq!(late.keys, vec![None]);
        assert_eq!(late.unattributed, 1);
    }

    #[test]
    fn a_slow_frame_can_land_after_the_next_key_was_sent() {
        let matched = match_keys(&[100, 150], &[change(170), change(180)], 100);
        assert_eq!(matched.keys, vec![Some(change(170)), Some(change(180))]);
    }

    #[test]
    fn type_log_lines_are_read_and_checked() {
        assert_eq!(
            parse_type_log("1,2,97\n3,5,98\n").unwrap(),
            vec![(1, 2), (3, 5)]
        );
        assert_eq!(parse_type_log("").unwrap(), vec![]);
        assert!(parse_type_log("5,1,97\n").is_err());
        assert!(parse_type_log("x,1,97\n").is_err());
    }

    #[test]
    fn the_report_gives_percentiles_in_milliseconds() {
        // 1000 ticks per second, so one tick is 1 ms.
        let keys = [1000, 2000, 3000, 4000];
        let changes = [change(1010), change(2020), change(3030), change(4040)];
        let log = [(1, 3), (2, 4), (3, 5), (4, 8)];
        let report = typing_report(&keys, &changes, Some(&log), 500, 1000).unwrap();
        assert_eq!(report.keys, 4);
        assert_eq!(report.lost, 0);
        assert_eq!(report.screen_p50_ms, 20.0);
        assert_eq!(report.screen_p95_ms, 40.0);
        assert_eq!(report.captured_p50_ms, 21.0);
        assert_eq!(report.app_p50_ms, Some(2.0));
        assert_eq!(report.app_p95_ms, Some(4.0));
        assert!(!report.meets_budget(), "40 ms p95 misses 33 ms");
    }

    #[test]
    fn the_budget_needs_both_percentiles_and_no_lost_keys() {
        let keys = [1000, 2000];
        let good = typing_report(&keys, &[change(1020), change(2020)], None, 500, 1000).unwrap();
        assert!(good.meets_budget());
        let lost = typing_report(&keys, &[change(1020)], None, 500, 1000).unwrap();
        assert_eq!(lost.lost, 1);
        assert!(!lost.meets_budget());
    }

    #[test]
    fn a_spike_run_with_unexplained_pixels_or_wrong_key_count_is_refused() {
        let keys = [1000, 2000];
        let changes = [change(500), change(1020), change(2020)];
        let log = [(1, 2), (3, 4)];
        let extra_change = typing_report(&keys, &changes, Some(&log), 500, 1000);
        assert!(extra_change.unwrap_err().contains("came from no key"));
        let good_changes = [change(1020), change(2020)];
        let short_log = [(1, 2)];
        let short = typing_report(&keys, &good_changes, Some(&short_log), 500, 1000);
        assert!(short.unwrap_err().contains("logged 1 keys"));
    }

    #[test]
    fn a_program_with_a_blinking_caret_still_reports_the_extra_changes() {
        let keys = [1000, 2000];
        let changes = [change(500), change(1020), change(2020)];
        let report = typing_report(&keys, &changes, None, 500, 1000).unwrap();
        assert_eq!(report.unattributed, 1);
        assert_eq!(report.app_p50_ms, None);
    }

    #[test]
    fn a_run_where_no_key_changed_the_screen_is_refused() {
        assert!(typing_report(&[1000], &[], None, 500, 1000).is_err());
    }
}
