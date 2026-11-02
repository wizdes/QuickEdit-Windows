//! Hidden windows only. Every test ends by checking that its window never showed.
use qe_api::{
    Bitmap, Color, CommandId, Dip, DisplayList, DrawCommand, InputKind, Modifiers, MouseButton,
    MouseCursor, PixelRect, Rect, RenderError, Renderer, Theme, ThemeKind, View, ViewRequest,
};
use qe_render::BitmapRenderer;
use qe_shell::{
    AutoHideEdges, CaptionHit, ContrastColors, FrameMode, KeyBinding, KeyTable, ShellAction,
    ShellError, ShellEvent, ShellWindow, Tab, TabStrip, TestFault, VIEW_TIMER, WindowConfig,
    apply_autohide, color_from_colorref, theme_from_status, top_border_height,
};
use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::rc::Rc;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, SetKeyboardState, TME_LEAVE, TME_NONCLIENT, TME_QUERY, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_CONTROL, VK_F4, VK_F10, VK_F11, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

mod support;
use support::*;

/// The real renderer with a log of each call, so that tests can check the order.
#[derive(Clone)]
struct Logged {
    inner: Rc<RefCell<BitmapRenderer>>,
    log: Rc<RefCell<Vec<String>>>,
    fail_draw: bool,
}
impl Logged {
    fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(BitmapRenderer::new(64, 64).unwrap())),
            log: Rc::default(),
            fail_draw: false,
        }
    }
    fn took(&self) -> Vec<String> {
        std::mem::take(&mut self.log.borrow_mut())
    }
    fn bitmap(&self) -> Bitmap {
        self.inner.borrow().bitmap().unwrap()
    }
}
impl Renderer for Logged {
    fn begin_frame(&mut self) -> Result<(), RenderError> {
        self.log.borrow_mut().push("begin".into());
        self.inner.borrow_mut().begin_frame()
    }
    fn draw(&mut self, list: &DisplayList) -> Result<(), RenderError> {
        self.log.borrow_mut().push("draw".into());
        if self.fail_draw {
            return Err(RenderError::new("broken on purpose"));
        }
        Renderer::draw(&mut *self.inner.borrow_mut(), list)
    }
    fn end_frame(&mut self) -> Result<(), RenderError> {
        self.log.borrow_mut().push("end".into());
        self.inner.borrow_mut().end_frame()
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        self.log
            .borrow_mut()
            .push(format!("resize {width}x{height}"));
        self.inner.borrow_mut().resize(width, height)
    }
    fn set_dpi(&mut self, dpi: u32) -> Result<(), RenderError> {
        self.log.borrow_mut().push(format!("dpi {dpi}"));
        self.inner.borrow_mut().set_dpi(dpi)
    }
    fn render_to_bitmap(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Bitmap, RenderError> {
        self.log.borrow_mut().push("render_to_bitmap".into());
        self.inner
            .borrow_mut()
            .render_to_bitmap(list, width, height)
    }
    fn invalidate(&mut self, rect: Rect) {
        self.log.borrow_mut().push("invalidate".into());
        Renderer::invalidate(&mut *self.inner.borrow_mut(), rect);
    }
    fn invalidate_all(&mut self) {
        self.log.borrow_mut().push("invalidate_all".into());
        Renderer::invalidate_all(&mut *self.inner.borrow_mut());
    }
    fn present_rect(&mut self, dc: *mut c_void, rect: PixelRect) -> Result<(), RenderError> {
        self.log.borrow_mut().push("present_rect".into());
        Renderer::present_rect(&mut *self.inner.borrow_mut(), dc, rect)
    }
}

struct Fixture {
    window: ShellWindow,
    renderer: Logged,
    scale: f32,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(hwnd) = self.window.hwnd() {
            assert!(
                !unsafe { IsWindowVisible(HWND(hwnd)) }.as_bool(),
                "it showed"
            );
        }
    }
}
fn fixture_with(dpi: Option<u32>, renderer: Logged) -> Fixture {
    let window = ShellWindow::create_with(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            fake_dpi: dpi,
            record_system_commands: true,
            fault: None,
        },
        Box::new(renderer.clone()),
        Box::new(RealMeasurer),
    )
    .unwrap();
    // A new window reports its first size. Tests start from an empty queue.
    window.take_events().unwrap();
    Fixture {
        window,
        renderer,
        scale: dpi.unwrap_or(96) as f32 / 96.0,
    }
}
fn fixture(dpi: Option<u32>) -> Fixture {
    fixture_with(dpi, Logged::new())
}
/// Small enough at every test DPI to fit a 1024 x 768 desktop, which limits window size.
fn small_fixture(dpi: u32, renderer: Logged) -> Fixture {
    let window = ShellWindow::create_with(
        WindowConfig {
            cell_width: 2.0,
            line_height: 5.0,
            fake_dpi: Some(dpi),
            record_system_commands: true,
            fault: None,
        },
        Box::new(renderer.clone()),
        Box::new(RealMeasurer),
    )
    .unwrap();
    window.take_events().unwrap();
    Fixture {
        window,
        renderer,
        scale: dpi as f32 / 96.0,
    }
}
impl Fixture {
    fn hwnd(&self) -> HWND {
        HWND(self.window.hwnd().unwrap())
    }
    fn send(&self, msg: u32, w: usize, l: isize) -> isize {
        unsafe { SendMessageW(self.hwnd(), msg, Some(WPARAM(w)), Some(LPARAM(l))) }.0
    }
    fn client(&self) -> RECT {
        let mut b = RECT::default();
        unsafe { GetClientRect(self.hwnd(), &mut b) }.unwrap();
        b
    }
    fn outer(&self) -> RECT {
        let mut b = RECT::default();
        unsafe { GetWindowRect(self.hwnd(), &mut b) }.unwrap();
        b
    }
    fn screen(&self, x: i32, y: i32) -> isize {
        let mut at = POINT { x, y };
        let _ = unsafe { ClientToScreen(self.hwnd(), &mut at) };
        pack(at.x, at.y)
    }
    /// `WM_NCHITTEST` at a client point in device pixels.
    fn hit_px(&self, x: i32, y: i32) -> u32 {
        self.send(WM_NCHITTEST, 0, self.screen(x, y)) as u32
    }
    fn hit(&self, x: f32, y: f32) -> u32 {
        self.hit_px((x * self.scale) as i32, (y * self.scale) as i32)
    }
    fn width_dips(&self) -> f32 {
        self.client().right as f32 / self.scale
    }
    fn at(&self, x: f32, y: f32) -> isize {
        pack((x * self.scale) as i32, (y * self.scale) as i32)
    }
    fn screen_at(&self, x: f32, y: f32) -> isize {
        self.screen((x * self.scale) as i32, (y * self.scale) as i32)
    }
    /// The events a test is about. A desktop can add others: a DWM problem with no
    /// composition, and focus that moves when `SetFocus` runs.
    fn events(&self) -> Vec<ShellEvent> {
        self.window
            .take_events()
            .unwrap()
            .into_iter()
            .filter(|e| {
                !matches!(e, ShellEvent::Problem(_))
                    && !matches!(e, ShellEvent::ContentInput(input)
                        if matches!(input.kind, InputKind::Focus | InputKind::Blur))
            })
            .collect()
    }
    fn commands(&self) -> Vec<u32> {
        self.window.system_commands().unwrap()
    }
    fn renderer_problems(&self) -> Vec<ShellError> {
        self.window
            .problems()
            .unwrap()
            .into_iter()
            .filter(|p| {
                matches!(
                    p,
                    ShellError::Renderer { .. } | ShellError::Reentered { .. }
                )
            })
            .collect()
    }
    /// A list of what the row draws now, to compare states.
    fn row_list(&self) -> DisplayList {
        self.window
            .with_strip(|strip: &mut TabStrip| {
                let mut list = DisplayList::default();
                strip.draw(&mut list, &Theme::new(ThemeKind::Light), &RealMeasurer);
                list
            })
            .unwrap()
    }
}
fn pack(x: i32, y: i32) -> isize {
    ((y as i16 as u16 as u32) << 16 | (x as i16 as u16 as u32)) as i32 as isize
}

#[test]
fn create_removes_the_standard_title_bar_at_once() {
    let f = fixture(None);
    let (outer, client) = (f.outer(), f.client());
    println!(
        "hidden window, measured: outer {}x{}, client {}x{}",
        outer.right - outer.left,
        outer.bottom - outer.top,
        client.right,
        client.bottom
    );
    let mut origin = POINT { x: 0, y: 0 };
    let _ = unsafe { ClientToScreen(f.hwnd(), &mut origin) };
    assert_eq!(
        origin.y, outer.top,
        "the client area starts at the window's top"
    );
    let frame = origin.x - outer.left;
    assert!(frame > 0, "the side frame stays outside the client area");
    assert_eq!(client.right, outer.right - outer.left - 2 * frame);
    assert_eq!(client.bottom, outer.bottom - outer.top - frame);
}

