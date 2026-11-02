//! Windows declarations for `qe-perf`.
//!
//! No Direct3D, DXGI or DWM DLL is linked here. They load only in typing and
//! dup-check runs. If a launch run held `d3d11.dll` in memory, a cold launch of
//! an arm would find its pages warm, and the cold numbers would be false.
use std::{ffi::c_void, ptr};

pub type H = *mut c_void;

#[repr(C)]
pub struct Guid {
    pub a: u32,
    pub b: u16,
    pub c: u16,
    pub d: [u8; 8],
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct WinPoint {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
pub struct StartupInfo {
    pub cb: u32,
    pub reserved: *mut u16,
    pub desktop: *mut u16,
    pub title: *mut u16,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub chars_x: u32,
    pub chars_y: u32,
    pub fill: u32,
    pub flags: u32,
    pub show: u16,
    pub reserved2: u16,
    pub reserved_ptr: *mut u8,
    pub stdin: H,
    pub stdout: H,
    pub stderr: H,
}

#[repr(C)]
pub struct ProcessInfo {
    pub process: H,
    pub thread: H,
    pub pid: u32,
    pub tid: u32,
}

/// An INPUT with its keyboard member. The union starts at offset 8, and its
/// largest member (mouse) makes the whole struct 40 bytes on x64.
#[repr(C)]
pub struct KeyInput {
    pub kind: u32,
    pub padding: u32,
    pub vk: u16,
    pub scan: u16,
    pub flags: u32,
    pub time: u32,
    pub extra: usize,
    pub union_tail: [u8; 8],
}

/// TOKEN_PRIVILEGES with one entry.
#[repr(C)]
pub struct TokenPrivileges {
    pub count: u32,
    pub luid_low: u32,
    pub luid_high: i32,
    pub attributes: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn QueryPerformanceCounter(t: *mut i64) -> i32;
    pub fn QueryPerformanceFrequency(t: *mut i64) -> i32;
    pub fn CreateProcessW(
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
    pub fn CloseHandle(h: H) -> i32;
    pub fn WaitForSingleObject(h: H, ms: u32) -> u32;
    pub fn TerminateProcess(h: H, exit: u32) -> i32;
    pub fn GetLastError() -> u32;
    pub fn WaitForInputIdle(h: H, ms: u32) -> u32;
    pub fn GetExitCodeProcess(h: H, code: *mut u32) -> i32;
    pub fn GetCurrentProcess() -> H;
    pub fn LoadLibraryExW(name: *const u16, file: H, flags: u32) -> H;
    pub fn GetProcAddress(module: H, name: *const u8) -> H;
}

#[link(name = "user32")]
unsafe extern "system" {
    pub fn EnumWindows(callback: unsafe extern "system" fn(H, isize) -> i32, param: isize) -> i32;
    pub fn GetWindowThreadProcessId(h: H, pid: *mut u32) -> u32;
    pub fn IsWindowVisible(h: H) -> i32;
    pub fn IsWindow(h: H) -> i32;
    pub fn IsIconic(h: H) -> i32;
    pub fn GetWindowRect(h: H, r: *mut Rect) -> i32;
    pub fn GetClientRect(h: H, r: *mut Rect) -> i32;
    pub fn ClientToScreen(h: H, p: *mut WinPoint) -> i32;
    pub fn GetForegroundWindow() -> H;
    pub fn SetForegroundWindow(h: H) -> i32;
    pub fn GetWindow(h: H, cmd: u32) -> H;
    pub fn GetClassNameW(h: H, text: *mut u16, max: i32) -> i32;
    pub fn GetAsyncKeyState(vk: i32) -> i16;
    pub fn GetSystemMetrics(index: i32) -> i32;
    pub fn SendInput(count: u32, inputs: *const KeyInput, size: i32) -> u32;
    pub fn SetProcessDpiAwarenessContext(v: isize) -> i32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    pub fn NtSetSystemInformation(class: u32, data: *const u32, len: u32) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    pub fn OpenProcessToken(process: H, access: u32, token: *mut H) -> i32;
    pub fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut [u32; 2]) -> i32;
    pub fn AdjustTokenPrivileges(
        token: H,
        disable_all: i32,
        new: *const TokenPrivileges,
        size: u32,
        old: H,
        needed: *mut u32,
    ) -> i32;
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

pub fn qpc() -> i64 {
    let mut n = 0;
    unsafe { QueryPerformanceCounter(&mut n) };
    n
}

pub fn frequency() -> i64 {
    let mut n = 0;
    unsafe { QueryPerformanceFrequency(&mut n) };
    n
}

/// Finds a function in a DLL from System32, loading the DLL on first use.
/// `name` must end with a NUL byte.
pub fn system_proc(dll: &str, name: &'static [u8]) -> Result<H, String> {
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
    let module = unsafe {
        LoadLibraryExW(
            wide(dll).as_ptr(),
            ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        return Err(format!("loading {dll} failed: error {}", unsafe {
            GetLastError()
        }));
    }
    let proc = unsafe { GetProcAddress(module, name.as_ptr()) };
    if proc.is_null() {
        return Err(format!("{dll} has no such function: error {}", unsafe {
            GetLastError()
        }));
    }
    Ok(proc)
}

pub fn method(p: H, index: usize) -> H {
    unsafe { *(*(p as *const *const H)).add(index) }
}

/// Calls a COM method by its vtable index.
#[macro_export]
macro_rules! vcall {
    ($obj:expr, $index:expr, fn($($t:ty),*) -> $r:ty $(, $a:expr)* $(,)?) => {{
        let this: $crate::win::H = $obj;
        let f: unsafe extern "system" fn($crate::win::H $(, $t)*) -> $r =
            unsafe { ::std::mem::transmute($crate::win::method(this, $index)) };
        unsafe { f(this $(, $a)*) }
    }};
}

pub fn check(hr: i32, what: &str) -> Result<(), String> {
    if hr < 0 {
        Err(format!("{what} failed: HRESULT {hr:#010x}"))
    } else {
        Ok(())
    }
}

/// An owned COM pointer.
pub struct Com(pub H);
impl Com {
    pub fn null() -> Self {
        Com(ptr::null_mut())
    }
    pub fn slot(&mut self) -> *mut H {
        &mut self.0
    }
    pub fn query(&self, iid: &Guid, what: &str) -> Result<Com, String> {
        let mut out = Com::null();
        let hr = vcall!(self.0, 0, fn(*const Guid, *mut H) -> i32, iid, out.slot());
        check(hr, what)?;
        Ok(out)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        if !self.0.is_null() {
            vcall!(self.0, 2, fn() -> u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_struct_has_the_size_and_layout_sendinput_expects() {
        assert_eq!(size_of::<KeyInput>(), 40);
        assert_eq!(std::mem::offset_of!(KeyInput, vk), 8);
        assert_eq!(std::mem::offset_of!(KeyInput, extra), 24);
    }
}
