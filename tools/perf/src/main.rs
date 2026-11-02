// `vcall!` exists to make an unsafe call with its arguments. It is private to
// this crate, so the warning about hiding unsafe inside a macro does not apply.
#![allow(clippy::macro_metavars_in_unsafe)]

mod dup;
mod typing;
mod win;

use qe_perf::{
    Command as PerfCommand, Target, WARMUP_ROUNDS, csv_field, launch_schedule, parse_args,
    parse_launch_record, percentile, quote_windows_arg, ticks_to_ms,
};
use std::{
    env, fs, mem,
    path::Path,
    ptr, thread,
    time::{Duration, Instant},
};
use win::{H, ProcessInfo, Rect, StartupInfo, TokenPrivileges, qpc, wide};

pub struct Child {
    pub info: ProcessInfo,
}

impl Drop for Child {
    fn drop(&mut self) {
        unsafe {
            if win::WaitForSingleObject(self.info.process, 0) == 0x102 {
                win::TerminateProcess(self.info.process, 0);
                win::WaitForSingleObject(self.info.process, 1000);
            }
            win::CloseHandle(self.info.thread);
            win::CloseHandle(self.info.process);
        }
    }
}

pub fn spawn(target: &Target, extra: &str) -> Result<(Child, i64), String> {
    let mut cmd = wide(&format!(
        "{} {} {}",
        quote_windows_arg(&target.exe),
        target.args,
        extra
    ));
    let exe = wide(&target.exe);
    let si = StartupInfo {
        cb: mem::size_of::<StartupInfo>() as u32,
        reserved: ptr::null_mut(),
        desktop: ptr::null_mut(),
        title: ptr::null_mut(),
        x: 0,
        y: 0,
        w: 0,
        h: 0,
        chars_x: 0,
        chars_y: 0,
        fill: 0,
        flags: 0,
        show: 0,
        reserved2: 0,
        reserved_ptr: ptr::null_mut(),
        stdin: ptr::null_mut(),
        stdout: ptr::null_mut(),
        stderr: ptr::null_mut(),
    };
    let mut pi = unsafe { mem::zeroed::<ProcessInfo>() };
    let before = qpc();
    let ok = unsafe {
        win::CreateProcessW(
            exe.as_ptr(),
            cmd.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
            0,
            ptr::null_mut(),
            ptr::null(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(format!("CreateProcessW failed: {}", unsafe {
            win::GetLastError()
        }));
    }
    Ok((Child { info: pi }, before))
}

unsafe extern "system" fn find_window(hwnd: H, param: isize) -> i32 {
    let pair = unsafe { &mut *(param as *mut (u32, H)) };
    let mut pid = 0;
    unsafe { win::GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == pair.0 && unsafe { win::IsWindowVisible(hwnd) } != 0 {
        let mut rect = Rect::default();
        if unsafe { win::GetWindowRect(hwnd, &mut rect) } != 0
            && rect.right > rect.left
            && rect.bottom > rect.top
        {
            pair.1 = hwnd;
            return 0;
        }
    }
    1
}

pub fn visible_window(pid: u32) -> Option<H> {
    let mut pair: (u32, H) = (pid, ptr::null_mut());
    unsafe { win::EnumWindows(find_window, &mut pair as *mut _ as isize) };
    if pair.1.is_null() { None } else { Some(pair.1) }
}

pub fn wait_window(child: &Child, timeout: Duration) -> Result<i64, String> {
    unsafe { win::WaitForInputIdle(child.info.process, 5000) };
    let deadline = Instant::now() + timeout;
    loop {
        if visible_window(child.info.pid).is_some() {
            return Ok(qpc());
        }
        if unsafe { win::WaitForSingleObject(child.info.process, 0) } == 0 {
            return Err("process exited before a visible window".into());
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for a visible window".into());
        }
        thread::sleep(Duration::from_millis(2))
    }
}

pub fn is_spike(exe: &str) -> bool {
    Path::new(exe)
        .file_stem()
        .and_then(|x| x.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("spike-renderers"))
}

/// Purging the standby list needs this privilege turned on, even for an
/// administrator. A failure here is reported by the purge that follows.
fn enable_purge_privilege() -> Result<(), String> {
    const TOKEN_QUERY_AND_ADJUST: u32 = 0x8 | 0x20;
    const SE_PRIVILEGE_ENABLED: u32 = 2;
    let mut token = ptr::null_mut();
    if unsafe {
        win::OpenProcessToken(win::GetCurrentProcess(), TOKEN_QUERY_AND_ADJUST, &mut token)
    } == 0
    {
        return Err(format!("OpenProcessToken: error {}", unsafe {
            win::GetLastError()
        }));
    }
    let name = wide("SeProfileSingleProcessPrivilege");
    let mut luid = [0u32; 2];
    let found = unsafe { win::LookupPrivilegeValueW(ptr::null(), name.as_ptr(), &mut luid) };
    let result = if found == 0 {
        Err(format!("LookupPrivilegeValueW: error {}", unsafe {
            win::GetLastError()
        }))
    } else {
        let new = TokenPrivileges {
            count: 1,
            luid_low: luid[0],
            luid_high: luid[1] as i32,
            attributes: SE_PRIVILEGE_ENABLED,
        };
        let adjusted = unsafe {
            win::AdjustTokenPrivileges(token, 0, &new, 0, ptr::null_mut(), ptr::null_mut())
        };
        // AdjustTokenPrivileges returns success even when it changed nothing,
        // so the last error is the real answer. 1300 means "not all assigned".
        let error = unsafe { win::GetLastError() };
        if adjusted == 0 || error != 0 {
            Err(format!("AdjustTokenPrivileges: error {error}"))
        } else {
            Ok(())
        }
    };
    unsafe { win::CloseHandle(token) };
    result
}

/// Everything `run_launch` learned about one target.
#[derive(Default, Clone)]
struct Samples {
    first_frame: Vec<u64>,
    dwm_flush: Vec<u64>,
    takeover: Vec<u64>,
}

/// The stamps of one launch of a spike.
struct SpikeRun {
    before: i64,
    first: i64,
    main_to_frame: i64,
    flushed: Option<i64>,
    takeover: Option<i64>,
}

fn run_spike(target: &Target, log: &Path) -> Result<SpikeRun, String> {
    let extra = format!(
        "--launch-log {} --exit-after-first-frame",
        quote_windows_arg(&log.to_string_lossy())
    );
    let (child, before) = spawn(target, &extra)?;
    let err_path = format!("{}.err", log.to_string_lossy());
    // The stamps are in the file, so waiting for the exit adds nothing to them.
    // It also means the file is never read half-written.
    let waited = unsafe { win::WaitForSingleObject(child.info.process, 20_000) };
    if waited != 0 {
        return Err(format!("{} did not exit in 20 s", target.label()));
    }
    let mut code = 0;
    if unsafe { win::GetExitCodeProcess(child.info.process, &mut code) } == 0 {
        return Err("GetExitCodeProcess failed".into());
    }
    if code != 0 {
        let detail = fs::read_to_string(&err_path).unwrap_or_else(|e| format!("no .err file: {e}"));
        let _ = fs::remove_file(&err_path);
        return Err(format!(
            "{} exited with code {code}: {detail}",
            target.label()
        ));
    }
    let data = fs::read_to_string(log)
        .map_err(|e| format!("{} left no launch log: {e}", target.label()))?;
    fs::remove_file(log).map_err(|e| format!("removing the launch log: {e}"))?;
    let record = parse_launch_record(&data)?;
    Ok(SpikeRun {
        before,
        first: record.first_frame,
        main_to_frame: record.first_frame - record.start,
        flushed: record.dwm_flush,
        takeover: record.takeover,
    })
}

fn run_launch(targets: Vec<Target>, runs: usize, flush: bool) -> Result<(), String> {
    let hz = win::frequency();
    if hz <= 0 {
        return Err("QueryPerformanceFrequency failed".into());
    }
    let mut can_flush = flush;
    if flush && let Err(e) = enable_purge_privilege() {
        println!("SKIPPED standby-list flush, so these runs are NOT cold: {e}");
        can_flush = false;
    }
    println!(
        "target,method,phase,run,create_to_frame_ms,main_to_frame_ms,create_to_dwm_flush_ms,create_to_takeover_ms,cache"
    );
    let ms = |ticks: i64| ticks_to_ms(ticks.max(0) as u64, hz);
    let optional = |stamp: Option<i64>, before: i64| {
        stamp.map_or(String::new(), |s| format!("{:.3}", ms(s - before)))
    };
    let mut samples = vec![Samples::default(); targets.len()];
    for (round, index) in launch_schedule(runs, targets.len()) {
        let target = &targets[index];
        let mut cache = "not_flushed";
        if can_flush {
            let purge: u32 = 4;
            let status = unsafe { win::NtSetSystemInformation(80, &purge, 4) };
            if status < 0 {
                println!(
                    "SKIPPED standby-list flush, so later runs are NOT cold: NtSetSystemInformation returned NTSTATUS {status:#010x}; administrator rights are usually required"
                );
                can_flush = false;
            } else {
                cache = "flushed";
            }
        }
        let (first, inside, flushed, takeover, method, before) = if is_spike(&target.exe) {
            let log = env::temp_dir().join(format!(
                "qe-perf-{}-{}-{}.csv",
                std::process::id(),
                round,
                index
            ));
            let run = run_spike(target, &log)?;
            (
                run.first,
                Some(run.main_to_frame),
                run.flushed,
                run.takeover,
                "present_or_bitblt",
                run.before,
            )
        } else {
            let (child, before) = spawn(target, "")?;
            let shown = wait_window(&child, Duration::from_secs(20))?;
            (shown, None, None, None, "visible_window_poll", before)
        };
        if first < before {
            return Err("QPC order is reversed".into());
        }
        let warmup = round < WARMUP_ROUNDS;
        if !warmup {
            let sample = &mut samples[index];
            sample.first_frame.push((first - before) as u64);
            if let Some(stamp) = flushed {
                sample.dwm_flush.push((stamp - before).max(0) as u64);
            }
            if let Some(stamp) = takeover {
                sample.takeover.push((stamp - before).max(0) as u64);
            }
        }
        println!(
            "{},{},{},{},{:.3},{},{},{},{}",
            csv_field(&target.label()),
            method,
            if warmup { "warmup" } else { "sample" },
            if warmup {
                round + 1
            } else {
                round + 1 - WARMUP_ROUNDS
            },
            ms(first - before),
            inside.map_or(String::new(), |v| format!("{:.3}", ms(v))),
            optional(flushed, before),
            optional(takeover, before),
            cache
        );
    }
    for (target, sample) in targets.iter().zip(&samples) {
        for (label, values) in [
            ("first_frame", &sample.first_frame),
            ("dwm_flush", &sample.dwm_flush),
            ("takeover", &sample.takeover),
        ] {
            for (name, p) in [("median", 50.0), ("p95", 95.0)] {
                if let Some(ticks) = percentile(values, p) {
                    println!(
                        "{},summary,{label}_{name},{},{:.3},,,,",
                        csv_field(&target.label()),
                        values.len(),
                        ticks_to_ms(ticks, hz)
                    );
                }
            }
        }
    }
    Ok(())
}

fn run() -> Result<(), String> {
    match parse_args(&env::args().skip(1).collect::<Vec<_>>())? {
        PerfCommand::Launch {
            targets,
            runs,
            flush_standby,
        } => run_launch(targets, runs, flush_standby),
        PerfCommand::Typing { target, keys } => typing::run_typing(target, keys),
        PerfCommand::DupCheck => typing::run_dup_check(),
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("qe-perf: {e}");
        std::process::exit(1)
    }
}