#[test]
fn window_size_comes_from_the_dpi_before_the_first_draw() {
    let f = small_fixture(144, Logged::new());
    let log = f.renderer.took();
    assert_eq!(log[0], "dpi 144", "{log:?}");
    assert!(!log.iter().any(|l| l == "draw"), "no draw yet: {log:?}");
    assert_eq!(f.window.dpi().unwrap(), 144);
    let (columns, rows, row): (f32, f32, f32) = (90.0, 30.0, 40.0);
    let width = (columns * 2.0 + 138.0).max(500.0) * 1.5;
    let height = (rows * 5.0 + row) * 1.5;
    assert_eq!(f.client().right, width.ceil() as i32);
    assert_eq!(f.client().bottom, height.ceil() as i32);
    assert!(log.contains(&format!(
        "resize {}x{}",
        f.client().right,
        f.client().bottom
    )));
    f.send(WM_PAINT, 0, 0);
    let log = f.renderer.took();
    assert_eq!(log[..3], ["begin", "draw", "end"], "{log:?}");
    let image = f.renderer.bitmap();
    assert_eq!(image.width, f.client().right as u32);
    // The 40-dip row ends at 60 pixels, not at 40.
    // Dips 320 is the empty part of the row in this 500-wide window.
    let x = (320.0 * 1.5) as u32;
    assert_ne!(pixel(&image, x, 50), pixel(&image, x, 70));
    assert_eq!(pixel(&image, x, 62), pixel(&image, x, 200));
}

#[test]
fn real_renderer_paints_a_window_that_is_not_blank() {
    let f = fixture(None);
    f.send(WM_PAINT, 0, 0);
    assert_eq!(f.renderer_problems(), vec![]);
    let image = f.renderer.bitmap();
    let bg = pixel(&image, 300, 2);
    let ink = ink_box(&image, (0, 0, image.width, 40), bg, 40);
    assert!(ink.is_some(), "the row has ink");
    let text = pixel(&image, 300, 300);
    assert_ne!(bg, text, "the text area is not the row color");
    assert_eq!(
        pixel(&image, 300, 40),
        bg,
        "the row starts 1 pixel down, so 40 is in it"
    );
    assert_eq!(pixel(&image, 300, 41), text, "the row stops at 41");
    let log = f.renderer.took();
    assert!(!log.iter().any(|l| l == "render_to_bitmap"), "{log:?}");
}

#[test]
fn renderer_failure_is_named_and_the_frame_still_ends_and_presents() {
    let mut broken = Logged::new();
    broken.fail_draw = true;
    let f = fixture_with(None, broken);
    f.renderer.took();
    f.send(WM_PAINT, 0, 0);
    assert_eq!(f.renderer.took(), ["begin", "draw", "end", "present_rect"]);
    let expected = ShellError::Renderer {
        stage: "draw",
        message: "broken on purpose".into(),
    };
    assert_eq!(f.renderer_problems(), vec![expected.clone()]);
    assert!(
        f.window
            .take_events()
            .unwrap()
            .contains(&ShellEvent::Problem(expected))
    );
}

#[test]
fn the_old_font_token_is_an_error_in_the_real_renderer() {
    let mut list = DisplayList::default();
    list.push(qe_api::DrawCommand::TextRun {
        at: qe_api::Point {
            x: Dip(0.0),
            y: Dip(10.0),
        },
        text: "x".into(),
        font: "Segoe Fluent Icons;10px".into(),
        font_size: Dip(10.0),
        color: qe_api::Color::rgb(0, 0, 0),
    });
    let mut real = BitmapRenderer::new(32, 32).unwrap();
    real.begin_frame().unwrap();
    assert!(Renderer::draw(&mut real, &list).is_err());
}

#[test]
fn nchittest_gives_the_right_code_at_each_dpi() {
    for dpi in [96, 144, 192] {
        let f = fixture(Some(dpi));
        let w = f.width_dips();
        assert_eq!(f.hit(w - 7.0, 20.0), HTCLOSE, "{dpi} close at 7 from right");
        // The top border's row resizes, and the buttons start on the row below it.
        assert_eq!(
            f.hit_px((w - 23.0) as i32 * dpi as i32 / 96, 0),
            HTTOP,
            "{dpi}"
        );
        assert_eq!(
            f.hit_px((f.client().right as f32 - 23.0 * f.scale) as i32, 1),
            HTCLOSE,
            "{dpi} close from its first row"
        );
        assert_eq!(
            f.hit_px((f.client().right as f32 - 69.0 * f.scale) as i32, 1),
            HTMAXBUTTON,
            "{dpi} all of maximize"
        );
        assert_eq!(f.hit(w - 69.0, 39.0), HTMAXBUTTON, "{dpi}");
        assert_eq!(f.hit(w - 115.0, 20.0), HTMINBUTTON, "{dpi}");
        assert_eq!(f.hit(7.0, 20.0), HTCLIENT, "{dpi} tab 0 at 7 from left");
        assert_eq!(f.hit(w - 200.0, 2.0), HTTOP, "{dpi} top band");
        assert_eq!(f.hit(w - 200.0, 20.0), HTCAPTION, "{dpi} empty row");
        assert_eq!(f.hit(w / 2.0, 300.0), HTCLIENT, "{dpi} text area");
        let client = f.client();
        assert_eq!(f.hit_px(0, 300), HTCLIENT, "{dpi} left text column");
        assert_eq!(f.hit_px(client.right - 1, 300), HTCLIENT, "{dpi}");
        assert_eq!(f.hit_px(300, client.bottom - 1), HTCLIENT, "{dpi}");
        // The frame outside the client area still resizes.
        assert_eq!(f.hit_px(-2, 300), HTLEFT, "{dpi}");
        assert_eq!(f.hit_px(client.right + 2, 300), HTRIGHT, "{dpi}");
        assert_eq!(f.hit_px(300, client.bottom + 2), HTBOTTOM, "{dpi}");
    }
}

#[test]
fn dpi_change_uses_the_suggested_rectangle_and_invalidates() {
    let f = fixture(Some(96));
    f.renderer.took();
    // It fits a 1024 x 768 desktop, and is over the minimum size at 144 DPI.
    let suggested = RECT {
        left: 40,
        top: 50,
        right: 840,
        bottom: 550,
    };
    let dpi = 144usize;
    f.send(
        WM_DPICHANGED,
        dpi | (dpi << 16),
        &suggested as *const RECT as isize,
    );
    let outer = f.outer();
    assert_eq!(
        (outer.left, outer.top, outer.right, outer.bottom),
        (40, 50, 840, 550)
    );
    assert_eq!(f.window.dpi().unwrap(), 144);
    let log = f.renderer.took();
    assert!(log.contains(&"dpi 144".to_string()), "{log:?}");
    assert!(log.contains(&"invalidate_all".to_string()), "{log:?}");
    // Hit tests follow the new scale.
    let client = f.client();
    assert_eq!(
        f.hit_px(client.right - (47.0 * 1.5) as i32, 20),
        HTMAXBUTTON
    );
    assert_eq!(f.hit_px(client.right - (45.0 * 1.5) as i32, 20), HTCLOSE);
    f.send(WM_PAINT, 0, 0);
    assert!(f.renderer.took().contains(&"draw".to_string()));
}

#[test]
fn hover_clears_on_leave_messages() {
    let f = fixture(None);
    let w = f.width_dips();
    let rest = f.row_list();
    f.send(
        WM_NCMOUSEMOVE,
        HTMAXBUTTON as usize,
        f.screen_at(w - 69.0, 20.0),
    );
    assert_ne!(f.row_list(), rest, "non-client move uses the same hit test");
    f.send(WM_NCMOUSELEAVE, 0, 0);
    assert_eq!(f.row_list(), rest);
    let plus = f.window.with_strip(|s| s.split_rect().x.0 + 5.0).unwrap();
    f.send(WM_MOUSEMOVE, 0, f.at(plus, 20.0));
    assert_ne!(f.row_list(), rest);
    f.send(WM_MOUSELEAVE, 0, 0);
    assert_eq!(f.row_list(), rest);
}

#[test]
fn caption_press_acts_on_release_and_cancels_on_lost_capture() {
    let f = fixture(None);
    let w = f.width_dips();
    let close = f.screen_at(w - 23.0, 20.0);
    f.send(WM_NCLBUTTONDOWN, HTCLOSE as usize, close);
    assert_eq!(f.commands(), Vec::<u32>::new(), "not on press");
    f.send(WM_LBUTTONUP, 0, f.at(w - 23.0, 20.0));
    assert_eq!(f.commands(), vec![SC_CLOSE]);

    // Release elsewhere does nothing.
    f.send(WM_NCLBUTTONDOWN, HTCLOSE as usize, close);
    f.send(WM_MOUSEMOVE, 0, f.at(100.0, 300.0));
    f.send(WM_LBUTTONUP, 0, f.at(100.0, 300.0));
    assert_eq!(f.commands(), vec![SC_CLOSE]);

    // Another window takes capture: the press ends.
    f.send(
        WM_NCLBUTTONDOWN,
        HTMINBUTTON as usize,
        f.screen_at(w - 115.0, 20.0),
    );
    f.send(WM_CAPTURECHANGED, 0, 0);
    f.send(WM_LBUTTONUP, 0, f.at(w - 115.0, 20.0));
    assert_eq!(f.commands(), vec![SC_CLOSE]);

    // A fast second click arrives as a double click, and it still works.
    let max = f.screen_at(w - 69.0, 20.0);
    f.send(WM_NCLBUTTONDOWN, HTMAXBUTTON as usize, max);
    f.send(WM_NCLBUTTONUP, HTMAXBUTTON as usize, max);
    f.send(WM_NCLBUTTONDBLCLK, HTMAXBUTTON as usize, max);
    f.send(WM_NCLBUTTONUP, HTMAXBUTTON as usize, max);
    assert_eq!(f.commands(), vec![SC_CLOSE, SC_MAXIMIZE, SC_MAXIMIZE]);
}

