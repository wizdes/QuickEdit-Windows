//! Hidden windows only. Every test ends by checking that its window never showed.
use qe_api::{
    Bitmap, Dip, DisplayList, PixelRect, Rect, RenderError, Renderer, Theme, ThemeKind, View,
};
use qe_render::BitmapRenderer;
use qe_shell::{
    AutoHideEdges, CaptionHit, FrameMode, ShellError, ShellEvent, ShellWindow, Tab, TabStrip,
    WindowConfig, apply_autohide, theme_from_status,
};
use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_F11, VK_MENU};
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
        },
        Box::new(renderer.clone()),
        Box::new(RealMeasurer),
    )
    .unwrap();
    Fixture {
        window,
        renderer,
        scale: dpi.unwrap_or(96) as f32 / 96.0,
    }
}
fn fixture(dpi: Option<u32>) -> Fixture {
    fixture_with(dpi, Logged::new())
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
    fn events(&self) -> Vec<ShellEvent> {
        self.window.take_events().unwrap()
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
    let f = fixture(Some(144));
    let log = f.renderer.took();
    assert_eq!(log[0], "dpi 144", "{log:?}");
    assert!(!log.iter().any(|l| l == "draw"), "no draw yet: {log:?}");
    assert_eq!(f.window.dpi().unwrap(), 144);
    let (columns, rows, row): (f32, f32, f32) = (90.0, 30.0, 40.0);
    let width = (columns * 8.0 + 138.0) * 1.5;
    let height = (rows * 17.0 + row) * 1.5;
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
    let x = (500.0 * 1.5) as u32;
    assert_ne!(pixel(&image, x, 50), pixel(&image, x, 70));
    assert_eq!(pixel(&image, x, 62), pixel(&image, x, 300));
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
    assert_eq!(pixel(&image, 300, 40), text, "the row stops at 40");
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
    assert!(f.events().contains(&ShellEvent::Problem(expected)));
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
        assert_eq!(f.hit(w - 23.0, 0.0), HTCLOSE, "{dpi} close at the very top");
        assert_eq!(f.hit(w - 69.0, 0.0), HTMAXBUTTON, "{dpi} all of maximize");
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
    let suggested = RECT {
        left: 40,
        top: 50,
        right: 1240,
        bottom: 950,
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
        (40, 50, 1240, 950)
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
    let b = f.window.with_strip(|s| s.tab_rects()[0]).unwrap();
    let x = b.x.0 + b.width.0 - 5.0;
    f.send(WM_LBUTTONDOWN, 0, f.at(x, 20.0));
    f.send(WM_LBUTTONUP, 0, f.at(x, 20.0));
    assert!(f.events().contains(&ShellEvent::CloseTabRequested(0)));
    assert_eq!(f.window.with_strip(|s| s.tabs().len()).unwrap(), 2);
    f.window.close_tab(0).unwrap();
    assert_eq!(f.window.with_strip(|s| s.tabs().len()).unwrap(), 1);
}

#[test]
fn many_windows_share_one_thread_and_nothing_quits_on_its_own() {
    let a = fixture(None);
    let b = fixture(None);
    let hwnd_b = b.hwnd();
    a.window.destroy().unwrap();
    assert!(a.events_after_destroy());
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
impl Fixture {
    /// The window has gone, so its queue can only be seen from a handler set earlier.
    fn events_after_destroy(&self) -> bool {
        self.window.take_events() == Err(ShellError::WindowGone)
    }
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
    assert!(f.events().is_empty(), "the strip has focus and takes it");
    f.send(WM_KEYDOWN, 0x1b, 0);
    assert_eq!(f.events(), vec![ShellEvent::FocusContent]);

    let w = f.window.with_strip(|s| s.tab_rects()[0].x.0).unwrap();
    let notch_down = (-120i16 as u16 as usize) << 16;
    f.send(WM_MOUSEWHEEL, notch_down, f.screen_at(200.0, 400.0));
    assert!(matches!(
        f.events().as_slice(),
        [ShellEvent::ContentInput(_)]
    ));
    assert_eq!(f.window.with_strip(|s| s.tab_rects()[0].x.0).unwrap(), w);
    if f.window.with_strip(|s| s.overflow()).unwrap() {
        f.window.with_strip(|s| s.select(0)).unwrap();
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
