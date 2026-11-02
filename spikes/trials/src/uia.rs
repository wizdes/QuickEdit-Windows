#![allow(function_casts_as_integer)]
use crate::win::*;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};

const S_OK: i32 = 0;
const E_NOINTERFACE: i32 = 0x80004002u32 as i32;
const E_NOTIMPL: i32 = 0x80004001u32 as i32;
const E_FAIL: i32 = 0x80004005u32 as i32;
const IID_UNKNOWN: Guid = Guid::new(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_SIMPLE: Guid = Guid::new(
    0xd6dd68d1,
    0x86fd,
    0x4332,
    [0x86, 0x66, 0x9a, 0xbe, 0xde, 0xa2, 0xd2, 0x4c],
);
const IID_TEXT: Guid = Guid::new(
    0x3589c92c,
    0x63f3,
    0x4367,
    [0x99, 0xbb, 0xad, 0xa6, 0x53, 0xb7, 0x7c, 0xf2],
);
const IID_RANGE: Guid = Guid::new(
    0x5347ad7b,
    0xc355,
    0x46f8,
    [0xaf, 0xf5, 0x90, 0x90, 0x33, 0x58, 0x2f, 0x63],
);
const CLSID_UIAUTOMATION: Guid = Guid::new(
    0xff48dba4,
    0x60ef,
    0x4201,
    [0xaa, 0x87, 0x54, 0x10, 0x3e, 0xef, 0x59, 0x4e],
);
const IID_UIAUTOMATION: Guid = Guid::new(
    0x30cbe57d,
    0xd9d0,
    0x452a,
    [0xab, 0x13, 0x7a, 0xc5, 0xac, 0x48, 0x25, 0xee],
);
const TEXT: &str = "QuickEdit UIA trial";
static SIMPLE: AtomicUsize = AtomicUsize::new(0);
static TEXT_PROVIDER: AtomicUsize = AtomicUsize::new(0);
static SELECTION: AtomicUsize = AtomicUsize::new(9);
#[repr(C)]
struct Obj {
    vt: *const usize,
}
#[repr(C)]
struct Range {
    vt: *const usize,
    start: i32,
    end: i32,
}
#[repr(C)]
struct Variant {
    vt: u16,
    r1: u16,
    r2: u16,
    r3: u16,
    data: u64,
    extra: u64,
}
#[link(name = "uiautomationcore")]
unsafe extern "system" {
    fn UiaReturnRawElementProvider(hwnd: Hwnd, w: usize, l: isize, p: *mut c_void) -> isize;
}
#[link(name = "oleaut32")]
unsafe extern "system" {
    fn SysAllocStringLen(s: *const u16, len: u32) -> *mut u16;
    fn SysFreeString(s: *mut u16);
    fn SafeArrayCreateVector(vt: u16, lower: i32, count: u32) -> *mut c_void;
    fn SafeArrayPutElement(a: *mut c_void, index: *const i32, element: *mut c_void) -> i32;
    fn SafeArrayDestroy(a: *mut c_void) -> i32;
}
unsafe extern "system" fn qi(this: *mut c_void, iid: *const Guid, out: *mut *mut c_void) -> i32 {
    unsafe {
        if out.is_null() {
            return E_NOINTERFACE;
        }
        *out = null_mut();
        let id = *iid;
        let target = if id == IID_UNKNOWN || id == IID_SIMPLE {
            SIMPLE.load(Ordering::Relaxed)
        } else if id == IID_TEXT {
            TEXT_PROVIDER.load(Ordering::Relaxed)
        } else {
            0
        };
        if target == 0 {
            E_NOINTERFACE
        } else {
            *out = target as *mut c_void;
            let _ = this;
            S_OK
        }
    }
}
unsafe extern "system" fn range_qi(
    this: *mut c_void,
    iid: *const Guid,
    out: *mut *mut c_void,
) -> i32 {
    unsafe {
        if out.is_null() {
            return E_NOINTERFACE;
        }
        *out = null_mut();
        if *iid == IID_UNKNOWN || *iid == IID_RANGE {
            *out = this;
            S_OK
        } else {
            E_NOINTERFACE
        }
    }
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
unsafe extern "system" fn pattern(_: *mut c_void, id: i32, out: *mut *mut c_void) -> i32 {
    unsafe {
        *out = if id == 10014 {
            TEXT_PROVIDER.load(Ordering::Relaxed) as *mut c_void
        } else {
            null_mut()
        };
        S_OK
    }
}
unsafe extern "system" fn property(_: *mut c_void, id: i32, out: *mut Variant) -> i32 {
    unsafe {
        let v = &mut *out;
        *v = std::mem::zeroed();
        match id {
            30003 => {
                v.vt = 3;
                v.data = 50004
            }
            30005 => {
                v.vt = 8;
                v.data = bstr("UIA trial text") as u64
            }
            30017 => {
                v.vt = 11;
                v.data = 1
            }
            30020 => {
                v.vt = 3;
                v.data = 0
            }
            _ => {}
        }
        S_OK
    }
}
unsafe extern "system" fn options(_: *mut c_void, out: *mut i32) -> i32 {
    unsafe {
        *out = 1;
        S_OK
    }
}
unsafe extern "system" fn host(_: *mut c_void, out: *mut *mut c_void) -> i32 {
    unsafe {
        *out = null_mut();
        S_OK
    }
}
unsafe fn bstr(s: &str) -> *mut u16 {
    unsafe {
        let v: Vec<u16> = s.encode_utf16().collect();
        SysAllocStringLen(v.as_ptr(), v.len() as u32)
    }
}
unsafe fn array_one(item: *mut c_void) -> *mut c_void {
    unsafe {
        let a = SafeArrayCreateVector(13, 0, 1);
        if a.is_null() {
            eprintln!("SafeArrayCreateVector failed");
            return null_mut();
        }
        let hr = SafeArrayPutElement(a, &0, item);
        if hr < 0 {
            eprintln!("SafeArrayPutElement failed: 0x{:08x}", hr as u32);
            SafeArrayDestroy(a);
            return null_mut();
        }
        a
    }
}
unsafe fn range(start: i32, end: i32) -> *mut c_void {
    let vt = range_vtable();
    Box::into_raw(Box::new(Range { vt, start, end })).cast()
}
unsafe extern "system" fn get_selection(_: *mut c_void, out: *mut *mut c_void) -> i32 {
    unsafe {
        *out = array_one(range(0, SELECTION.load(Ordering::Relaxed) as i32));
        if (*out).is_null() { E_FAIL } else { S_OK }
    }
}
unsafe extern "system" fn get_visible(_: *mut c_void, out: *mut *mut c_void) -> i32 {
    unsafe {
        *out = array_one(range(0, TEXT.encode_utf16().count() as i32));
        if (*out).is_null() { E_FAIL } else { S_OK }
    }
}
unsafe extern "system" fn get_document(_: *mut c_void, out: *mut *mut c_void) -> i32 {
    unsafe {
        *out = range(0, TEXT.encode_utf16().count() as i32);
        S_OK
    }
}
unsafe extern "system" fn supported(_: *mut c_void, out: *mut i32) -> i32 {
    unsafe {
        *out = 1;
        S_OK
    }
}
unsafe extern "system" fn clone_range(this: *mut c_void, out: *mut *mut c_void) -> i32 {
    unsafe {
        let r = &*(this as *mut Range);
        *out = range(r.start, r.end);
        S_OK
    }
}
unsafe extern "system" fn compare(this: *mut c_void, other: *mut c_void, out: *mut i32) -> i32 {
    unsafe {
        let a = &*(this as *mut Range);
        let b = &*(other as *mut Range);
        *out = i32::from(a.start == b.start && a.end == b.end);
        S_OK
    }
}
unsafe extern "system" fn compare_ends(
    this: *mut c_void,
    endpoint: i32,
    other: *mut c_void,
    other_end: i32,
    out: *mut i32,
) -> i32 {
    unsafe {
        let a = &*(this as *mut Range);
        let b = &*(other as *mut Range);
        *out = (if endpoint == 0 { a.start } else { a.end })
            - (if other_end == 0 { b.start } else { b.end });
        S_OK
    }
}
unsafe extern "system" fn expand(this: *mut c_void, unit: i32) -> i32 {
    unsafe {
        let r = &mut *(this as *mut Range);
        if unit >= 2 {
            r.start = 0;
            r.end = TEXT.encode_utf16().count() as i32;
        } else {
            r.end = (r.start + 1).min(TEXT.encode_utf16().count() as i32);
        }
        S_OK
    }
}
unsafe extern "system" fn get_text(this: *mut c_void, max: i32, out: *mut *mut u16) -> i32 {
    unsafe {
        let r = &*(this as *mut Range);
        let utf: Vec<u16> = TEXT.encode_utf16().collect();
        let a = r.start.clamp(0, utf.len() as i32) as usize;
        let b = r.end.clamp(a as i32, utf.len() as i32) as usize;
        let n = if max < 0 {
            b - a
        } else {
            (b - a).min(max as usize)
        };
        *out = SysAllocStringLen(utf[a..a + n].as_ptr(), n as u32);
        S_OK
    }
}
unsafe extern "system" fn select(this: *mut c_void) -> i32 {
    unsafe {
        let r = &*(this as *mut Range);
        SELECTION.store(r.end as usize, Ordering::Relaxed);
        S_OK
    }
}
unsafe extern "system" fn move_range(
    this: *mut c_void,
    unit: i32,
    count: i32,
    out: *mut i32,
) -> i32 {
    unsafe {
        let r = &mut *(this as *mut Range);
        let _ = unit;
        let old = r.start;
        r.start = (r.start + count).clamp(0, TEXT.encode_utf16().count() as i32);
        r.end = r.start;
        *out = r.start - old;
        S_OK
    }
}
unsafe extern "system" fn move_end(
    this: *mut c_void,
    which: i32,
    unit: i32,
    count: i32,
    out: *mut i32,
) -> i32 {
    unsafe {
        let r = &mut *(this as *mut Range);
        let _ = unit;
        let p = if which == 0 { &mut r.start } else { &mut r.end };
        let old = *p;
        *p = (*p + count).clamp(0, TEXT.encode_utf16().count() as i32);
        *out = *p - old;
        S_OK
    }
}
unsafe extern "system" fn move_to(
    this: *mut c_void,
    which: i32,
    other: *mut c_void,
    other_end: i32,
) -> i32 {
    unsafe {
        let a = &mut *(this as *mut Range);
        let b = &*(other as *mut Range);
        let v = if other_end == 0 { b.start } else { b.end };
        if which == 0 {
            a.start = v
        } else {
            a.end = v
        }
        S_OK
    }
}
unsafe extern "system" fn empty_array(_: *mut c_void, out: *mut *mut c_void) -> i32 {
    unsafe {
        *out = SafeArrayCreateVector(13, 0, 0);
        S_OK
    }
}
fn range_vtable() -> *const usize {
    use std::sync::OnceLock;
    static VT: OnceLock<Box<[usize]>> = OnceLock::new();
    VT.get_or_init(|| {
        vec![
            range_qi as usize,
            addref as usize,
            release as usize,
            clone_range as usize,
            compare as usize,
            compare_ends as usize,
            expand as usize,
            stub as usize,
            stub as usize,
            stub as usize,
            stub as usize,
            stub as usize,
            get_text as usize,
            move_range as usize,
            move_end as usize,
            move_to as usize,
            select as usize,
            select as usize,
            stub as usize,
            stub as usize,
            empty_array as usize,
        ]
        .into_boxed_slice()
    })
    .as_ptr()
}
fn init() {
    use std::sync::OnceLock;
    static VTS: OnceLock<Box<[usize]>> = OnceLock::new();
    static VTT: OnceLock<Box<[usize]>> = OnceLock::new();
    let a = VTS.get_or_init(|| {
        vec![
            qi as usize,
            addref as usize,
            release as usize,
            options as usize,
            pattern as usize,
            property as usize,
            host as usize,
        ]
        .into_boxed_slice()
    });
    let b = VTT.get_or_init(|| {
        vec![
            qi as usize,
            addref as usize,
            release as usize,
            get_selection as usize,
            get_visible as usize,
            stub as usize,
            stub as usize,
            get_document as usize,
            supported as usize,
        ]
        .into_boxed_slice()
    });
    SIMPLE.store(
        Box::into_raw(Box::new(Obj { vt: a.as_ptr() })) as usize,
        Ordering::Relaxed,
    );
    TEXT_PROVIDER.store(
        Box::into_raw(Box::new(Obj { vt: b.as_ptr() })) as usize,
        Ordering::Relaxed,
    );
}
unsafe extern "system" fn proc(hwnd: Hwnd, msg: u32, w: usize, l: isize) -> isize {
    unsafe {
        match msg {
            WM_GETOBJECT => {
                if l == -25 {
                    UiaReturnRawElementProvider(
                        hwnd,
                        w,
                        l,
                        SIMPLE.load(Ordering::Relaxed) as *mut c_void,
                    )
                } else {
                    DefWindowProcW(hwnd, msg, w, l)
                }
            }
            WM_PAINT => {
                let mut p = std::mem::zeroed::<Paint>();
                let dc = BeginPaint(hwnd, &mut p);
                let mut r = Rect::default();
                GetClientRect(hwnd, &mut r);
                fill(dc, r, rgb(255, 255, 255));
                fill(
                    dc,
                    Rect {
                        left: 20,
                        top: 45,
                        right: 115,
                        bottom: 77,
                    },
                    rgb(170, 205, 255),
                );
                text(
                    dc,
                    TEXT,
                    Rect {
                        left: 20,
                        top: 45,
                        right: r.right - 20,
                        bottom: 80,
                    },
                    rgb(0, 0, 0),
                    DT_SINGLELINE,
                );
                text(
                    dc,
                    "Selected: QuickEdit",
                    Rect {
                        left: 20,
                        top: 100,
                        right: r.right - 20,
                        bottom: 135,
                    },
                    rgb(60, 60, 60),
                    DT_SINGLELINE,
                );
                EndPaint(hwnd, &p);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}
unsafe fn method<T: Copy>(obj: *mut c_void, index: usize) -> T {
    unsafe {
        let vt = *(obj as *mut *const usize);
        std::mem::transmute_copy(&*vt.add(index))
    }
}
unsafe fn release_com(p: *mut c_void) {
    if !p.is_null() {
        unsafe {
            let f: unsafe extern "system" fn(*mut c_void) -> u32 = method(p, 2);
            f(p);
        }
    }
}
fn self_check(hwnd: Hwnd) -> Result<(), String> {
    let handle = hwnd as usize;
    let worker = std::thread::spawn(move || unsafe {
        let hr = CoInitializeEx(null_mut(), 0);
        ok(hr, "CoInitializeEx")?;
        let mut automation = null_mut();
        ok(
            CoCreateInstance(
                &CLSID_UIAUTOMATION,
                null_mut(),
                1,
                &IID_UIAUTOMATION,
                &mut automation,
            ),
            "CoCreateInstance(UIAutomation)",
        )?;
        let element_from: unsafe extern "system" fn(*mut c_void, Hwnd, *mut *mut c_void) -> i32 =
            method(automation, 6);
        let mut element = null_mut();
        ok(
            element_from(automation, handle as Hwnd, &mut element),
            "ElementFromHandle",
        )?;
        let get_pattern: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> i32 =
            method(element, 16);
        let mut provider = null_mut();
        ok(
            get_pattern(element, 10014, &mut provider),
            "GetCurrentPattern(Text)",
        )?;
        if provider.is_null() {
            return Err("Text pattern is null".into());
        }
        let doc: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 =
            method(provider, 7);
        let mut range_ptr = null_mut();
        ok(doc(provider, &mut range_ptr), "DocumentRange")?;
        let get: unsafe extern "system" fn(*mut c_void, i32, *mut *mut u16) -> i32 =
            method(range_ptr, 12);
        let mut b = null_mut();
        ok(get(range_ptr, -1, &mut b), "GetText")?;
        let len = if b.is_null() {
            0
        } else {
            *((b as *const u32).sub(1)) as usize / 2
        };
        let actual = String::from_utf16_lossy(std::slice::from_raw_parts(b, len));
        SysFreeString(b);
        let selection: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 =
            method(provider, 5);
        let mut arr = null_mut();
        ok(selection(provider, &mut arr), "GetSelection")?;
        let length: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32 = method(arr, 3);
        let mut count = 0;
        ok(length(arr, &mut count), "selection Length")?;
        let mut selected = String::new();
        if count > 0 {
            let element: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> i32 =
                method(arr, 4);
            let mut item = null_mut();
            ok(element(arr, 0, &mut item), "selection GetElement")?;
            let get: unsafe extern "system" fn(*mut c_void, i32, *mut *mut u16) -> i32 =
                method(item, 12);
            let mut b = null_mut();
            ok(get(item, -1, &mut b), "selection GetText")?;
            let len = if b.is_null() {
                0
            } else {
                *((b as *const u32).sub(1)) as usize / 2
            };
            selected = String::from_utf16_lossy(std::slice::from_raw_parts(b, len));
            SysFreeString(b);
            release_com(item);
        }
        release_com(arr);
        release_com(range_ptr);
        release_com(provider);
        release_com(element);
        release_com(automation);
        CoUninitialize();
        if actual == TEXT && selected == "QuickEdit" {
            Ok(())
        } else {
            Err(format!("text={actual:?}, selection={selected:?}"))
        }
    });
    unsafe {
        let mut msg = std::mem::zeroed::<Msg>();
        while !worker.is_finished() {
            while PeekMessageW(&mut msg, null_mut(), 0, 0, 1) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    let result = worker
        .join()
        .map_err(|_| "self-check thread panicked".to_string())?;
    unsafe {
        DestroyWindow(hwnd);
    }
    result
}
pub fn run(check: bool) -> Result<(), String> {
    unsafe {
        init();
        let hwnd = make_window("SpikeUIA", "UIA trial", proc, 650, 300)?;
        if check {
            match self_check(hwnd) {
                Ok(()) => {
                    println!("PASS: UIA text and selection");
                    Ok(())
                }
                Err(e) => {
                    println!("FAIL: {e}");
                    Err(e)
                }
            }
        } else {
            loop_messages();
            Ok(())
        }
    }
}
