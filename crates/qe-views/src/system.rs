//! What the host reads from Windows. Both calls return quickly and read no file.
use crate::page::Paper;
use windows::Win32::Globalization::GetUserDefaultLocaleName;
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};
use windows::core::BOOL;

/// The paper for the user's region. A locale with no region part, or a failed call, gives
/// A4, which is the Mac's answer for any region other than the US and Canada.
pub fn system_paper() -> Paper {
    let mut name = [0u16; 85];
    let length = unsafe { GetUserDefaultLocaleName(&mut name) };
    let Some(text) = (length > 0)
        .then(|| name.get(..length as usize - 1))
        .flatten()
        .map(String::from_utf16_lossy)
    else {
        return Paper::A4;
    };
    Paper::for_region(text.rsplit('-').next().unwrap_or(""))
}

/// True when Windows' "Animation effects" setting is off, which is its Reduce Motion. If
/// the call fails, this says false, and the fade runs as it does by default.
pub fn system_reduce_motion() -> bool {
    let mut animate = BOOL(1);
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&raw mut animate).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    read.is_ok() && !animate.as_bool()
}
