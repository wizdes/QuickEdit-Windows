#![allow(function_casts_as_integer)]
use crate::win::*;
use std::ffi::c_void;
use std::io::Write;
use std::ptr::{null, null_mut};
use std::sync::OnceLock;

unsafe fn create_thread_mgr(out: *mut *mut c_void) -> i32 {
    unsafe {
        let library = LoadLibraryW(wide("msctf.dll").as_ptr());
        if library.is_null() {
            return 0x80004005u32 as i32;
        }
        let symbol = GetProcAddress(library, b"TF_CreateThreadMgr\0".as_ptr());
        if symbol.is_null() {
            return 0x80004005u32 as i32;
        }
        let call: unsafe extern "system" fn(*mut *mut c_void) -> i32 = std::mem::transmute(symbol);
        call(out)
    }
}

const E_NOINTERFACE: i32 = 0x80004002u32 as i32;
const E_NOTIMPL: i32 = 0x80004001u32 as i32;
const E_INVALIDARG: i32 = 0x80070057u32 as i32;
const IID_UNKNOWN: Guid = Guid::new(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_STORE: Guid = Guid::new(
    0x28888fe3,
    0xc2a0,
    0x483a,
    [0xa3, 0xea, 0x8c, 0xb1, 0xce, 0x51, 0xff, 0x3d],
);
const IID_COMP: Guid = Guid::new(
    0x5f20aa40,
    0xb57a,
    0x4f34,
    [0x96, 0xab, 0x35, 0x76, 0xf3, 0x77, 0xcc, 0x79],
);
const IID_THREAD: Guid = Guid::new(
    0xaa80e801,
    0x2021,
    0x11d2,
    [0x93, 0xe0, 0, 0x60, 0xb0, 0x67, 0xb8, 0x6e],
);
const CLSID_THREAD: Guid = Guid::new(
    0x529a9e6b,
    0x6587,
    0x4f23,
    [0xab, 0x9e, 0x9c, 0x7d, 0x68, 0x3e, 0x3c, 0x50],
);
#[repr(C)]
struct Selection {
    start: i32,
    end: i32,
    active: i32,
    interim: i32,
}
#[repr(C)]
struct Change {
    start: i32,
    old_end: i32,
    new_end: i32,
}
#[repr(C)]
struct RunInfo {
    count: u32,
    kind: i32,
}
#[repr(C)]
struct Comp {
    vt: *const usize,
    owner: *mut Store,
}
#[repr(C)]
struct Store {
    vt: *const usize,
    comp: Comp,
    hwnd: Hwnd,
    sink: *mut c_void,
    text: Vec<u16>,
    caret: i32,
    composition: bool,
    comp_start: i32,
    comp_end: i32,
    log: Option<std::fs::File>,
    thread: *mut c_void,
    doc: *mut c_void,
    context: *mut c_void,
}
unsafe fn store(this: *mut c_void) -> &'static mut Store {
    unsafe { &mut *(this as *mut Store) }
}
unsafe fn comp_store(this: *mut c_void) -> &'static mut Store {
    unsafe { &mut *(*(this as *mut Comp)).owner }
}
unsafe fn method<T: Copy>(obj: *mut c_void, index: usize) -> T {
    unsafe {
        let vt = *(obj as *mut *const usize);
        std::mem::transmute_copy(&*vt.add(index))
    }
}
unsafe fn release_com(obj: *mut c_void) {
    unsafe {
        if !obj.is_null() {
            let f: unsafe extern "system" fn(*mut c_void) -> u32 = method(obj, 2);
            f(obj);
        }
    }
}
fn log(s: &mut Store, event: &str) {
    if let Some(f) = s.log.as_mut() {
        if let Err(e) = writeln!(
            f,
            "{event} caret={} text={:?}",
            s.caret,
            String::from_utf16_lossy(&s.text)
        ) {
            eprintln!("TSF log write failed: {e}");
        } else if let Err(e) = f.flush() {
            eprintln!("TSF log flush failed: {e}");
        }
    }
}
unsafe fn caret_x(s: &Store, pos: i32) -> i32 {
    unsafe {
        let count = pos.clamp(0, s.text.len() as i32);
        let dc = GetDC(s.hwnd);
        let mut size = Size::default();
        let _ = GetTextExtentPoint32W(dc, s.text.as_ptr(), count, &mut size);
        ReleaseDC(s.hwnd, dc);
        24 + size.cx
    }
}
unsafe extern "system" fn qi(this: *mut c_void, iid: *const Guid, out: *mut *mut c_void) -> i32 {
    unsafe {
        if out.is_null() {
            return E_INVALIDARG;
        }
        *out = if *iid == IID_UNKNOWN || *iid == IID_STORE {
            this
        } else if *iid == IID_COMP {
            &mut store(this).comp as *mut Comp as *mut c_void
        } else {
            null_mut()
        };
        if (*out).is_null() { E_NOINTERFACE } else { 0 }
    }
}
unsafe extern "system" fn comp_qi(
    this: *mut c_void,
    iid: *const Guid,
    out: *mut *mut c_void,
) -> i32 {
    unsafe { qi(comp_store(this) as *mut Store as *mut c_void, iid, out) }
}
unsafe extern "system" fn addref(_: *mut c_void) -> u32 {
    2
}
unsafe extern "system" fn release(_: *mut c_void) -> u32 {
    1
}
unsafe extern "system" fn stub(_: *mut c_void) -> i32 {
    E_NOTIMPL
}
unsafe extern "system" fn advise(
    this: *mut c_void,
    _iid: *const Guid,
    sink: *mut c_void,
    _mask: u32,
) -> i32 {
    unsafe {
        let s = store(this);
        s.sink = sink;
        let add: unsafe extern "system" fn(*mut c_void) -> u32 = method(sink, 1);
        add(sink);
        0
    }
}
unsafe extern "system" fn unadvise(this: *mut c_void, sink: *mut c_void) -> i32 {
    unsafe {
        let s = store(this);
        if s.sink == sink {
            release_com(s.sink);
            s.sink = null_mut();
            0
        } else {
            E_INVALIDARG
        }
    }
}
unsafe extern "system" fn lock(this: *mut c_void, _flags: u32, session: *mut i32) -> i32 {
    unsafe {
        let s = store(this);
        if s.sink.is_null() {
            *session = E_INVALIDARG;
            return 0;
        }
        let on: unsafe extern "system" fn(*mut c_void, u32) -> i32 = method(s.sink, 3);
        *session = on(s.sink, 0);
        0
    }
}
unsafe extern "system" fn status(_: *mut c_void, out: *mut u32) -> i32 {
    unsafe {
        *out = 0;
        *out.add(1) = 0;
        0
    }
}
unsafe extern "system" fn query_insert(
    this: *mut c_void,
    start: i32,
    end: i32,
    count: u32,
    out_start: *mut i32,
    out_end: *mut i32,
) -> i32 {
    unsafe {
        let s = store(this);
        if start < 0 || end < start || end > s.text.len() as i32 {
            return E_INVALIDARG;
        }
        *out_start = start;
        *out_end = start + count as i32;
        0
    }
}
unsafe extern "system" fn get_selection(
    this: *mut c_void,
    _index: u32,
    count: u32,
    out: *mut Selection,
    fetched: *mut u32,
) -> i32 {
    unsafe {
        if count == 0 {
            return E_INVALIDARG;
        }
        let s = store(this);
        *out = Selection {
            start: s.caret,
            end: s.caret,
            active: 2,
            interim: 0,
        };
        *fetched = 1;
        0
    }
}
unsafe extern "system" fn set_selection(
    this: *mut c_void,
    count: u32,
    selection: *const Selection,
) -> i32 {
    unsafe {
        if count == 0 {
            return E_INVALIDARG;
        }
        let s = store(this);
        s.caret = (*selection).end.clamp(0, s.text.len() as i32);
        InvalidateRect(s.hwnd, null(), 0);
        0
    }
}
unsafe extern "system" fn get_text(
    this: *mut c_void,
    start: i32,
    end: i32,
    plain: *mut u16,
    plain_req: u32,
    plain_ret: *mut u32,
    runs: *mut RunInfo,
    run_req: u32,
    run_ret: *mut u32,
    next: *mut i32,
) -> i32 {
    unsafe {
        let s = store(this);
        let a = start.max(0) as usize;
        let b = if end < 0 { s.text.len() } else { end as usize };
        if a > b || b > s.text.len() {
            return E_INVALIDARG;
        }
        let n = (b - a).min(plain_req as usize);
        if n > 0 && !plain.is_null() {
            std::ptr::copy_nonoverlapping(s.text.as_ptr().add(a), plain, n);
        }
        *plain_ret = n as u32;
        *next = (a + n) as i32;
        *run_ret = if run_req > 0 && n > 0 {
            if !runs.is_null() {
                *runs = RunInfo {
                    count: n as u32,
                    kind: 0,
                };
            }
            1
        } else {
            0
        };
        0
    }
}
unsafe fn replace(
    s: &mut Store,
    start: i32,
    end: i32,
    text: *const u16,
    count: u32,
    change: *mut Change,
) -> i32 {
    unsafe {
        let a = start as usize;
        let b = end as usize;
        if start < 0 || end < start || b > s.text.len() || count > 0 && text.is_null() {
            return E_INVALIDARG;
        }
        let replacement = std::slice::from_raw_parts(
            if count == 0 {
                std::ptr::NonNull::<u16>::dangling().as_ptr()
            } else {
                text
            },
            count as usize,
        );
        s.text.splice(a..b, replacement.iter().copied());
        s.caret = start + count as i32;
        if !change.is_null() {
            *change = Change {
                start,
                old_end: end,
                new_end: s.caret,
            };
        }
        if s.composition {
            s.comp_start = s.comp_start.min(start);
            s.comp_end = s.caret;
            log(s, "composition update");
        } else {
            log(s, "text change");
        }
        InvalidateRect(s.hwnd, null(), 0);
        0
    }
}
unsafe extern "system" fn set_text(
    this: *mut c_void,
    _flags: u32,
    start: i32,
    end: i32,
    text: *const u16,
    count: u32,
    change: *mut Change,
) -> i32 {
    unsafe { replace(store(this), start, end, text, count, change) }
}
unsafe extern "system" fn insert_selection(
    this: *mut c_void,
    flags: u32,
    text: *const u16,
    count: u32,
    start: *mut i32,
    end: *mut i32,
    change: *mut Change,
) -> i32 {
    unsafe {
        let s = store(this);
        let old = s.caret;
        if !start.is_null() {
            *start = old;
        }
        if !end.is_null() {
            *end = old + count as i32;
        }
        if flags & 2 != 0 {
            return 0;
        }
        replace(s, old, old, text, count, change)
    }
}
unsafe extern "system" fn attrs(
    _: *mut c_void,
    _flags: u32,
    _count: u32,
    _ids: *const Guid,
) -> i32 {
    0
}
unsafe extern "system" fn attrs_at(
    _: *mut c_void,
    _position: i32,
    _count: u32,
    _ids: *const Guid,
    _flags: u32,
) -> i32 {
    0
}
unsafe extern "system" fn attr_next(
    _: *mut c_void,
    start: i32,
    _halt: i32,
    _count: u32,
    _ids: *const Guid,
    _flags: u32,
    next: *mut i32,
    found: *mut i32,
    offset: *mut i32,
) -> i32 {
    unsafe {
        *next = start;
        *found = 0;
        *offset = 0;
        0
    }
}
unsafe extern "system" fn attr_retrieve(
    _: *mut c_void,
    _count: u32,
    _values: *mut c_void,
    fetched: *mut u32,
) -> i32 {
    unsafe {
        *fetched = 0;
        0
    }
}
unsafe extern "system" fn end_acp(this: *mut c_void, out: *mut i32) -> i32 {
    unsafe {
        *out = store(this).text.len() as i32;
        0
    }
}
unsafe extern "system" fn active_view(_: *mut c_void, out: *mut u32) -> i32 {
    unsafe {
        *out = 0;
        0
    }
}
unsafe extern "system" fn acp_point(
    this: *mut c_void,
    _view: u32,
    point: *const Point,
    _flags: u32,
    out: *mut i32,
) -> i32 {
    unsafe {
        let s = store(this);
        let mut p = *point;
        ScreenToClient(s.hwnd, &mut p);
        *out = ((p.x - 24) / 10).clamp(0, s.text.len() as i32);
        0
    }
}
unsafe extern "system" fn text_ext(
    this: *mut c_void,
    _view: u32,
    start: i32,
    _end: i32,
    out: *mut Rect,
    clipped: *mut i32,
) -> i32 {
    unsafe {
        let s = store(this);
        let mut p = Point {
            x: caret_x(s, start),
            y: 70,
        };
        ClientToScreen(s.hwnd, &mut p);
        *out = Rect {
            left: p.x,
            top: p.y,
            right: p.x + 2,
            bottom: p.y + 26,
        };
        *clipped = 0;
        0
    }
}
unsafe extern "system" fn screen_ext(this: *mut c_void, _view: u32, out: *mut Rect) -> i32 {
    unsafe {
        GetWindowRect(store(this).hwnd, out);
        0
    }
}
unsafe extern "system" fn get_wnd(this: *mut c_void, _view: u32, out: *mut Hwnd) -> i32 {
    unsafe {
        *out = store(this).hwnd;
        0
    }
}
unsafe extern "system" fn comp_start(
    this: *mut c_void,
    _composition: *mut c_void,
    allow: *mut i32,
) -> i32 {
    unsafe {
        let s = comp_store(this);
        s.composition = true;
        s.comp_start = s.caret;
        s.comp_end = s.caret;
        *allow = 1;
        log(s, "composition start");
        InvalidateRect(s.hwnd, null(), 0);
        0
    }
}
unsafe extern "system" fn comp_update(
    this: *mut c_void,
    _composition: *mut c_void,
    _range: *mut c_void,
) -> i32 {
    unsafe {
        let s = comp_store(this);
        log(s, "composition update");
        InvalidateRect(s.hwnd, null(), 0);
        0
    }
}
unsafe extern "system" fn comp_end(this: *mut c_void, _composition: *mut c_void) -> i32 {
    unsafe {
        let s = comp_store(this);
        log(s, "composition end");
        s.composition = false;
        InvalidateRect(s.hwnd, null(), 0);
        0
    }
}
#[rustfmt::skip]
fn vtable() -> *const usize {
    static VT: OnceLock<Box<[usize]>> = OnceLock::new();
    VT.get_or_init(|| vec![
        qi as usize, addref as usize, release as usize, advise as usize, unadvise as usize, lock as usize,
        status as usize, query_insert as usize, get_selection as usize, set_selection as usize,
        get_text as usize, set_text as usize, stub as usize, stub as usize, stub as usize,
        stub as usize, insert_selection as usize, stub as usize, attrs as usize, attrs_at as usize,
        attrs_at as usize, attr_next as usize, attr_retrieve as usize, end_acp as usize,
        active_view as usize, acp_point as usize, text_ext as usize, screen_ext as usize, get_wnd as usize,
    ].into_boxed_slice()).as_ptr()
}
#[rustfmt::skip]
fn comp_vtable() -> *const usize {
    static VT: OnceLock<Box<[usize]>> = OnceLock::new();
    VT.get_or_init(|| vec![comp_qi as usize, addref as usize, release as usize,
        comp_start as usize, comp_update as usize, comp_end as usize].into_boxed_slice()).as_ptr()
}
unsafe fn notify_change(s: &Store, change: Change) {
    unsafe {
        if !s.sink.is_null() {
            let on: unsafe extern "system" fn(*mut c_void, u32, *const Change) -> i32 =
                method(s.sink, 4);
            let hr = on(s.sink, 0, &change);
            if hr < 0 {
                eprintln!("OnTextChange failed: 0x{:08x}", hr as u32);
            }
            let sel: unsafe extern "system" fn(*mut c_void) -> i32 = method(s.sink, 5);
            let hr = sel(s.sink);
            if hr < 0 {
                eprintln!("OnSelectionChange failed: 0x{:08x}", hr as u32);
            }
        }
    }
}
unsafe extern "system" fn proc(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize {
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, -21) as *mut Store;
        match msg {
            WM_APP_LAYOUT if !ptr.is_null() => {
                let s = &*ptr;
                if !s.sink.is_null() {
                    let on: unsafe extern "system" fn(*mut c_void, i32, u32) -> i32 =
                        method(s.sink, 6);
                    let hr = on(s.sink, 1, 0);
                    if hr < 0 {
                        eprintln!("OnLayoutChange failed: 0x{:08x}", hr as u32);
                    }
                }
                0
            }
            WM_PAINT => {
                let mut p = std::mem::zeroed::<Paint>();
                let dc = BeginPaint(hwnd, &mut p);
                let mut r = Rect::default();
                GetClientRect(hwnd, &mut r);
                fill(dc, r, rgb(255, 255, 255));
                text(
                    dc,
                    "Type with a Japanese IME below:",
                    Rect {
                        left: 24,
                        top: 20,
                        right: r.right - 24,
                        bottom: 50,
                    },
                    rgb(40, 40, 40),
                    DT_SINGLELINE,
                );
                if !ptr.is_null() {
                    let s = &*ptr;
                    let line = String::from_utf16_lossy(&s.text);
                    text(
                        dc,
                        &line,
                        Rect {
                            left: 24,
                            top: 70,
                            right: r.right - 24,
                            bottom: 102,
                        },
                        rgb(0, 0, 0),
                        DT_SINGLELINE,
                    );
                    let x = caret_x(s, s.caret);
                    MoveToEx(dc, x, 70, null_mut());
                    LineTo(dc, x, 96);
                    if s.composition {
                        MoveToEx(dc, caret_x(s, s.comp_start), 99, null_mut());
                        LineTo(dc, caret_x(s, s.comp_end), 99);
                    }
                }
                EndPaint(hwnd, &p);
                0
            }
            WM_CHAR if !ptr.is_null() => {
                if w >= 32 {
                    let s = &mut *ptr;
                    let old = s.caret;
                    let c = w as u16;
                    let mut change = Change {
                        start: old,
                        old_end: old,
                        new_end: old + 1,
                    };
                    replace(s, old, old, &c, 1, &mut change);
                    notify_change(s, change);
                }
                0
            }
            WM_DESTROY => {
                if !ptr.is_null() {
                    let s = &mut *ptr;
                    if !s.doc.is_null() {
                        let pop: unsafe extern "system" fn(*mut c_void, u32) -> i32 =
                            method(s.doc, 5);
                        let _ = pop(s.doc, 1);
                    }
                    if !s.thread.is_null() {
                        let deact: unsafe extern "system" fn(*mut c_void) -> i32 =
                            method(s.thread, 4);
                        let _ = deact(s.thread);
                    }
                    release_com(s.context);
                    release_com(s.doc);
                    release_com(s.thread);
                    release_com(s.sink);
                    drop(Box::from_raw(ptr));
                }
                CoUninitialize();
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}
#[rustfmt::skip]
pub fn run(path: Option<&str>) -> Result<(), String> {
    unsafe {
        ok(CoInitializeEx(null_mut(), 2), "CoInitializeEx")?;
        let hwnd = make_window("SpikeTSF", "TSF trial", proc, 700, 260)?;
        let file = match path {
            Some(p) => Some(std::fs::File::create(p).map_err(|e| format!("--log {p}: {e}"))?),
            None => None,
        };
        let mut owned = Box::new(Store {
            vt: vtable(), comp: Comp { vt: comp_vtable(), owner: null_mut() },
            hwnd, sink: null_mut(), text: Vec::new(), caret: 0,
            composition: false, comp_start: 0, comp_end: 0, log: file,
            thread: null_mut(), doc: null_mut(), context: null_mut(),
        });
        owned.comp.owner = &mut *owned;
        let s = Box::into_raw(owned);
        SetWindowLongPtrW(hwnd, -21, s as isize);
        let hr = CoCreateInstance(&CLSID_THREAD, null_mut(), 1, &IID_THREAD, &mut (*s).thread);
        if hr < 0 {
            eprintln!("CoCreateInstance(ThreadMgr) failed: 0x{:08x}; trying TF_CreateThreadMgr", hr as u32);
            ok(create_thread_mgr(&mut (*s).thread), "TF_CreateThreadMgr")?;
        }
        let activate: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 = method((*s).thread, 3);
        let mut client = 0;
        ok(activate((*s).thread, &mut client), "ITfThreadMgr::Activate")?;
        let create_doc: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 = method((*s).thread, 5);
        ok(create_doc((*s).thread, &mut (*s).doc), "CreateDocumentMgr")?;
        let create_context: unsafe extern "system" fn(*mut c_void, u32, u32, *mut c_void, *mut *mut c_void, *mut u32) -> i32 = method((*s).doc, 3);
        let mut cookie = 0;
        ok(create_context((*s).doc, client, 0, s.cast(), &mut (*s).context, &mut cookie), "CreateContext")?;
        let push: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32 = method((*s).doc, 4);
        ok(push((*s).doc, (*s).context), "DocumentMgr::Push")?;
        let associate: unsafe extern "system" fn(*mut c_void, Hwnd, *mut c_void, *mut *mut c_void) -> i32 = method((*s).thread, 9);
        let mut previous = null_mut();
        ok(associate((*s).thread, hwnd, (*s).doc, &mut previous), "AssociateFocus")?;
        release_com(previous);
        SetFocus(hwnd);
        loop_messages();
        Ok(())
    }
}
