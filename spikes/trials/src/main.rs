#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(not(windows))]
fn main() {
    eprintln!("These trials need Windows.");
}

#[cfg(windows)]
mod caption;
#[cfg(windows)]
mod tsf;
#[cfg(windows)]
mod uia;
#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() {
    // Per-monitor V2 DPI awareness keeps hit tests and caret coordinates in physical pixels.
    unsafe {
        if win::SetProcessDpiAwarenessContext(-4) == 0 {
            let error = win::GetLastError();
            if win::AreDpiAwarenessContextsEqual(win::GetThreadDpiAwarenessContext(), -4) == 0 {
                let previous = win::SetThreadDpiAwarenessContext(-4);
                if previous == 0
                    || win::AreDpiAwarenessContextsEqual(win::GetThreadDpiAwarenessContext(), -4)
                        == 0
                {
                    eprintln!(
                        "Per-monitor V2 DPI awareness unavailable: process error={error}, thread error={}",
                        win::GetLastError()
                    );
                    std::process::exit(1);
                }
            }
        }
    }
    let args: Vec<String> = std::env::args().collect();
    let trial = args
        .windows(2)
        .find(|a| a[0] == "--trial")
        .map(|a| a[1].as_str());
    let result = match trial {
        Some("caption") => caption::run(),
        Some("tsf") => {
            let log = args
                .windows(2)
                .find(|a| a[0] == "--log")
                .map(|a| a[1].as_str());
            tsf::run(log)
        }
        Some("uia") => uia::run(args.iter().any(|a| a == "--self-check")),
        _ => Err("usage: spike-trials --trial caption|tsf|uia [--log FILE] [--self-check]".into()),
    };
    if let Err(error) = result {
        eprintln!("FAIL: {error}");
        std::process::exit(1);
    }
}