#[test]
fn blur_ends_a_press_and_hover() {
    let f = fixture(None);
    f.send(WM_MOUSEMOVE, 0, f.at(100.0, 20.0));
    f.send(WM_LBUTTONDOWN, 0, f.at(100.0, 20.0));
    f.send(WM_KILLFOCUS, 0, 0);
    f.send(WM_MOUSEMOVE, 0, f.at(500.0, 200.0));
    f.send(WM_LBUTTONUP, 0, f.at(100.0, 20.0));
    assert!(
        !f.events()
            .iter()
            .any(|e| matches!(e, ShellEvent::SelectTab(_))),
        "the press was cancelled"
    );
}

#[test]
fn close_is_a_request_and_the_owner_decides() {
    let f = fixture(None);
    assert_eq!(f.send(WM_CLOSE, 0, 0), 0);
    assert!(unsafe { IsWindow(Some(f.hwnd())) }.as_bool(), "still here");
    assert!(f.events().contains(&ShellEvent::CloseWindowRequested));

    f.window.add_tab(Tab::new("Second")).unwrap();
    f.events();
    let (b, first) = f
        .window
        .with_strip(|s| (s.tab_rects()[0], s.tab_id(0).unwrap()))
        .unwrap();
    let x = b.x.0 + b.width.0 - 5.0;
    f.send(WM_LBUTTONDOWN, 0, f.at(x, 20.0));
    f.send(WM_LBUTTONUP, 0, f.at(x, 20.0));
    assert!(f.events().contains(&ShellEvent::CloseTabRequested(first)));
    assert_eq!(f.window.with_strip(|s| s.tabs().len()).unwrap(), 2);
    f.window.close_tab(first).unwrap();
    assert_eq!(f.window.with_strip(|s| s.tabs().len()).unwrap(), 1);
    f.window.close_tab(first).unwrap();
    assert_eq!(
        f.window.with_strip(|s| s.tabs()[0].title.clone()).unwrap(),
        "Second",
        "a late answer changes nothing"
    );
}

#[test]
fn many_windows_share_one_thread_and_nothing_quits_on_its_own() {
    let a = fixture(None);
    let b = fixture(None);
    let hwnd_b = b.hwnd();
    a.window.destroy().unwrap();
    assert_eq!(a.window.hwnd().unwrap_err(), ShellError::WindowGone);
    assert!(unsafe { IsWindow(Some(hwnd_b)) }.as_bool());
    let mut msg = MSG::default();
    // A quit comes only after the posted messages, so that this reads all of them.
    while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
        assert_ne!(msg.message, WM_QUIT, "destroying a window posts no quit");
    }
    qe_shell::quit_message_loop(7);
    assert_eq!(qe_shell::run_message_loop().unwrap(), 7);
    b.send(WM_NCHITTEST, 0, 0);
}
#[test]
fn owner_hears_about_destruction() {
    let f = fixture(None);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    f.window
        .set_event_handler(move |_, event| log.borrow_mut().push(event));
    f.window.destroy().unwrap();
    assert!(seen.borrow().contains(&ShellEvent::WindowDestroyed));
}

#[test]
fn handler_can_send_messages_and_change_the_strip() {
    let f = fixture(None);
    let seen = Rc::new(RefCell::new(0));
    let hits = seen.clone();
    let point = f.screen_at(100.0, 20.0);
    f.window.set_event_handler(move |handle, event| {
        if event == ShellEvent::NewTab {
            // A dialog's nested message loop would send messages like this.
            let hwnd = HWND(handle.hwnd().unwrap());
            let code = unsafe { SendMessageW(hwnd, WM_NCHITTEST, None, Some(LPARAM(point))) };
            *hits.borrow_mut() = code.0;
            unsafe { SendMessageW(hwnd, WM_MOUSEMOVE, None, None) };
            unsafe { SendMessageW(hwnd, WM_PAINT, None, None) };
            handle.add_tab(Tab::new("From the handler")).unwrap();
        }
    });
    let x = f.window.with_strip(|s| s.split_rect().x.0 + 5.0).unwrap();
    f.send(WM_LBUTTONDOWN, 0, f.at(x, 20.0));
    f.send(WM_LBUTTONUP, 0, f.at(x, 20.0));
    assert_eq!(f.window.with_strip(|s| s.tabs().len()).unwrap(), 2);
    assert_ne!(*seen.borrow(), 0, "the nested message was answered");
    assert_eq!(f.renderer_problems(), vec![]);
}

#[test]
fn a_message_inside_with_strip_is_named_not_undefined() {
    let f = fixture(None);
    let hwnd = f.hwnd();
    let point = f.screen_at(100.0, 20.0);
    f.window
        .with_strip(|_| unsafe {
            SendMessageW(hwnd, WM_NCHITTEST, None, Some(LPARAM(point)));
        })
        .unwrap();
    assert!(
        f.window
            .problems()
            .unwrap()
            .contains(&ShellError::Reentered {
                message: WM_NCHITTEST
            })
    );
}

fn full_screen_key(f: &Fixture, repeat: bool) {
    let l = if repeat { 1isize << 30 } else { 0 };
    f.send(WM_KEYDOWN, usize::from(VK_F11.0), l);
}
fn set_maximized_style(f: &Fixture) {
    let hwnd = f.hwnd();
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(hwnd, GWL_STYLE, style | WS_MAXIMIZE.0 as isize);
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        )
        .unwrap();
    }
}

#[test]
fn maximized_client_area_drops_the_top_band_and_insets_the_top() {
    let f = fixture(None);
    let (outer_before, client_before) = (f.outer(), f.client());
    set_maximized_style(&f);
    f.send(WM_SIZE, 0, 0);
    assert_eq!(f.window.frame_mode().unwrap(), FrameMode::Maximized);
    let inset = unsafe {
        GetSystemMetricsForDpi(SM_CYSIZEFRAME, 96) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, 96)
    };
    let mut origin = POINT { x: 0, y: 0 };
    let _ = unsafe { ClientToScreen(f.hwnd(), &mut origin) };
    assert_eq!(origin.y, f.outer().top + inset);
    assert_eq!(f.outer().top, outer_before.top);
    assert!(f.client().bottom < client_before.bottom);
    // The very top of the client area is a tab or a drag area, never a resize zone.
    assert_eq!(f.hit(5.0, 0.0), HTCLIENT);
    assert_eq!(f.hit(f.width_dips() - 200.0, 0.0), HTCAPTION);
    assert_eq!(f.hit(f.width_dips() - 23.0, 0.0), HTCLOSE);
}

#[test]
fn full_screen_from_maximized_uses_one_state_everywhere() {
    let f = fixture(None);
    set_maximized_style(&f);
    f.send(WM_SIZE, 0, 0);
    full_screen_key(&f, false);
    assert_eq!(f.window.frame_mode().unwrap(), FrameMode::FullScreen);
    let style = unsafe { GetWindowLongPtrW(f.hwnd(), GWL_STYLE) };
    assert_eq!(style & WS_MAXIMIZE.0 as isize, 0, "no WS_MAXIMIZE");
    assert_ne!(style & WS_POPUP.0 as isize, 0);
    assert!(!unsafe { IsZoomed(f.hwnd()) }.as_bool());
    // The row is 32 tall in drawing and in hit tests, so y=36 cannot close the window.
    let w = f.width_dips();
    assert_eq!(f.hit(w - 10.0, 36.0), HTCLIENT);
    assert_eq!(f.hit(w - 10.0, 20.0), HTCLOSE);
    assert_eq!(
        f.hit(w - 200.0, 20.0),
        HTCLIENT,
        "full screen does not drag"
    );
    // Holding F11 changes nothing.
    full_screen_key(&f, true);
    assert_eq!(f.window.frame_mode().unwrap(), FrameMode::FullScreen);
    // The second press goes back to maximized.
    full_screen_key(&f, false);
    assert_eq!(f.window.frame_mode().unwrap(), FrameMode::Maximized);
    assert!(unsafe { IsZoomed(f.hwnd()) }.as_bool());
    let style = unsafe { GetWindowLongPtrW(f.hwnd(), GWL_STYLE) };
    assert_eq!(style & WS_POPUP.0 as isize, 0);
}

#[test]
fn full_screen_returns_to_the_restored_rectangle() {
    let f = fixture(None);
    let before = f.outer();
    full_screen_key(&f, false);
    assert_ne!(
        (f.outer().right - f.outer().left, f.outer().bottom),
        (before.right - before.left, before.bottom),
        "full screen covers the monitor"
    );
    full_screen_key(&f, false);
    let after = f.outer();
    assert_eq!(
        (after.left, after.top, after.right, after.bottom),
        (before.left, before.top, before.right, before.bottom)
    );
    assert_eq!(f.window.frame_mode().unwrap(), FrameMode::Restored);
}

#[test]
fn maximize_in_full_screen_leaves_full_screen_first() {
    let f = fixture(None);
    full_screen_key(&f, false);
    let w = f.width_dips();
    let max = f.screen_at(w - 69.0, 20.0);
    assert_eq!(f.hit(w - 69.0, 20.0), HTMAXBUTTON);
    f.send(WM_NCLBUTTONDOWN, HTMAXBUTTON as usize, max);
    f.send(WM_LBUTTONUP, 0, f.at(w - 69.0, 20.0));
    assert_eq!(f.window.frame_mode().unwrap(), FrameMode::Restored);
    assert_eq!(f.commands(), vec![SC_MAXIMIZE]);
    let style = unsafe { GetWindowLongPtrW(f.hwnd(), GWL_STYLE) };
    assert_eq!(style & WS_POPUP.0 as isize, 0);
}

