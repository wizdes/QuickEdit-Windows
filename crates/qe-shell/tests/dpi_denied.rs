//! This file runs in its own process, because it sets the process's DPI awareness first.
//! Windows then refuses the window's own request, as it does in some accounts.
use qe_render::BitmapRenderer;
use qe_shell::{ShellError, ShellWindow};
use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    DPI_AWARENESS_CONTEXT_UNAWARE, GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
};

mod support;

#[test]
fn a_denied_process_setting_still_gives_an_aware_window() {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE) }.unwrap();
    assert!(
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
            .is_err(),
        "Windows refuses a second setting"
    );
    let window = ShellWindow::create(
        Box::new(BitmapRenderer::new(64, 64).unwrap()),
        Box::new(support::RealMeasurer),
        8.0,
        17.0,
    )
    .unwrap();
    let aware = unsafe {
        AreDpiAwarenessContextsEqual(
            GetThreadDpiAwarenessContext(),
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
    };
    assert!(aware.as_bool(), "this thread's window is aware");
    let problems = window.problems().unwrap();
    assert!(
        !problems.iter().any(|p| matches!(
            p,
            ShellError::Win32 {
                call: "SetProcessDpiAwarenessContext",
                ..
            }
        )),
        "{problems:?}"
    );
}
