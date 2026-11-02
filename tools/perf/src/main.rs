use qe_perf::{
    Command as PerfCommand, Target, csv_field, parse_args, parse_launch_log, percentile,
    quote_windows_arg,
};
use std::{
    env,
    ffi::c_void,
    fs, mem,
    path::Path,
    ptr, thread,
    time::{Duration, Instant},
};
type H = *mut c_void;
#[repr(C)]
struct StartupInfo {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    chars_x: u32,
    chars_y: u32,
    fill: u32,
    flags: u32,
    show: u16,
    reserved2: u16,
    reserved_ptr: *mut u8,
    stdin: H,
    stdout: H,
    stderr: H,
}
#[repr(C)]
struct ProcessInfo {
    process: H,
    thread: H,
    pid: u32,
    tid: u32,
}
#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryPerformanceCounter(t: *mut i64) -> i32;
    fn QueryPerformanceFrequency(t: *mut i64) -> i32;
    fn CreateProcessW(
        app: *const u16,
        cmd: *mut u16,
        pa: H,
        ta: H,
        inherit: i32,
        flags: u32,
        env: H,
        dir: *const u16,
        si: *const StartupInfo,
        pi: *mut ProcessInfo,
    ) -> i32;
    fn CloseHandle(h: H) -> i32;
    fn WaitForSingleObject(h: H, ms: u32) -> u32;
    fn TerminateProcess(h: H, exit: u32) -> i32;
    fn GetLastError() -> u32;
    fn WaitForInputIdle(h: H, ms: u32) -> u32;
}
#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(callback: unsafe extern "system" fn(H, isize) -> i32, param: isize) -> i32;
    fn GetWindowThreadProcessId(h: H, pid: *mut u32) -> u32;
    fn IsWindowVisible(h: H) -> i32;
    fn GetWindowRect(h: H, r: *mut Rect) -> i32;
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSetSystemInformation(class: u32, data: *const u32, len: u32) -> i32;
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
fn qpc() -> i64 {
    let mut n = 0;
    unsafe { QueryPerformanceCounter(&mut n) };
    n
}
fn frequency() -> i64 {
    let mut n = 0;
    unsafe { QueryPerformanceFrequency(&mut n) };
    n
}
struct Child {
    info: ProcessInfo,
}
impl Drop for Child {
    fn drop(&mut self) {
        unsafe {
            if WaitForSingleObject(self.info.process, 0) == 0x102 {
                TerminateProcess(self.info.process, 0);
                WaitForSingleObject(self.info.process, 1000);
            }
            CloseHandle(self.info.thread);
            CloseHandle(self.info.process);
        }
    }
}
fn spawn(target: &Target, extra: &str) -> Result<(Child, i64), String> {
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
        CreateProcessW(
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
            GetLastError()
        }));
    }
    Ok((Child { info: pi }, before))
}
unsafe extern "system" fn find_window(hwnd: H, param: isize) -> i32 {
    let pair = unsafe { &mut *(param as *mut (u32, H)) };
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == pair.0 && unsafe { IsWindowVisible(hwnd) } != 0 {
        let mut rect = Rect::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) } != 0
            && rect.right > rect.left
            && rect.bottom > rect.top
        {
            pair.1 = hwnd;
            return 0;
        }
    }
    1
}
fn visible_window(pid: u32) -> Option<H> {
    let mut pair: (u32, H) = (pid, ptr::null_mut());
    unsafe { EnumWindows(find_window, &mut pair as *mut _ as isize) };
    if pair.1.is_null() { None } else { Some(pair.1) }
}
fn wait_window(child: &Child, timeout: Duration) -> Result<i64, String> {
    unsafe { WaitForInputIdle(child.info.process, 5000) };
    let deadline = Instant::now() + timeout;
    loop {
        if visible_window(child.info.pid).is_some() {
            return Ok(qpc());
        }
        if unsafe { WaitForSingleObject(child.info.process, 0) } == 0 {
            return Err("process exited before a visible window".into());
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for a visible window".into());
        }
        thread::sleep(Duration::from_millis(2))
    }
}
fn parse_log(path: &Path) -> Result<(i64, i64), String> {
    let data = fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_launch_log(&data)
}
fn run_launch(targets: Vec<Target>, runs: usize, flush: bool) -> Result<(), String> {
    let hz = frequency();
    if hz <= 0 {
        return Err("QueryPerformanceFrequency failed".into());
    }
    println!("target,method,phase,run,create_to_frame_ms,main_to_frame_ms");
    let mut values = vec![Vec::<u64>::new(); targets.len()];
    let mut flush_skipped = false;
    for round in 0..runs + 2 {
        for (index, target) in targets.iter().enumerate() {
            if flush && !flush_skipped {
                let purge: u32 = 4;
                let status = unsafe { NtSetSystemInformation(80, &purge, 4) };
                if status < 0 {
                    println!(
                        "SKIPPED standby-list flush: NtSetSystemInformation returned NTSTATUS {status:#010x}; administrator rights are usually required"
                    );
                    flush_skipped = true
                }
            }
            let spike = Path::new(&target.exe)
                .file_stem()
                .and_then(|x| x.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("spike-renderers"));
            let log = env::temp_dir().join(format!(
                "qe-perf-{}-{}-{}.csv",
                std::process::id(),
                round,
                index
            ));
            let extra = if spike {
                format!(
                    "--launch-log {} --exit-after-first-frame",
                    quote_windows_arg(&log.to_string_lossy())
                )
            } else {
                String::new()
            };
            let (child, before) = spawn(target, &extra)?;
            let (after, inside, method) = if spike {
                let deadline = Instant::now() + Duration::from_secs(20);
                loop {
                    if log.exists() {
                        let (start, end) = parse_log(&log)?;
                        let _ = fs::remove_file(&log);
                        break (end, Some(end - start), "present_or_bitblt");
                    }
                    if unsafe { WaitForSingleObject(child.info.process, 0) } == 0 {
                        return Err(format!("{} exited without a launch log", target.exe));
                    }
                    if Instant::now() >= deadline {
                        return Err(format!("{} timed out waiting for launch log", target.exe));
                    }
                    thread::sleep(Duration::from_millis(2))
                }
            } else {
                (
                    wait_window(&child, Duration::from_secs(20))?,
                    None,
                    "visible_window_poll",
                )
            };
            if after < before {
                return Err("QPC order is reversed".into());
            }
            let ticks = (after - before) as u64;
            let ms = ticks as f64 * 1000.0 / hz as f64;
            let phase = if round < 2 { "warmup" } else { "sample" };
            if round >= 2 {
                values[index].push(ticks)
            }
            let run_number = if round < 2 { round + 1 } else { round - 1 };
            println!(
                "{},{},{},{},{:.3},{}",
                csv_field(&target.exe),
                method,
                phase,
                run_number,
                ms,
                inside
                    .map(|v| format!("{:.3}", v as f64 * 1000.0 / hz as f64))
                    .unwrap_or_default()
            );
            drop(child)
        }
    }
    for (index, target) in targets.iter().enumerate() {
        for (label, p) in [("median", 50.0), ("p95", 95.0)] {
            let ticks = percentile(&values[index], p).ok_or("no samples")?;
            println!(
                "{},summary,{},{},{:.3},",
                csv_field(&target.exe),
                label,
                runs,
                ticks as f64 * 1000.0 / hz as f64
            )
        }
    }
    Ok(())
}
fn run() -> Result<(), String> {
    match parse_args(&env::args().skip(1).collect::<Vec<_>>())?{PerfCommand::Launch{targets,runs,flush_standby}=>run_launch(targets,runs,flush_standby),PerfCommand::Typing{target:_,keys:_}=>Err("typing measurement is unavailable: DXGI Desktop Duplication is not implemented; no latency is reported".into())}
}
fn main() {
    if let Err(e) = run() {
        eprintln!("qe-perf: {e}");
        std::process::exit(1)
    }
}
