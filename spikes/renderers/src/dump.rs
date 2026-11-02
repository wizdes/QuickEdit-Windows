//! Proof tools behind `--dump-frame FILE`: saving what an arm drew and which
//! DLLs were loaded. The launch and typing timers never pass this flag, because
//! the copy adds time before the stamp.
//!
//! `dwm_flush` also lives here: it is a measurement tool, not part of any arm.
use crate::ffi::{self, H};
use std::{fs, mem, ptr};

/// Writes a top-down 32-bit BGRA bitmap file.
pub fn write_bmp(
    path: &str,
    width: u32,
    height: u32,
    pitch: u32,
    data: &[u8],
) -> Result<(), String> {
    let row = width as usize * 4;
    let image_bytes = row * height as usize;
    let mut out = Vec::with_capacity(54 + image_bytes);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + image_bytes as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(-(height as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for y in 0..height as usize {
        let start = y * pitch as usize;
        out.extend_from_slice(&data[start..start + row]);
    }
    fs::write(path, out).map_err(|e| format!("dump {path}: {e}"))
}

/// Lists the loaded DLLs, so a run can prove which ones the first frame needed.
pub fn write_modules(path: &str) -> Result<(), String> {
    let process = unsafe { ffi::GetCurrentProcess() };
    let mut modules: Vec<H> = vec![ptr::null_mut(); 512];
    let mut needed = 0;
    let bytes = (modules.len() * size_of::<H>()) as u32;
    if unsafe { ffi::K32EnumProcessModules(process, modules.as_mut_ptr(), bytes, &mut needed) } == 0
    {
        return Err("K32EnumProcessModules failed".into());
    }
    let count = (needed as usize / size_of::<H>()).min(modules.len());
    let mut lines = Vec::with_capacity(count);
    for module in &modules[..count] {
        let mut name = [0u16; 260];
        let len = unsafe { ffi::K32GetModuleBaseNameW(process, *module, name.as_mut_ptr(), 260) };
        lines.push(String::from_utf16_lossy(&name[..len as usize]));
    }
    fs::write(path, lines.join("\n")).map_err(|e| format!("modules {path}: {e}"))
}

pub fn dwm_flush() -> Result<(), String> {
    let flush: unsafe extern "system" fn() -> i32 =
        unsafe { mem::transmute(ffi::system_proc("dwmapi.dll", b"DwmFlush\0")?) };
    ffi::check(unsafe { flush() }, "DwmFlush")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_file_drops_row_padding() {
        let path = std::env::temp_dir().join(format!("qe-dump-test-{}.bmp", std::process::id()));
        let path = path.to_string_lossy().into_owned();
        // Two rows of one pixel, with four bytes of padding on each row.
        let data = [1, 2, 3, 4, 0, 0, 0, 0, 5, 6, 7, 8, 0, 0, 0, 0];
        write_bmp(&path, 1, 2, 8, &data).unwrap();
        let written = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(written.len(), 54 + 8);
        assert_eq!(&written[54..], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