#[test]
fn alt_alone_opens_the_list_through_the_window_and_skips_the_menu_loop() {
    let f = fixture(None);
    let alt = usize::from(VK_MENU.0);
    assert_eq!(f.send(WM_SYSKEYDOWN, alt, 1 << 29), 0);
    assert!(
        !f.events()
            .iter()
            .any(|e| matches!(e, ShellEvent::OpenCaretList(_)))
    );
    assert_eq!(f.send(WM_SYSKEYUP, alt, 1 << 29), 0);
    assert!(
        f.events()
            .iter()
            .any(|e| matches!(e, ShellEvent::OpenCaretList(_)))
    );
    // A repeat of the press does not arm a release.
    f.send(WM_SYSKEYDOWN, alt, 1 << 29);
    f.send(WM_KILLFOCUS, 0, 0);
    f.send(WM_SYSKEYUP, alt, 1 << 29);
    assert!(
        !f.events()
            .iter()
            .any(|e| matches!(e, ShellEvent::OpenCaretList(_)))
    );
}

#[test]
fn keys_and_wheel_go_to_the_content_unless_the_strip_takes_them() {
    let f = fixture(None);
    for _ in 0..12 {
        f.window.add_tab(Tab::new("More")).unwrap();
    }
    f.events();
    assert_eq!(f.send(WM_KEYDOWN, 0x09, 0), 0);
    assert!(matches!(
        f.events().as_slice(),
        [ShellEvent::ContentInput(_)]
    ));
    f.window.focus_strip().unwrap();
    f.send(WM_KEYDOWN, 0x27, 0);
    let keys = content_events(&f);
    assert!(
        !keys.iter().any(|k| matches!(k, InputKind::KeyDown { .. })),
        "the strip has focus and takes it: {keys:?}"
    );
    f.send(WM_KEYDOWN, 0x1b, 0);
    assert!(f.events().contains(&ShellEvent::FocusContent));

    let w = f.window.with_strip(|s| s.tab_rects()[0].x.0).unwrap();
    let notch_down = (-120i16 as u16 as usize) << 16;
    f.send(WM_MOUSEWHEEL, notch_down, f.screen_at(200.0, 400.0));
    assert!(matches!(
        f.events().as_slice(),
        [ShellEvent::ContentInput(_)]
    ));
    assert_eq!(f.window.with_strip(|s| s.tab_rects()[0].x.0).unwrap(), w);
    if f.window.with_strip(|s| s.overflow()).unwrap() {
        f.window
            .with_strip(|s| s.select(s.tab_id(0).unwrap()))
            .unwrap();
        f.events();
        let start = f.window.with_strip(|s| s.tab_rects()[0].x.0).unwrap();
        f.send(WM_MOUSEWHEEL, notch_down, f.screen_at(200.0, 20.0));
        assert!(f.window.with_strip(|s| s.tab_rects()[0].x.0).unwrap() < start);
    }
    assert!(
        f.window.with_strip(|s| s.overflow()).unwrap(),
        "the test used overflow"
    );
}

#[test]
fn set_focus_request_reaches_the_owner_for_the_content() {
    let f = fixture(None);
    f.window
        .with_strip(|s| s.set_focused(Some(CaptionHit::Tab(0))))
        .unwrap();
    f.send(WM_KEYDOWN, 0x1b, 0);
    assert_eq!(f.events(), vec![ShellEvent::FocusContent]);
    assert_eq!(f.window.with_strip(|s| s.focused()).unwrap(), None);
}

#[test]
fn paint_does_only_what_changed() {
    let f = fixture(None);
    f.send(WM_PAINT, 0, 0);
    f.renderer.took();

    // Nothing pending: no frame, only the copy that Windows asked for.
    f.send(WM_PAINT, 0, 0);
    assert_eq!(f.renderer.took(), ["present_rect"]);

    // A first hover asks for small rectangles and no layout-wide repaint.
    f.send(WM_MOUSEMOVE, 0, f.at(100.0, 20.0));
    let log = f.renderer.took();
    assert!(log.iter().all(|l| l == "invalidate"), "{log:?}");
    assert!(!log.is_empty());

    // The same target again asks for nothing.
    f.send(WM_MOUSEMOVE, 0, f.at(101.0, 21.0));
    assert_eq!(f.renderer.took(), Vec::<String>::new());

    // One paint is one frame with one draw.
    f.send(WM_PAINT, 0, 0);
    assert_eq!(f.renderer.took(), ["begin", "draw", "end", "present_rect"]);
}

#[test]
fn a_zero_size_never_reaches_the_renderer() {
    let f = fixture(None);
    f.renderer.took();
    f.send(WM_SIZE, SIZE_MINIMIZED as usize, 0);
    assert_eq!(f.renderer.took(), Vec::<String>::new());
}

#[test]
fn registry_status_two_means_light_and_others_are_problems() {
    assert_eq!(theme_from_status(0, 0), Ok(true));
    assert_eq!(theme_from_status(0, 1), Ok(false));
    assert_eq!(theme_from_status(2, 1), Ok(false));
    assert_eq!(
        theme_from_status(5, 1),
        Err(ShellError::ThemeRead { status: 5 })
    );
}

#[test]
fn second_window_reports_no_awareness_problem() {
    let a = fixture(None);
    let b = fixture(None);
    for f in [&a, &b] {
        let problems = f.window.problems().unwrap();
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
}

#[test]
fn autohide_taskbar_keeps_two_pixels_free_on_its_edge() {
    let b = RECT {
        left: 0,
        top: 0,
        right: 100,
        bottom: 100,
    };
    let inset = apply_autohide(
        b,
        AutoHideEdges {
            bottom: true,
            ..Default::default()
        },
    );
    assert_eq!((inset.bottom, inset.top, inset.right), (98, 0, 100));
    let all = apply_autohide(
        b,
        AutoHideEdges {
            top: true,
            bottom: true,
            left: true,
            right: true,
        },
    );
    assert_eq!((all.left, all.top, all.right, all.bottom), (2, 2, 98, 98));
}

#[test]
fn trait_present_and_scroll_reach_the_real_renderer() {
    let mut real = BitmapRenderer::new(32, 32).unwrap();
    let renderer: &mut dyn Renderer = &mut real;
    renderer.invalidate_all();
    renderer
        .scroll(rect(0.0, 0.0, 32.0, 32.0), Dip(2.0))
        .unwrap();
    renderer.begin_frame().unwrap();
    renderer.draw(&DisplayList::default()).unwrap();
    renderer.end_frame().unwrap();
    renderer
        .present_rect(std::ptr::null_mut(), PixelRect::default())
        .unwrap();
}

fn content_events(f: &Fixture) -> Vec<InputKind> {
    f.events()
        .into_iter()
        .filter_map(|e| match e {
            ShellEvent::ContentInput(input) => Some(input.kind),
            _ => None,
        })
        .collect()
}
fn capture(f: &Fixture) -> bool {
    let holder = unsafe { GetCapture() };
    holder == f.hwnd()
}
/// Sets this thread's view of the modifier keys. No key is really pressed.
fn hold_keys(keys: &[u16]) {
    let mut state = [0u8; 256];
    for key in keys {
        state[*key as usize] = 0x80;
    }
    unsafe { SetKeyboardState(&state) }.unwrap();
}

#[test]
fn a_caption_press_holds_the_mouse_until_release() {
    let f = fixture(None);
    let w = f.width_dips();
    assert!(!capture(&f));
    f.send(
        WM_NCLBUTTONDOWN,
        HTMAXBUTTON as usize,
        f.screen_at(w - 69.0, 20.0),
    );
    assert!(capture(&f), "held");
    f.send(WM_LBUTTONUP, 0, f.at(w - 69.0, 20.0));
    assert!(!capture(&f), "released");
}

#[test]
fn hover_asks_windows_for_a_repaint() {
    let f = fixture(None);
    f.send(WM_PAINT, 0, 0);
    let plus = f.window.with_strip(|s| s.split_rect().x.0 + 5.0).unwrap();
    f.window.invalidated_rects().unwrap();
    f.send(WM_MOUSEMOVE, 0, f.at(plus, 20.0));
    // A hidden window keeps no update region, so `GetUpdateRect` cannot see this. The
    // list holds only rectangles for which `InvalidateRect` itself returned success.
    let rects = f.window.invalidated_rects().unwrap();
    assert_eq!(rects.len(), 1, "{rects:?}");
    let rect = rects[0];
    assert!(rect.right - rect.left <= 40, "only the button: {rect:?}");
    assert!(rect.bottom <= 40, "{rect:?}");
    f.send(WM_MOUSEMOVE, 0, f.at(plus + 1.0, 20.0));
    assert_eq!(f.window.invalidated_rects().unwrap(), vec![], "no change");
}

#[test]
fn window_buttons_send_the_right_system_command() {
    let f = fixture(None);
    let w = f.width_dips();
    let click = |code: u32, x: f32| {
        f.send(WM_NCLBUTTONDOWN, code as usize, f.screen_at(x, 20.0));
        f.send(WM_LBUTTONUP, 0, f.at(x, 20.0));
    };
    click(HTMINBUTTON, w - 115.0);
    assert_eq!(f.commands(), vec![SC_MINIMIZE]);
    click(HTMAXBUTTON, w - 69.0);
    assert_eq!(f.commands(), vec![SC_MINIMIZE, SC_MAXIMIZE]);
    set_maximized_style(&f);
    f.send(WM_SIZE, 0, 0);
    let w = f.width_dips();
    f.send(
        WM_NCLBUTTONDOWN,
        HTMAXBUTTON as usize,
        f.screen_at(w - 69.0, 20.0),
    );
    f.send(WM_LBUTTONUP, 0, f.at(w - 69.0, 20.0));
    assert_eq!(f.commands(), vec![SC_MINIMIZE, SC_MAXIMIZE, SC_RESTORE]);
}

/// With no test double at all: the real `SendMessageW` and `DefWindowProcW` turn the
/// Close button into `WM_CLOSE`, and the window stays hidden and alive.
#[test]
fn the_close_button_reaches_wm_close_through_the_real_system_command() {
    let window = ShellWindow::create_with(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            ..Default::default()
        },
        Box::new(Logged::new()),
        Box::new(RealMeasurer),
    )
    .unwrap();
    let hwnd = HWND(window.hwnd().unwrap());
    window.take_events().unwrap();
    let mut client = RECT::default();
    unsafe { GetClientRect(hwnd, &mut client) }.unwrap();
    let (x, y) = (client.right - 10, 20);
    let mut at = POINT { x, y };
    let _ = unsafe { ClientToScreen(hwnd, &mut at) };
    let send = |msg: u32, w: usize, l: isize| unsafe {
        SendMessageW(hwnd, msg, Some(WPARAM(w)), Some(LPARAM(l)));
    };
    send(WM_NCLBUTTONDOWN, HTCLOSE as usize, pack(at.x, at.y));
    send(WM_LBUTTONUP, 0, pack(x, y));
    assert!(
        window
            .take_events()
            .unwrap()
            .contains(&ShellEvent::CloseWindowRequested)
    );
    assert!(unsafe { IsWindow(Some(hwnd)) }.as_bool());
    assert!(!unsafe { IsWindowVisible(hwnd) }.as_bool());
}

