use std::{env, ffi::c_void, fs, mem, ptr};

type H = *mut c_void;
type Hr = i32;
#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    a: u32,
    b: u16,
    c: u16,
    d: [u8; 8],
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}
#[repr(C)]
struct Msg {
    hwnd: H,
    message: u32,
    wp: usize,
    lp: isize,
    time: u32,
    pt: Point,
    private: u32,
}
#[repr(C)]
struct WndClass {
    style: u32,
    proc: Option<unsafe extern "system" fn(H, u32, usize, isize) -> isize>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: H,
    icon: H,
    cursor: H,
    background: H,
    menu: *const u16,
    name: *const u16,
}
#[repr(C)]
struct GlyphRun {
    face: H,
    size: f32,
    count: u32,
    indices: *const u16,
    advances: *const f32,
    offsets: H,
    sideways: i32,
    bidi: u32,
}
#[repr(C)]
struct Size {
    cx: i32,
    cy: i32,
}
#[link(name = "user32")]
unsafe extern "system" {
    fn SetProcessDpiAwarenessContext(v: isize) -> i32;
    fn RegisterClassW(c: *const WndClass) -> u16;
    fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: H,
        menu: H,
        inst: H,
        param: H,
    ) -> H;
    fn DefWindowProcW(h: H, m: u32, w: usize, l: isize) -> isize;
    fn ShowWindow(h: H, c: i32) -> i32;
    fn UpdateWindow(h: H) -> i32;
    fn GetMessageW(m: *mut Msg, h: H, min: u32, max: u32) -> i32;
    fn TranslateMessage(m: *const Msg) -> i32;
    fn DispatchMessageW(m: *const Msg) -> isize;
    fn PostQuitMessage(c: i32);
    fn ValidateRect(h: H, r: *const Rect) -> i32;
    fn GetDC(h: H) -> H;
    fn ReleaseDC(h: H, dc: H) -> i32;
    fn GetDpiForWindow(h: H) -> u32;
    fn AdjustWindowRectExForDpi(r: *mut Rect, s: u32, menu: i32, ex: u32, dpi: u32) -> i32;
    fn SetWindowPos(h: H, after: H, x: i32, y: i32, w: i32, hgt: i32, flags: u32) -> i32;
}
#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(dc: H) -> H;
    fn DeleteDC(dc: H) -> i32;
    fn CreateFontW(
        height: i32,
        width: i32,
        esc: i32,
        orient: i32,
        weight: i32,
        italic: u32,
        under: u32,
        strike: u32,
        charset: u32,
        out: u32,
        clip: u32,
        quality: u32,
        pitch: u32,
        name: *const u16,
    ) -> H;
    fn SelectObject(dc: H, obj: H) -> H;
    fn DeleteObject(obj: H) -> i32;
    fn GetTextExtentPoint32W(dc: H, text: *const u16, len: i32, size: *mut Size) -> i32;
    fn PatBlt(dc: H, x: i32, y: i32, w: i32, h: i32, rop: u32) -> i32;
    fn BitBlt(dst: H, x: i32, y: i32, w: i32, h: i32, src: H, sx: i32, sy: i32, rop: u32) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryPerformanceCounter(t: *mut i64) -> i32;
    fn GetModuleHandleW(name: *const u16) -> H;
}
#[link(name = "dwrite")]
unsafe extern "system" {
    fn DWriteCreateFactory(ty: u32, iid: *const Guid, out: *mut H) -> Hr;
}
const IID_FACTORY: Guid = Guid {
    a: 0xb859ee5a,
    b: 0xd838,
    c: 0x4b5b,
    d: [0xa2, 0xe8, 0x1a, 0xdc, 0x7d, 0x93, 0xdb, 0x48],
};
const SAMPLE: [&str; 30] = [
    "QuickEdit renderer race - Cascadia Mono 10 pt",
    "0123456789 abcdefghijklmnopqrstuvwxyz",
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ !@#$%^&*()",
    "The quick brown fox jumps over the lazy dog.",
    "A: DirectWrite bitmap and BitBlt",
    "B: Direct2D and hardware swap chain",
    "C: Direct3D glyph atlas and waitable swap chain",
    "D: software first, then hardware",
    "",
    "fn main() { println!(\"Hello, world!\"); }",
    "let latency = after_present - key_received;",
    "if latency < budget { show_frame(); }",
    "Window size: 90 columns by 30 rows",
    "DPI aware per monitor, version 2",
    "White background. Black text.",
    "This same text appears in each renderer.",
    "Key presses append to the first line.",
    "The new character appears after this marker:",
    "A screen capture can watch that spot.",
    "The launcher records the first frame.",
    "The typing tool records screen changes.",
    "One window remains open for the whole run.",
    "Measure cold and warm starts separately.",
    "Percentiles use nearest rank.",
    "The GPU paths require a desktop session.",
    "No animation runs while idle.",
    "Test line 27: 11111111111111111111",
    "Test line 28: 22222222222222222222",
    "Test line 29: 33333333333333333333",
    "Test line 30: END OF SAMPLE TEXT",
];
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
unsafe fn method(p: H, index: usize) -> H {
    unsafe { *(*(p as *const *const H)).add(index) }
}
unsafe fn release(p: H) {
    if !p.is_null() {
        let f: unsafe extern "system" fn(H) -> u32 = unsafe { mem::transmute(method(p, 2)) };
        unsafe { f(p) };
    }
}
fn check(hr: Hr, what: &str) -> Result<(), String> {
    if hr < 0 {
        Err(format!("{what} failed: HRESULT {hr:#010x}"))
    } else {
        Ok(())
    }
}
fn qpc() -> i64 {
    let mut v = 0;
    unsafe { QueryPerformanceCounter(&mut v) };
    v
}
struct State {
    first: bool,
    start: i64,
    launch_log: Option<String>,
    type_log: Option<String>,
    exit_first: bool,
    typed: String,
    face: H,
    target: H,
    render_params: H,
    dc: H,
    font: H,
    old_font: H,
    width: i32,
    height: i32,
    cell: i32,
    line: i32,
    font_size: f32,
}
impl State {
    fn new(
        start: i64,
        launch_log: Option<String>,
        type_log: Option<String>,
        exit_first: bool,
    ) -> Self {
        Self {
            first: true,
            start,
            launch_log,
            type_log,
            exit_first,
            typed: String::new(),
            face: ptr::null_mut(),
            target: ptr::null_mut(),
            render_params: ptr::null_mut(),
            dc: ptr::null_mut(),
            font: ptr::null_mut(),
            old_font: ptr::null_mut(),
            width: 0,
            height: 0,
            cell: 0,
            line: 0,
            font_size: 0.0,
        }
    }
    unsafe fn init(&mut self, hwnd: H) -> Result<(), String> {
        let dpi = unsafe { GetDpiForWindow(hwnd) } as i32;
        let screen = unsafe { GetDC(hwnd) };
        self.dc = unsafe { CreateCompatibleDC(screen) };
        unsafe { ReleaseDC(hwnd, screen) };
        let font = unsafe {
            CreateFontW(
                -(10 * dpi / 72),
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                1,
                0,
                0,
                5,
                49,
                wide("Cascadia Mono").as_ptr(),
            )
        };
        if font.is_null() {
            return Err("CreateFontW failed".into());
        }
        self.font = font;
        self.old_font = unsafe { SelectObject(self.dc, font) };
        let mut size = Size { cx: 0, cy: 0 };
        let one = wide("M");
        if unsafe { GetTextExtentPoint32W(self.dc, one.as_ptr(), 1, &mut size) } == 0 {
            return Err("GetTextExtentPoint32W failed".into());
        }
        self.cell = size.cx;
        self.line = size.cy + 2 * dpi / 96;
        self.width = self.cell * 90;
        self.height = self.line * 30;
        self.font_size = 10.0 * dpi as f32 / 72.0;
        let mut factory = ptr::null_mut();
        check(
            unsafe { DWriteCreateFactory(0, &IID_FACTORY, &mut factory) },
            "DWriteCreateFactory",
        )?;
        let mut interop = ptr::null_mut();
        let get: unsafe extern "system" fn(H, *mut H) -> Hr =
            unsafe { mem::transmute(method(factory, 17)) };
        check(unsafe { get(factory, &mut interop) }, "GetGdiInterop")?;
        let params_fn: unsafe extern "system" fn(H, *mut H) -> Hr =
            unsafe { mem::transmute(method(factory, 10)) };
        check(
            unsafe { params_fn(factory, &mut self.render_params) },
            "CreateRenderingParams",
        )?;
        let face_fn: unsafe extern "system" fn(H, H, *mut H) -> Hr =
            unsafe { mem::transmute(method(interop, 6)) };
        check(
            unsafe { face_fn(interop, self.dc, &mut self.face) },
            "CreateFontFaceFromHdc",
        )?;
        let target_fn: unsafe extern "system" fn(H, H, u32, u32, *mut H) -> Hr =
            unsafe { mem::transmute(method(interop, 7)) };
        let screen = unsafe { GetDC(hwnd) };
        let result = check(
            unsafe {
                target_fn(
                    interop,
                    screen,
                    self.width as u32,
                    self.height as u32,
                    &mut self.target,
                )
            },
            "CreateBitmapRenderTarget",
        );
        unsafe {
            ReleaseDC(hwnd, screen);
            release(interop);
            release(factory)
        };
        result
    }
    unsafe fn draw(&mut self, hwnd: H) -> Result<i64, String> {
        let memdc_fn: unsafe extern "system" fn(H) -> H =
            unsafe { mem::transmute(method(self.target, 4)) };
        let memdc = unsafe { memdc_fn(self.target) };
        if unsafe { PatBlt(memdc, 0, 0, self.width, self.height, 0x00ff0062) } == 0 {
            return Err("PatBlt failed".into());
        }
        for (row, sample) in SAMPLE.iter().enumerate() {
            let line = if row == 0 {
                format!("{} {}", sample, self.typed)
            } else {
                (*sample).to_owned()
            };
            let line: Vec<u16> = line.encode_utf16().collect();
            let count = line.len();
            let mut glyphs = vec![0u16; count];
            let indices: unsafe extern "system" fn(H, *const u32, u32, *mut u16) -> Hr =
                unsafe { mem::transmute(method(self.face, 11)) };
            let codepoints: Vec<u32> = line.iter().map(|c| *c as u32).collect();
            check(
                unsafe {
                    indices(
                        self.face,
                        codepoints.as_ptr(),
                        count as u32,
                        glyphs.as_mut_ptr(),
                    )
                },
                "GetGlyphIndices",
            )?;
            let advances = vec![self.cell as f32; count];
            let run = GlyphRun {
                face: self.face,
                size: self.font_size,
                count: count as u32,
                indices: glyphs.as_ptr(),
                advances: advances.as_ptr(),
                offsets: ptr::null_mut(),
                sideways: 0,
                bidi: 0,
            };
            let draw: unsafe extern "system" fn(
                H,
                f32,
                f32,
                u32,
                *const GlyphRun,
                H,
                u32,
                *mut Rect,
            ) -> Hr = unsafe { mem::transmute(method(self.target, 3)) };
            let mut dirty = Rect::default();
            check(
                unsafe {
                    draw(
                        self.target,
                        2.0,
                        (row as i32 * self.line + self.line - 3) as f32,
                        0,
                        &run,
                        self.render_params,
                        0,
                        &mut dirty,
                    )
                },
                "DrawGlyphRun",
            )?;
        }
        let dc = unsafe { GetDC(hwnd) };
        let copied = unsafe { BitBlt(dc, 0, 0, self.width, self.height, memdc, 0, 0, 0x00cc0020) };
        unsafe { ReleaseDC(hwnd, dc) };
        if copied == 0 {
            return Err("BitBlt failed".into());
        }
        Ok(qpc())
    }
}
impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            release(self.target);
            release(self.face);
            release(self.render_params);
            if !self.dc.is_null() {
                SelectObject(self.dc, self.old_font);
                DeleteDC(self.dc);
            }
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
        }
    }
}
static mut STATE: *mut State = ptr::null_mut();
unsafe extern "system" fn wndproc(hwnd: H, msg: u32, wp: usize, lp: isize) -> isize {
    match msg {
        0x000f => {
            let state = unsafe { &mut *STATE };
            match unsafe { state.draw(hwnd) } {
                Ok(after) => {
                    if state.first {
                        state.first = false;
                        if let Some(path) = &state.launch_log {
                            if let Err(e) = fs::write(path, format!("{},{}\n", state.start, after))
                            {
                                eprintln!("launch log: {e}");
                                unsafe { PostQuitMessage(1) };
                            }
                        }
                        if state.exit_first {
                            unsafe { PostQuitMessage(0) }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("draw: {e}");
                    unsafe { PostQuitMessage(1) }
                }
            }
            unsafe { ValidateRect(hwnd, ptr::null()) };
            0
        }
        0x0102 => {
            let state = unsafe { &mut *STATE };
            let received = qpc();
            if let Some(c) = char::from_u32(wp as u32) {
                if !c.is_control() {
                    state.typed.push(c);
                    match unsafe { state.draw(hwnd) } {
                        Ok(after) => {
                            if let Some(path) = &state.type_log {
                                use std::io::Write;
                                match fs::OpenOptions::new().create(true).append(true).open(path) {
                                    Ok(mut f) => {
                                        if let Err(e) = writeln!(f, "{received},{after},{wp}") {
                                            eprintln!("type log: {e}");
                                            unsafe { PostQuitMessage(1) };
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("type log: {e}");
                                        unsafe { PostQuitMessage(1) };
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("draw: {e}");
                            unsafe { PostQuitMessage(1) };
                        }
                    }
                }
            }
            0
        }
        0x0002 => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}
fn run(start: i64) -> Result<(), String> {
    let mut arm = None;
    let mut launch_log = None;
    let mut type_log = None;
    let mut exit_first = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--arm" => arm = args.next().and_then(|s| s.chars().next()),
            "--launch-log" => launch_log = args.next(),
            "--type-log" => type_log = args.next(),
            "--exit-after-first-frame" => exit_first = true,
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    let arm = arm.ok_or("--arm a|b|c|d is required")?;
    if !"abcd".contains(arm) {
        return Err("--arm must be a, b, c, or d".into());
    }
    if arm != 'a' {
        return Err(format!("arm {arm} is not implemented yet"));
    }
    if let Some(path) = &type_log {
        fs::write(path, "").map_err(|e| format!("type log: {e}"))?;
    }
    unsafe { SetProcessDpiAwarenessContext(-4) };
    let class = wide("QeRendererSpike");
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let wc = WndClass {
        style: 0,
        proc: Some(wndproc),
        cls_extra: 0,
        wnd_extra: 0,
        instance,
        icon: ptr::null_mut(),
        cursor: ptr::null_mut(),
        background: ptr::null_mut(),
        menu: ptr::null(),
        name: class.as_ptr(),
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        return Err("RegisterClassW failed".into());
    }
    let mut state = Box::new(State::new(start, launch_log, type_log, exit_first));
    unsafe { STATE = &mut *state };
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            wide("QuickEdit renderer spike").as_ptr(),
            0x10cf0000,
            100,
            100,
            900,
            700,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null_mut(),
        )
    };
    if hwnd.is_null() {
        return Err("CreateWindowExW failed".into());
    }
    unsafe { state.init(hwnd) }?;
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let mut rect = Rect {
        left: 0,
        top: 0,
        right: state.width,
        bottom: state.height,
    };
    unsafe { AdjustWindowRectExForDpi(&mut rect, 0x10cf0000, 0, 0, dpi) };
    unsafe {
        SetWindowPos(
            hwnd,
            ptr::null_mut(),
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            0x0006,
        );
        ShowWindow(hwnd, 5);
        UpdateWindow(hwnd)
    };
    let mut msg = unsafe { mem::zeroed::<Msg>() };
    loop {
        let result = unsafe { GetMessageW(&mut msg, ptr::null_mut(), 0, 0) };
        if result == 0 {
            break;
        }
        if result < 0 {
            return Err("GetMessageW failed".into());
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    if msg.wp == 0 {
        Ok(())
    } else {
        Err(format!("window exited with code {}", msg.wp))
    }
}
fn main() {
    let start = qpc();
    if let Err(e) = run(start) {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
