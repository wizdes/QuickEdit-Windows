//! The plain font chooser: the Windows system font dialog, as with the file dialogs.
//! That dialog has no dark mode, so it stays light in a dark app.
//!
//! `comdlg32` loads only when the Change button runs the dialog, because a direct import
//! would load it at launch (design-spec section 4, rule 6).
use crate::FontChoice;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, LOGFONTW, LOGPIXELSY, ReleaseDC};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::Dialogs::{
    CF_FORCEFONTEXIST, CF_INITTOLOGFONTSTRUCT, CF_NOSCRIPTSEL, CF_NOSTYLESEL, CF_NOVERTFONTS,
    CF_SCREENFONTS, CHOOSEFONT_FLAGS, CHOOSEFONTW,
};
use windows::core::{BOOL, PCSTR, PCWSTR, s, w};

/// A function from a DLL that loads on this first use. The DLL stays loaded for the rest
/// of the process, so the address stays good.
pub(crate) fn late_function(dll: PCWSTR, name: PCSTR) -> Result<LateFunction, String> {
    // SAFETY: both names are static strings with a terminating zero.
    unsafe {
        let module = LoadLibraryW(dll).map_err(|e| e.to_string())?;
        GetProcAddress(module, name).ok_or_else(|| {
            format!(
                "{} has no function {}",
                dll.to_string().unwrap_or_default(),
                name.to_string().unwrap_or_default()
            )
        })
    }
}
pub(crate) type LateFunction = unsafe extern "system" fn() -> isize;
type ChooseFontCall = unsafe extern "system" fn(*mut CHOOSEFONTW) -> BOOL;
type DialogErrorCall = unsafe extern "system" fn() -> u32;

/// The dialog's flags: every screen font, with no script, style or vertical choices,
/// because a plain font is a family and a size.
pub fn font_dialog_flags() -> CHOOSEFONT_FLAGS {
    CF_INITTOLOGFONTSTRUCT
        | CF_SCREENFONTS
        | CF_NOSCRIPTSEL
        | CF_NOSTYLESEL
        | CF_NOVERTFONTS
        | CF_FORCEFONTEXIST
}
/// The dialog's first selection. `pixels_per_inch` turns points into the font height
/// that `LOGFONTW` holds. A name too long for its field is cut, and the dialog then
/// shows the nearest font.
pub fn font_dialog_start(current: &FontChoice, pixels_per_inch: i32) -> LOGFONTW {
    let mut font = LOGFONTW {
        lfHeight: -((current.points * pixels_per_inch as f32 / 72.0).round() as i32),
        ..Default::default()
    };
    let last = font.lfFaceName.len() - 1;
    for (slot, unit) in font.lfFaceName[..last]
        .iter_mut()
        .zip(current.family.encode_utf16())
    {
        *slot = unit;
    }
    font
}
/// Reads what the dialog chose. `tenths` is its size field, in tenths of a point.
pub fn font_dialog_result(font: &LOGFONTW, tenths: i32) -> Result<FontChoice, String> {
    let end = font
        .lfFaceName
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(font.lfFaceName.len());
    let family = String::from_utf16(&font.lfFaceName[..end])
        .map_err(|e| format!("The font dialog gave a name that is not valid: {e}"))?;
    if family.is_empty() || tenths <= 0 {
        return Err("The font dialog gave no font name or size.".into());
    }
    Ok(FontChoice {
        family,
        points: tenths as f32 / 10.0,
    })
}
fn screen_pixels_per_inch() -> i32 {
    // SAFETY: a null window asks for the screen's device context, released below.
    unsafe {
        let screen = GetDC(None);
        let value = GetDeviceCaps(Some(screen), LOGPIXELSY);
        ReleaseDC(None, screen);
        if value > 0 { value } else { 96 }
    }
}
/// Shows the dialog and waits for it. `Ok(None)` means the user cancelled. This runs
/// only from a press of the Change button, so nothing here loads at startup.
pub fn choose_plain_font(
    owner: Option<HWND>,
    current: &FontChoice,
) -> Result<Option<FontChoice>, String> {
    let mut font = font_dialog_start(current, screen_pixels_per_inch());
    let mut choose = CHOOSEFONTW {
        lStructSize: std::mem::size_of::<CHOOSEFONTW>() as u32,
        hwndOwner: owner.unwrap_or_default(),
        lpLogFont: &mut font,
        Flags: font_dialog_flags(),
        ..Default::default()
    };
    let failed = |why: String| format!("The font dialog could not open: {why}");
    let choose_font = late_function(w!("comdlg32.dll"), s!("ChooseFontW")).map_err(failed)?;
    let last_error =
        late_function(w!("comdlg32.dll"), s!("CommDlgExtendedError")).map_err(failed)?;
    // SAFETY: these are the signatures that `commdlg.h` gives these names.
    let (choose_font, last_error) = unsafe {
        (
            std::mem::transmute::<LateFunction, ChooseFontCall>(choose_font),
            std::mem::transmute::<LateFunction, DialogErrorCall>(last_error),
        )
    };
    // SAFETY: the struct has its size set, and it points at a font that outlives the call.
    if unsafe { choose_font(&mut choose) }.as_bool() {
        return font_dialog_result(&font, choose.iPointSize).map(Some);
    }
    // SAFETY: a plain query about the call above.
    match unsafe { last_error() } {
        0 => Ok(None),
        code => Err(failed(format!("code {code:#x}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_late_functions_exist_and_a_wrong_name_says_so() {
        // Only the look-up runs: no dialog opens in a test.
        assert!(late_function(w!("comdlg32.dll"), s!("ChooseFontW")).is_ok());
        assert!(late_function(w!("comdlg32.dll"), s!("CommDlgExtendedError")).is_ok());
        let missing = late_function(w!("comdlg32.dll"), s!("NoSuchFunction"));
        assert!(missing.unwrap_err().contains("NoSuchFunction"));
    }
    #[test]
    fn the_dialog_shows_screen_fonts_and_no_style_choices() {
        let flags = font_dialog_flags();
        assert_ne!(flags.0 & CF_SCREENFONTS.0, 0);
        assert_ne!(flags.0 & CF_NOSTYLESEL.0, 0);
        assert_ne!(flags.0 & CF_INITTOLOGFONTSTRUCT.0, 0);
    }
}