#[test]
fn alt_f4_and_alt_space_reach_the_default_handler() {
    let f = fixture(None);
    let alt_down = 1isize << 29;
    f.send(WM_SYSKEYDOWN, usize::from(VK_F4.0), alt_down);
    // On an inactive window the default handler sends no command, so that this checks
    // that the key reached it. Root's visible run checks that Alt+F4 closes the window.
    assert!(
        f.window
            .default_messages()
            .unwrap()
            .contains(&WM_SYSKEYDOWN)
    );
    f.send(WM_SYSCHAR, usize::from(b' '), alt_down);
    assert_eq!(f.commands(), vec![SC_KEYMENU], "Alt+Space");
}

#[test]
fn alt_chords_reach_the_owner_and_alt_letters_do_not_ask_for_a_menu() {
    let f = fixture(None);
    let alt_down = 1isize << 29;
    let x = 0x58usize;
    f.send(WM_SYSKEYDOWN, x, alt_down);
    assert!(matches!(
        content_events(&f).as_slice(),
        [InputKind::KeyDown {
            virtual_key: 0x58,
            ..
        }]
    ));
    // The default handler looks for a menu here, and beeps. It must never run. The test
    // double stops any `WM_SYSCOMMAND`, so that this test cannot make a sound either.
    assert_eq!(f.send(WM_SYSCHAR, x, alt_down), 0);
    assert!(!f.window.default_messages().unwrap().contains(&WM_SYSCHAR));
    assert_eq!(f.commands(), Vec::<u32>::new());
    f.send(WM_SYSKEYUP, x, alt_down);
    assert!(matches!(
        content_events(&f).as_slice(),
        [InputKind::KeyUp {
            virtual_key: 0x58,
            ..
        }]
    ));
}

#[test]
fn f10_and_alt_never_start_the_invisible_menu_loop() {
    let f = fixture(None);
    for key in [usize::from(VK_F10.0), usize::from(VK_MENU.0)] {
        f.send(WM_SYSKEYDOWN, key, 0);
        f.send(WM_SYSKEYUP, key, 0);
        assert_eq!(f.commands(), Vec::<u32>::new(), "{key:#x}");
        let log = f.window.default_messages().unwrap();
        assert!(!log.contains(&WM_SYSKEYDOWN), "{key:#x}: {log:?}");
        assert!(!log.contains(&WM_SYSKEYUP), "{key:#x}: {log:?}");
    }
    let f10 = content_events(&f);
    assert!(
        f10.iter().any(
            |k| matches!(k, InputKind::KeyDown { virtual_key, .. } if *virtual_key == VK_F10.0)
        )
    );
}

#[test]
fn track_mouse_event_watches_for_the_pointer_leaving() {
    let f = fixture(None);
    let w = f.width_dips();
    // The pointer is not over a hidden window, so Windows may post the leave at once.
    let tracked = |flags: u32| {
        let mut query = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_QUERY,
            ..Default::default()
        };
        unsafe { TrackMouseEvent(&mut query) }.unwrap();
        let live = query.hwndTrack == f.hwnd() && query.dwFlags.0 & flags == flags;
        let mut msg = MSG::default();
        let mut posted = false;
        while unsafe { PeekMessageW(&mut msg, Some(f.hwnd()), 0, 0, PM_REMOVE) }.as_bool() {
            posted |= msg.message == WM_NCMOUSELEAVE || msg.message == WM_MOUSELEAVE;
        }
        live || posted
    };
    f.send(
        WM_NCMOUSEMOVE,
        HTCLOSE as usize,
        f.screen_at(w - 23.0, 20.0),
    );
    assert!(tracked(TME_LEAVE.0 | TME_NONCLIENT.0), "non-client");
    f.send(WM_NCMOUSELEAVE, 0, 0);
    f.send(WM_MOUSEMOVE, 0, f.at(100.0, 300.0));
    assert!(tracked(TME_LEAVE.0), "client");
}

/// Terminal lets these reach `DefWindowProc`, and the Snap flyout may depend on it. The
/// test before this one asserted the opposite. Astra's review reversed it.
#[test]
fn ncmousemove_on_a_window_button_reaches_the_default_handler() {
    let f = fixture(None);
    let w = f.width_dips();
    let count = |f: &Fixture, msg: u32| {
        f.window
            .default_messages()
            .unwrap()
            .iter()
            .filter(|m| **m == msg)
            .count()
    };
    for (i, (code, x)) in [
        (HTMINBUTTON, w - 115.0),
        (HTMAXBUTTON, w - 69.0),
        (HTCLOSE, w - 23.0),
        (HTCAPTION, w - 200.0),
    ]
    .into_iter()
    .enumerate()
    {
        f.send(WM_NCMOUSEMOVE, code as usize, f.screen_at(x, 20.0));
        assert_eq!(count(&f, WM_NCMOUSEMOVE), i + 1, "{code}");
    }
    f.send(WM_NCMOUSELEAVE, 0, 0);
    assert_eq!(count(&f, WM_NCMOUSELEAVE), 1);
}

#[test]
fn both_forms_of_wm_nccalcsize_give_the_same_client_area() {
    let f = fixture(None);
    let start = RECT {
        left: 100,
        top: 100,
        right: 900,
        bottom: 700,
    };
    let mut plain = start;
    f.send(WM_NCCALCSIZE, 0, &mut plain as *mut RECT as isize);
    let mut position = WINDOWPOS::default();
    let mut params = NCCALCSIZE_PARAMS {
        rgrc: [start, start, start],
        lppos: &mut position,
    };
    f.send(
        WM_NCCALCSIZE,
        1,
        &mut params as *mut NCCALCSIZE_PARAMS as isize,
    );
    let wide = params.rgrc[0];
    assert_eq!(
        (plain.left, plain.top, plain.right, plain.bottom),
        (wide.left, wide.top, wide.right, wide.bottom)
    );
    assert!(
        plain.left > start.left && plain.right < start.right,
        "side frame"
    );
    assert_eq!(plain.top, start.top, "no top frame");
    assert!(plain.bottom < start.bottom, "bottom frame");
    println!(
        "measured: ({}, {}, {}, {}) from (100, 100, 900, 700)",
        plain.left, plain.top, plain.right, plain.bottom
    );
}

#[test]
fn a_failed_wm_create_releases_the_window_state_once() {
    let renderer = Logged::new();
    let result = ShellWindow::create_with(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            record_system_commands: true,
            fault: Some(TestFault::FailWmCreate),
            ..Default::default()
        },
        Box::new(renderer.clone()),
        Box::new(RealMeasurer),
    );
    assert!(matches!(
        result,
        Err(ShellError::Win32 {
            call: "CreateWindowExW",
            ..
        })
    ));
    assert_eq!(Rc::strong_count(&renderer.log), 1, "released");
    // A second release would free memory that is still in use, and this would crash.
    for _ in 0..200 {
        let _ = std::hint::black_box(vec![0u8; 512]);
    }
}

#[test]
fn a_colorref_is_blue_green_red() {
    assert_eq!(
        color_from_colorref(0x00112233),
        Color::rgb(0x33, 0x22, 0x11)
    );
}

