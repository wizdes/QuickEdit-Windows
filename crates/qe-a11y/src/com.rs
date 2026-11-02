//! Small COM helpers: the errors UIA expects, and SAFEARRAY building.
use std::ffi::c_void;
use windows::{
    Win32::{
        Foundation::{E_INVALIDARG, HWND, POINT},
        Graphics::Gdi::ClientToScreen,
        System::{
            Com::SAFEARRAY,
            Ole::{SafeArrayCreateVector, SafeArrayDestroy, SafeArrayPutElement},
            Variant::{VARIANT, VT_I4, VT_R8, VT_UNKNOWN},
        },
        UI::{Accessibility::*, HiDpi::GetDpiForWindow},
    },
    core::{Error, HRESULT, Interface, Result},
};

use crate::text::ReadError;

/// The object is gone: closed, replaced, or from an older document.
pub(crate) fn gone() -> Error {
    Error::new(
        HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32),
        "This element is no longer available.",
    )
}
/// The answer is not known yet. The same call can work later.
pub(crate) fn not_yet(what: &str) -> Error {
    Error::new(HRESULT(UIA_E_TIMEOUT as i32), what)
}
pub(crate) fn invalid(what: &str) -> Error {
    Error::new(E_INVALIDARG, what)
}
pub(crate) fn not_supported(what: &str) -> Error {
    Error::new(HRESULT(UIA_E_NOTSUPPORTED as i32), what)
}
pub(crate) fn not_enabled(what: &str) -> Error {
    Error::new(HRESULT(UIA_E_ELEMENTNOTENABLED as i32), what)
}
/// S_OK with a null result, which is how UIA says "none". The generated glue turns this
/// empty error into S_OK and leaves the out pointer alone. Every caller of these methods,
/// UIA's own stubs included, starts that pointer at null.
pub(crate) fn none() -> Error {
    Error::empty()
}
pub(crate) fn read_error(error: ReadError) -> Error {
    match error {
        ReadError::NotYetKnown => not_yet("This text is not in memory yet. Ask again soon."),
        ReadError::OutOfRange => invalid("The text range is outside this document."),
        ReadError::Failed(message) => Error::new(HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32), message),
    }
}

pub(crate) fn not_supported_value() -> Result<VARIANT> {
    let value = unsafe { UiaGetReservedNotSupportedValue() }?;
    Ok(VARIANT::from(value))
}

fn check(status: Result<()>, array: *mut SAFEARRAY) -> Result<()> {
    if status.is_err() {
        unsafe { SafeArrayDestroy(array) }?;
    }
    status
}

/// A SAFEARRAY of COM pointers. Each element takes its own reference.
pub(crate) fn interface_array<T: Interface>(items: &[T]) -> Result<*mut SAFEARRAY> {
    let count = u32::try_from(items.len()).map_err(|_| invalid("Too many items."))?;
    let array = unsafe { SafeArrayCreateVector(VT_UNKNOWN, 0, count) };
    if array.is_null() {
        return Err(gone());
    }
    for (index, item) in items.iter().enumerate() {
        let at = i32::try_from(index).map_err(|_| invalid("Too many items."))?;
        let status = unsafe { SafeArrayPutElement(array, &at, item.as_raw()) };
        check(status, array)?;
    }
    Ok(array)
}

/// Left, top, width and height of each rectangle, in screen pixels.
pub(crate) fn rectangle_array(rectangles: &[[f64; 4]]) -> Result<*mut SAFEARRAY> {
    let count = rectangles
        .len()
        .checked_mul(4)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| invalid("Too many rectangles."))?;
    let array = unsafe { SafeArrayCreateVector(VT_R8, 0, count) };
    if array.is_null() {
        return Err(gone());
    }
    for (index, value) in rectangles.iter().flatten().enumerate() {
        let at = i32::try_from(index).map_err(|_| invalid("Too many rectangles."))?;
        let status =
            unsafe { SafeArrayPutElement(array, &at, std::ptr::from_ref(value).cast::<c_void>()) };
        check(status, array)?;
    }
    Ok(array)
}

pub(crate) fn int_array(values: &[i32]) -> Result<*mut SAFEARRAY> {
    let count = u32::try_from(values.len()).map_err(|_| invalid("Too many items."))?;
    let array = unsafe { SafeArrayCreateVector(VT_I4, 0, count) };
    if array.is_null() {
        return Err(gone());
    }
    for (index, value) in values.iter().enumerate() {
        let at = i32::try_from(index).map_err(|_| invalid("Too many items."))?;
        let status =
            unsafe { SafeArrayPutElement(array, &at, std::ptr::from_ref(value).cast::<c_void>()) };
        check(status, array)?;
    }
    Ok(array)
}

fn hwnd_of(window: isize) -> HWND {
    HWND(window as *mut c_void)
}

fn scale_of(window: isize) -> Result<f64> {
    let dpi = unsafe { GetDpiForWindow(hwnd_of(window)) };
    if dpi == 0 {
        return Err(gone());
    }
    Ok(f64::from(dpi) / 96.0)
}

/// Window coordinates in device-independent pixels to screen pixels.
pub(crate) fn to_screen(window: isize, rect: qe_api::Rect) -> Result<[f64; 4]> {
    let scale = scale_of(window)?;
    let mut origin = POINT::default();
    if !unsafe { ClientToScreen(hwnd_of(window), &mut origin) }.as_bool() {
        return Err(gone());
    }
    Ok([
        f64::from(origin.x) + f64::from(rect.x.0) * scale,
        f64::from(origin.y) + f64::from(rect.y.0) * scale,
        f64::from(rect.width.0) * scale,
        f64::from(rect.height.0) * scale,
    ])
}

/// Screen pixels to window coordinates in device-independent pixels.
pub(crate) fn to_window(window: isize, x: f64, y: f64) -> Result<(f32, f32)> {
    let scale = scale_of(window)?;
    let mut origin = POINT::default();
    if !unsafe { ClientToScreen(hwnd_of(window), &mut origin) }.as_bool() {
        return Err(gone());
    }
    Ok((
        ((x - f64::from(origin.x)) / scale) as f32,
        ((y - f64::from(origin.y)) / scale) as f32,
    ))
}