#[test]
fn minimum_size_is_for_the_client_area_at_each_dpi() {
    for dpi in [96u32, 144] {
        let f = fixture(Some(dpi));
        let mut info = MINMAXINFO::default();
        f.send(WM_GETMINMAXINFO, 0, &mut info as *mut MINMAXINFO as isize);
        let (outer, client) = (f.outer(), f.client());
        let frame_w = outer.right - outer.left - client.right;
        let frame_h = outer.bottom - outer.top - client.bottom;
        let scale = dpi as f32 / 96.0;
        assert_eq!(
            info.ptMinTrackSize.x - frame_w,
            (500.0 * scale).ceil() as i32
        );
        assert_eq!(
            info.ptMinTrackSize.y - frame_h,
            (160.0 * scale).ceil() as i32
        );
    }
}

#[test]
fn the_top_frame_margin_follows_terminal() {
    let f = fixture(None);
    assert!(
        f.window.frame_top_margin().unwrap() > 1,
        "the whole top frame"
    );
    assert_eq!(top_border_height(FrameMode::Restored), 1);
    assert_eq!(top_border_height(FrameMode::Maximized), 0);
    assert_eq!(top_border_height(FrameMode::FullScreen), 0);
    set_maximized_style(&f);
    f.send(WM_SIZE, 0, 0);
    assert_eq!(f.window.frame_top_margin().unwrap(), 0, "maximized");
    let g = fixture(None);
    full_screen_key(&g, false);
    assert_eq!(g.window.frame_top_margin().unwrap(), 0, "full screen");
    full_screen_key(&g, false);
    assert!(g.window.frame_top_margin().unwrap() > 1, "restored again");
}

#[test]
fn typed_characters_reach_the_owner_and_surrogates_join() {
    let f = fixture(None);
    f.send(WM_CHAR, usize::from(b'a'), 0);
    f.send(WM_CHAR, 0xd83d, 0);
    f.send(WM_CHAR, 0xde00, 0);
    assert_eq!(
        content_events(&f),
        vec![InputKind::Char('a'), InputKind::Char('\u{1f600}')]
    );
    f.send(WM_CHAR, 0xde00, 0);
    assert_eq!(content_events(&f), vec![]);
    assert!(
        f.window
            .problems()
            .unwrap()
            .contains(&ShellError::LoneSurrogate { unit: 0xde00 })
    );
}

#[test]
fn a_key_the_strip_takes_does_not_type_its_character() {
    let f = fixture(None);
    f.window.focus_strip().unwrap();
    f.send(WM_KEYDOWN, 0x20, 0);
    f.send(WM_CHAR, 0x20, 0);
    f.send(WM_KEYUP, 0x20, 0);
    let events = content_events(&f);
    assert!(!events.contains(&InputKind::Char(' ')), "{events:?}");
    f.send(WM_KEYDOWN, 0x41, 0);
    f.send(WM_CHAR, usize::from(b'a'), 0);
    assert!(content_events(&f).contains(&InputKind::Char('a')));
}

#[test]
fn other_mouse_buttons_focus_resize_and_dpi_reach_the_owner() {
    let f = fixture(None);
    for (down, up, button) in [
        (WM_RBUTTONDOWN, WM_RBUTTONUP, MouseButton::Right),
        (WM_MBUTTONDOWN, WM_MBUTTONUP, MouseButton::Middle),
    ] {
        f.send(down, 0, f.at(100.0, 300.0));
        assert!(capture(&f), "{button:?} holds the mouse");
        f.send(up, 0, f.at(100.0, 300.0));
        assert!(!capture(&f));
        let events = content_events(&f);
        assert!(matches!(&events[0], InputKind::MouseDown { button: b, .. } if *b == button));
        assert!(matches!(&events[1], InputKind::MouseUp { button: b, .. } if *b == button));
        // Over the row, it stays out of the text area.
        f.send(down, 0, f.at(100.0, 20.0));
        f.send(up, 0, f.at(100.0, 20.0));
        assert_eq!(content_events(&f), vec![]);
    }
    f.send(WM_XBUTTONDOWN, 2 << 16, f.at(100.0, 300.0));
    f.send(WM_XBUTTONUP, 2 << 16, f.at(100.0, 300.0));
    assert!(matches!(
        content_events(&f)[0],
        InputKind::MouseDown {
            button: MouseButton::X2,
            ..
        }
    ));
    f.window.take_events().unwrap();
    f.send(WM_SETFOCUS, 0, 0);
    f.send(WM_KILLFOCUS, 0, 0);
    let kinds: Vec<InputKind> = f
        .window
        .take_events()
        .unwrap()
        .into_iter()
        .filter_map(|e| match e {
            ShellEvent::ContentInput(input) => Some(input.kind),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec![InputKind::Focus, InputKind::Blur]);

    let outer = f.outer();
    unsafe {
        SetWindowPos(
            f.hwnd(),
            None,
            0,
            0,
            outer.right - outer.left + 30,
            outer.bottom - outer.top + 20,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .unwrap();
    let content = f.window.content_rect().unwrap();
    assert!(content_events(&f).contains(&InputKind::Resize {
        width: content.width,
        height: content.height,
    }));
    let suggested = f.outer();
    f.send(
        WM_DPICHANGED,
        144 | (144 << 16),
        &suggested as *const RECT as isize,
    );
    assert!(content_events(&f).contains(&InputKind::DpiChange { dpi: 144 }));
}

#[test]
fn ime_start_and_end_reach_the_owner() {
    let f = fixture(None);
    f.send(WM_IME_STARTCOMPOSITION, 0, 0);
    f.send(WM_IME_ENDCOMPOSITION, 0, 0);
    assert_eq!(
        content_events(&f),
        vec![
            InputKind::ImeStart,
            InputKind::ImeEnd {
                text: String::new()
            }
        ]
    );
}

#[test]
fn the_owner_paints_the_text_area_through_the_hook() {
    let f = fixture(None);
    let seen = Rc::new(RefCell::new(None));
    let log = seen.clone();
    let hwnd = f.hwnd();
    f.window.set_content_painter(move |list, frame| {
        *log.borrow_mut() = Some(frame.rect);
        // A painter holds no borrow, so that it can send messages.
        unsafe { SendMessageW(hwnd, WM_NCHITTEST, None, Some(LPARAM(pack(200, 200)))) };
        list.push(DrawCommand::FillRect {
            rect: rect(0.0, 0.0, 100.0, 100.0),
            color: Color::rgb(10, 200, 30),
        });
    });
    f.window.invalidate_content().unwrap();
    f.send(WM_PAINT, 0, 0);
    let content = f.window.content_rect().unwrap();
    assert_eq!(*seen.borrow(), Some(content));
    assert_eq!(
        content.y.0, 41.0,
        "under the row, which starts 1 pixel down"
    );
    assert_eq!(content.height.0, f.client().bottom as f32 - 41.0);
    let image = f.renderer.bitmap();
    assert_eq!(pixel(&image, 50, 61), [10, 200, 30], "under the row");
    assert_ne!(pixel(&image, 50, 30), [10, 200, 30], "clipped off the row");
    assert_eq!(f.renderer_problems(), vec![]);

    // Only the text area repaints when the owner asks.
    f.renderer.took();
    f.window.invalidate_content().unwrap();
    assert_eq!(f.renderer.took(), ["invalidate"]);
    let rects = f.window.invalidated_rects().unwrap();
    assert!(rects.iter().all(|r| r.top >= 40), "{rects:?}");
    f.window
        .invalidate_rect(rect(0.0, 50.0, 10.0, 10.0))
        .unwrap();
    assert_eq!(f.renderer.took(), ["invalidate"]);
}

#[test]
fn the_text_area_cursor_is_routed_and_the_row_keeps_the_arrow() {
    let f = fixture(None);
    let code = (HTCLIENT as isize) | ((WM_MOUSEMOVE as isize) << 16);
    let cursor = |id| unsafe { LoadCursorW(None, id) }.unwrap();
    f.window.set_content_cursor(MouseCursor::IBeam).unwrap();
    f.send(WM_MOUSEMOVE, 0, f.at(100.0, 300.0));
    assert_eq!(f.send(WM_SETCURSOR, 0, code), 1);
    assert_eq!(unsafe { GetCursor() }, cursor(IDC_IBEAM));
    f.send(WM_MOUSEMOVE, 0, f.at(100.0, 20.0));
    f.send(WM_SETCURSOR, 0, code);
    assert_eq!(unsafe { GetCursor() }, cursor(IDC_ARROW));
    f.window
        .apply_requests(vec![ViewRequest::SetCursor(MouseCursor::Hand)])
        .unwrap();
    f.send(WM_MOUSEMOVE, 0, f.at(100.0, 300.0));
    f.send(WM_SETCURSOR, 0, code);
    assert_eq!(unsafe { GetCursor() }, cursor(IDC_HAND));
    // On the frame, the default handler picks the resize cursor.
    f.send(WM_SETCURSOR, 0, HTLEFT as isize);
    assert!(f.window.default_messages().unwrap().contains(&WM_SETCURSOR));
}

#[test]
fn timers_and_run_requests_reach_the_owner() {
    let f = fixture(None);
    f.window.set_timer(5, 60_000).unwrap();
    f.send(WM_TIMER, 5, 0);
    assert_eq!(f.events(), vec![ShellEvent::Timer(5)]);
    f.window.kill_timer(5).unwrap();
    f.window
        .apply_requests(vec![
            ViewRequest::RequestTimer(60_000),
            ViewRequest::Run(CommandId::Find),
        ])
        .unwrap();
    assert_eq!(f.events(), vec![ShellEvent::RunCommand(CommandId::Find)]);
    f.send(WM_TIMER, VIEW_TIMER as usize, 0);
    assert_eq!(f.events(), vec![ShellEvent::Timer(VIEW_TIMER)]);
    assert!(
        f.window.kill_timer(VIEW_TIMER).is_err(),
        "a view timer fires once"
    );
}

#[test]
fn a_selection_drag_keeps_the_mouse_over_the_row() {
    let f = fixture(None);
    let rest = f.row_list();
    f.send(WM_LBUTTONDOWN, 0, f.at(100.0, 300.0));
    assert!(capture(&f), "the text area holds the mouse");
    f.events();
    f.send(WM_MOUSEMOVE, 0, f.at(100.0, 20.0));
    let events = content_events(&f);
    assert!(
        matches!(events.as_slice(), [InputKind::MouseMove { .. }]),
        "{events:?}"
    );
    assert_eq!(f.row_list(), rest, "the row does not hover meanwhile");
    f.send(WM_LBUTTONUP, 0, f.at(100.0, 20.0));
    assert!(matches!(
        content_events(&f).as_slice(),
        [InputKind::MouseUp { .. }]
    ));
    assert!(!capture(&f), "released");
    // Over the row with no drag, the strip has it again.
    f.send(WM_MOUSEMOVE, 0, f.at(100.0, 20.0));
    assert_eq!(content_events(&f), vec![]);

    f.send(WM_LBUTTONDOWN, 0, f.at(100.0, 300.0));
    f.events();
    f.send(WM_CAPTURECHANGED, 0, 0);
    assert_eq!(f.events(), vec![ShellEvent::ContentCaptureLost]);
    f.send(WM_MOUSEMOVE, 0, f.at(100.0, 20.0));
    assert_eq!(content_events(&f), vec![], "the drag is over");
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture() }.unwrap();
}

#[test]
fn changing_tabs_during_a_press_releases_the_mouse() {
    let f = fixture(None);
    f.send(WM_LBUTTONDOWN, 0, f.at(100.0, 20.0));
    assert!(capture(&f));
    f.window.add_tab(Tab::new("New")).unwrap();
    assert!(!capture(&f), "add");
    f.send(WM_LBUTTONUP, 0, f.at(100.0, 20.0));
    f.send(WM_LBUTTONDOWN, 0, f.at(100.0, 20.0));
    assert!(capture(&f));
    let id = f.window.with_strip(|s| s.tab_id(1).unwrap()).unwrap();
    f.window.close_tab(id).unwrap();
    assert!(!capture(&f), "close");
}

#[test]
fn clicking_the_text_area_takes_focus_from_the_strip() {
    let f = fixture(None);
    f.window.add_tab(Tab::new("Second")).unwrap();
    f.window.focus_strip().unwrap();
    f.events();
    f.send(WM_LBUTTONDOWN, 0, f.at(100.0, 300.0));
    f.send(WM_LBUTTONUP, 0, f.at(100.0, 300.0));
    f.events();
    f.send(WM_KEYDOWN, 0x20, 0);
    let events = f.events();
    assert!(
        !events.iter().any(|e| matches!(e, ShellEvent::SelectTab(_))),
        "{events:?}"
    );
}

#[test]
fn a_full_screen_click_on_the_row_stays_out_of_the_text_area() {
    let f = fixture(None);
    full_screen_key(&f, false);
    f.events();
    let w = f.width_dips();
    assert_eq!(f.hit(w - 200.0, 20.0), HTCLIENT);
    f.send(WM_LBUTTONDOWN, 0, f.at(w - 200.0, 20.0));
    f.send(WM_LBUTTONUP, 0, f.at(w - 200.0, 20.0));
    assert_eq!(content_events(&f), vec![]);
    assert!(!capture(&f));
}

#[test]
fn f11_and_ctrl_t_come_from_a_table_the_owner_can_replace() {
    let f = fixture(None);
    let f11 = usize::from(VK_F11.0);
    let plain_f11_up = |f: &Fixture| {
        f.send(WM_KEYUP, f11, 0);
        assert!(
            !content_events(f)
                .iter()
                .any(|k| matches!(k, InputKind::KeyUp { .. })),
            "its key-up stays with the table"
        );
    };
    f.send(WM_KEYDOWN, f11, 0);
    assert_eq!(f.window.frame_mode().unwrap(), FrameMode::FullScreen);
    plain_f11_up(&f);
    f.send(WM_KEYDOWN, f11, 0);
    assert_eq!(f.window.frame_mode().unwrap(), FrameMode::Restored);
    plain_f11_up(&f);
    f.events();

    // F11 with a modifier is another key.
    hold_keys(&[VK_SHIFT.0]);
    f.send(WM_KEYDOWN, f11, 0);
    assert_eq!(f.window.frame_mode().unwrap(), FrameMode::Restored);
    assert_eq!(content_events(&f).len(), 1, "it reaches the owner");

    hold_keys(&[VK_CONTROL.0]);
    f.send(WM_KEYDOWN, 0x54, 0);
    f.send(WM_CHAR, 0x14, 0);
    assert_eq!(f.events(), vec![ShellEvent::NewTab]);
    f.send(WM_KEYUP, 0x54, 0);
    assert_eq!(f.events(), vec![]);

    // A held key does not make more tabs.
    f.send(WM_KEYDOWN, 0x54, 1 << 30);
    assert_eq!(f.events(), vec![]);

    // The owner moves New Tab, and F11 becomes an ordinary key.
    hold_keys(&[]);
    f.window
        .set_key_table(KeyTable {
            bindings: vec![KeyBinding {
                virtual_key: 0x71,
                modifiers: Modifiers::default(),
                action: ShellAction::NewTab,
            }],
        })
        .unwrap();
    f.send(WM_KEYDOWN, 0x71, 0);
    assert_eq!(f.events(), vec![ShellEvent::NewTab]);
    f.send(WM_KEYDOWN, f11, 0);
    assert_eq!(f.window.frame_mode().unwrap(), FrameMode::Restored);
    assert_eq!(content_events(&f).len(), 1);
    hold_keys(&[VK_CONTROL.0]);
    f.send(WM_KEYDOWN, 0x54, 0);
    assert_eq!(content_events(&f).len(), 1, "Ctrl+T is an ordinary key now");
    hold_keys(&[]);
}

fn contrast_colors() -> ContrastColors {
    ContrastColors {
        window: Color::rgb(0, 0, 0),
        window_text: Color::rgb(255, 255, 255),
        highlight: Color::rgb(26, 235, 255),
        highlight_text: Color::rgb(0, 0, 0),
        gray_text: Color::rgb(63, 242, 63),
    }
}
#[test]
fn a_change_of_only_highlight_text_or_gray_text_repaints() {
    let f = fixture(None);
    let colors = contrast_colors();
    f.window.apply_theme(true, Some(colors)).unwrap();
    f.send(WM_PAINT, 0, 0);
    f.renderer.took();
    f.window.apply_theme(true, Some(colors)).unwrap();
    assert!(
        !f.renderer.took().contains(&"invalidate_all".to_string()),
        "nothing changed"
    );
    for changed in [
        ContrastColors {
            highlight_text: Color::rgb(9, 9, 9),
            ..colors
        },
        ContrastColors {
            gray_text: Color::rgb(9, 9, 9),
            ..colors
        },
    ] {
        f.window.apply_theme(true, Some(changed)).unwrap();
        assert!(f.renderer.took().contains(&"invalidate_all".to_string()));
    }
}

#[test]
fn a_polling_owner_hears_that_the_window_is_gone() {
    let f = fixture(None);
    f.window.destroy().unwrap();
    assert!(f.events().contains(&ShellEvent::WindowDestroyed));
}

#[test]
fn a_late_failure_in_create_destroys_the_hidden_window() {
    let renderer = Logged::new();
    let result = ShellWindow::create_with(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            record_system_commands: true,
            fault: Some(TestFault::AfterCreate),
            ..Default::default()
        },
        Box::new(renderer.clone()),
        Box::new(RealMeasurer),
    );
    assert!(result.is_err());
    // The window owned one copy of the renderer. Only destroying it lets that copy go.
    assert_eq!(Rc::strong_count(&renderer.log), 1, "the window is gone");
}

#[test]
fn a_failed_move_into_full_screen_changes_nothing() {
    let renderer = Logged::new();
    let window = ShellWindow::create_with(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            record_system_commands: true,
            fault: Some(TestFault::FullScreenMove),
            ..Default::default()
        },
        Box::new(renderer.clone()),
        Box::new(RealMeasurer),
    )
    .unwrap();
    let hwnd = HWND(window.hwnd().unwrap());
    let before = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
    unsafe {
        SendMessageW(
            hwnd,
            WM_KEYDOWN,
            Some(WPARAM(usize::from(VK_F11.0))),
            Some(LPARAM(0)),
        );
    }
    assert_eq!(window.frame_mode().unwrap(), FrameMode::Restored);
    assert_eq!(unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) }, before);
    assert!(window.problems().unwrap().iter().any(|p| matches!(
        p,
        ShellError::Win32 {
            call: "SetWindowPos",
            ..
        }
    )));
}

fn ink_rows(image: &Bitmap, x0: u32, x1: u32, y1: u32) -> (u32, u32) {
    let bg = pixel(image, x0, y1 - 1);
    let ink = ink_box(image, (x0, 1, x1, y1), bg, 60).expect("ink");
    (ink.1, ink.3)
}
/// Root measured these on a real Terminal at 100%: `+` ink at y 19 to 29 and caret ink at
/// y 22 to 26, both inclusive, with the row starting below the 1-pixel top border.
#[test]
fn restored_row_starts_below_the_top_border_as_in_terminal() {
    let f = fixture(None);
    f.send(WM_PAINT, 0, 0);
    let image = f.renderer.bitmap();
    let split = f.window.with_strip(|s| s.split_rect()).unwrap();
    assert_eq!(split.y.0, 13.0, "the split button starts on row 13");
    let x = split.x.0 as u32;
    assert_eq!(ink_rows(&image, x + 2, x + 29, 36), (19, 30), "plus");
    assert_eq!(ink_rows(&image, x + 34, x + 60, 36), (22, 27), "caret");
    let tab = f.window.with_strip(|s| s.tab_rects()[0]).unwrap();
    assert_eq!(tab.y.0, 9.0, "the tab's top is 1 pixel lower");
    let row = pixel(&image, x + 100, 30);
    assert_eq!(pixel(&image, 100, 8), row, "above the tab is the row");
    assert_ne!(pixel(&image, 100, 10), row, "the tab starts at 9");
    // The window buttons move with the row: their ink center is 21, as in Terminal.
    let w = f.client().right as u32;
    let ink = ink_box(&image, (w - 40, 1, w - 8, 40), row, 60).unwrap();
    assert_eq!((ink.1 + ink.3) / 2, 21, "close button ink {ink:?}");
    // Hit tests follow it.
    let px = |x: f32, y: i32| f.hit_px(x as i32, y);
    assert_eq!(
        px(split.x.0 + 5.0, 12),
        HTCAPTION,
        "just above the split button"
    );
    assert_eq!(px(split.x.0 + 5.0, 13), HTCLIENT, "its first row");
    assert_eq!(px(split.x.0 + 5.0, 36), HTCLIENT, "its last row");
    assert_eq!(px(split.x.0 + 5.0, 37), HTCAPTION, "just below it");
    assert_eq!(
        px(50.0, 7),
        HTTOP,
        "the band is 8 rows from the window's top"
    );
    assert_eq!(px(50.0, 8), HTCAPTION, "over the tab");
    assert_eq!(px(50.0, 9), HTCLIENT, "the tab's first row");
    assert_eq!(px(50.0, 40), HTCLIENT, "the tab's last row");
    assert_eq!(f.window.content_rect().unwrap().y.0, 41.0);
}

#[test]
fn the_row_starts_one_device_pixel_down_at_each_scale() {
    for dpi in [96u32, 120, 144, 192] {
        let f = fixture(Some(dpi));
        let scale = dpi as f32 / 96.0;
        let split = f.window.with_strip(|s| s.split_rect()).unwrap();
        assert!(
            (split.y.0 * scale - (12.0 * scale + 1.0)).abs() < 0.01,
            "{dpi}: {}",
            split.y.0
        );
    }
}

#[test]
fn maximized_and_full_screen_do_not_shift() {
    let f = fixture(None);
    set_maximized_style(&f);
    f.send(WM_SIZE, 0, 0);
    assert_eq!(f.window.with_strip(|s| s.split_rect().y.0).unwrap(), 4.0);
    assert_eq!(f.window.content_rect().unwrap().y.0, 32.0);
    let g = fixture(None);
    full_screen_key(&g, false);
    assert_eq!(g.window.with_strip(|s| s.split_rect().y.0).unwrap(), 4.0);
    assert_eq!(g.window.content_rect().unwrap().y.0, 32.0);
    full_screen_key(&g, false);
    assert_eq!(g.window.with_strip(|s| s.split_rect().y.0).unwrap(), 13.0);
    assert_eq!(g.window.content_rect().unwrap().y.0, 41.0);
}

fn refresh_fixture(fault: Option<TestFault>) -> (ShellWindow, Logged, Rc<RefCell<Vec<usize>>>) {
    thread_local! {
        static CALLS: RefCell<Option<Rc<RefCell<Vec<usize>>>>> = const { RefCell::new(None) };
        static ANSWER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    let calls: Rc<RefCell<Vec<usize>>> = Rc::default();
    CALLS.with(|c| *c.borrow_mut() = Some(calls.clone()));
    let renderer = Logged::new();
    let window = ShellWindow::create_with_refresh(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            record_system_commands: true,
            fault,
            ..Default::default()
        },
        renderer.clone(),
        |_, hwnd| {
            CALLS.with(|c| {
                if let Some(calls) = &*c.borrow() {
                    calls.borrow_mut().push(hwnd as usize);
                }
            });
            Ok(true)
        },
        Box::new(RealMeasurer),
    )
    .unwrap();
    (window, renderer, calls)
}
#[test]
fn text_settings_refresh_on_each_message_that_can_change_them() {
    let (window, renderer, calls) = refresh_fixture(None);
    let hwnd = HWND(window.hwnd().unwrap());
    let send = |msg: u32, w: usize, l: isize| unsafe {
        SendMessageW(hwnd, msg, Some(WPARAM(w)), Some(LPARAM(l)));
    };
    calls.borrow_mut().clear();
    send(WM_SETTINGCHANGE, 0, 0);
    assert_eq!(
        calls.borrow().as_slice(),
        [hwnd.0 as usize],
        "WM_SETTINGCHANGE"
    );
    send(WM_DISPLAYCHANGE, 32, 0);
    assert_eq!(calls.borrow().len(), 2, "WM_DISPLAYCHANGE");
    let mut outer = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut outer) }.unwrap();
    send(
        WM_DPICHANGED,
        96 | (96 << 16),
        &outer as *const RECT as isize,
    );
    assert_eq!(calls.borrow().len(), 3, "WM_DPICHANGED");
    // A move on the same monitor costs nothing.
    send(WM_MOVE, 0, 0);
    send(WM_MOVE, 0, 0);
    assert!(
        calls.borrow().len() <= 4,
        "at most the first move: {:?}",
        calls.borrow()
    );
    // A paint draws what the refresh asked for.
    send(WM_PAINT, 0, 0);
    renderer.took();
    send(WM_PAINT, 0, 0);
    assert!(
        !renderer.took().contains(&"draw".to_string()),
        "nothing waits"
    );
    send(WM_DISPLAYCHANGE, 32, 0);
    send(WM_PAINT, 0, 0);
    assert!(renderer.took().contains(&"draw".to_string()));
}
#[test]
fn a_move_to_another_monitor_refreshes_text_settings() {
    let (window, _renderer, calls) = refresh_fixture(Some(TestFault::MoveChangesMonitor));
    let hwnd = HWND(window.hwnd().unwrap());
    calls.borrow_mut().clear();
    unsafe { SendMessageW(hwnd, WM_MOVE, None, None) };
    assert_eq!(calls.borrow().len(), 1);
}
#[test]
fn the_real_renderer_refresh_runs_on_a_hidden_window() {
    let window = ShellWindow::create_with_refresh(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            ..Default::default()
        },
        BitmapRenderer::new(64, 64).unwrap(),
        |renderer, hwnd| renderer.refresh_text_settings_for_window(HWND(hwnd)),
        Box::new(RealMeasurer),
    )
    .unwrap();
    let hwnd = HWND(window.hwnd().unwrap());
    unsafe { SendMessageW(hwnd, WM_DISPLAYCHANGE, None, None) };
    let problems = window.problems().unwrap();
    assert!(
        !problems
            .iter()
            .any(|p| matches!(p, ShellError::Renderer { .. })),
        "{problems:?}"
    );
}

/// Any thread can wake a window, a timer cannot replace the wake, and a wake after the
/// window has gone does nothing.
#[test]
fn a_waker_reaches_the_owner_from_another_thread() {
    let f = fixture(None);
    let waker = f.window.handle().waker().unwrap();
    f.window.take_events().unwrap();
    // A long timer asked for after the wake, as a view's blink is, must not replace it.
    let from_thread = waker.clone();
    std::thread::spawn(move || {
        assert!(from_thread.wake());
        // A second wake before the first is served adds no second message.
        assert!(from_thread.wake());
    })
    .join()
    .unwrap();
    f.window
        .apply_requests(vec![ViewRequest::RequestTimer(60_000)])
        .unwrap();
    let hwnd = f.hwnd();
    let mut msg = MSG::default();
    while unsafe { PeekMessageW(&mut msg, Some(hwnd), 0, 0, PM_REMOVE) }.as_bool() {
        unsafe {
            DispatchMessageW(&msg);
        }
    }
    let events = f.window.take_events().unwrap();
    let wakes = events.iter().filter(|e| **e == ShellEvent::Wake).count();
    assert_eq!(wakes, 1, "{events:?}");
    // The next wake after that one is a new message.
    assert!(waker.wake());
    while unsafe { PeekMessageW(&mut msg, Some(hwnd), 0, 0, PM_REMOVE) }.as_bool() {
        unsafe {
            DispatchMessageW(&msg);
        }
    }
    assert!(f.window.take_events().unwrap().contains(&ShellEvent::Wake));
    f.window.destroy().unwrap();
    assert!(!waker.wake(), "a window that has gone cannot wake");
}
