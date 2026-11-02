//! COM providers used by UI Automation. Construct them only for WM_GETOBJECT's
//! `UiaRootObjectId`. The host updates selection after each editor action.
#![allow(non_snake_case, non_upper_case_globals)]
use crate::{EventFlags, EventTracker, ReadError, TextModel, TextRange};
use qe_editor::EditorView;
use std::{
    ffi::c_void,
    mem::ManuallyDrop,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::{
            Com::SAFEARRAY,
            Variant::{VARENUM, VARIANT},
        },
        UI::Accessibility::*,
    },
    core::{
        BOOL, BSTR, ComObject, Error, HRESULT, IUnknown, IUnknownImpl, Interface, Ref, Result,
        implement,
    },
};

const E_NOTIMPL: HRESULT = HRESULT(0x80004001u32 as i32);
const E_INVALIDARG: HRESULT = HRESULT(0x80070057u32 as i32);

fn unavailable() -> Error {
    Error::new(
        HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32),
        "This text or layout is not available yet.",
    )
}
fn unsupported() -> Error {
    Error::new(E_NOTIMPL, "This text operation is not supported.")
}
fn read_error(error: ReadError) -> Error {
    match error {
        ReadError::OutOfRange => {
            Error::new(E_INVALIDARG, "The text range is outside this document.")
        }
        ReadError::TooLong => Error::new(
            HRESULT(UIA_E_TIMEOUT as i32),
            "The requested search is over the 65,536 UTF-16 unit limit.",
        ),
        ReadError::NotYetKnown => Error::new(
            HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32),
            "The mapped text is still being indexed.",
        ),
        ReadError::Failed(message) => {
            Error::new(HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32), message)
        }
    }
}

fn variant_int(n: i32) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt = VARENUM(3);
        (*v.Anonymous.Anonymous).Anonymous.lVal = n;
    }
    v
}
fn variant_bool(b: bool) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt = VARENUM(11);
        (*v.Anonymous.Anonymous).Anonymous.boolVal.0 = if b { -1 } else { 0 };
    }
    v
}
fn variant_string(s: &str) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt = VARENUM(8);
        (*v.Anonymous.Anonymous).Anonymous.bstrVal = ManuallyDrop::new(BSTR::from(s));
    }
    v
}
fn variant_double(n: f64) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        (*v.Anonymous.Anonymous).vt = VARENUM(5);
        (*v.Anonymous.Anonymous).Anonymous.dblVal = n;
    }
    v
}

#[link(name = "oleaut32")]
unsafe extern "system" {
    fn SafeArrayCreateVector(vt: u16, lower: i32, count: u32) -> *mut SAFEARRAY;
    fn SafeArrayPutElement(array: *mut SAFEARRAY, index: *const i32, value: *const c_void) -> i32;
    fn SafeArrayDestroy(array: *mut SAFEARRAY) -> i32;
}
fn range_array(ranges: &[ITextRangeProvider]) -> Result<*mut SAFEARRAY> {
    let count = u32::try_from(ranges.len()).map_err(|_| Error::from_hresult(E_INVALIDARG))?;
    let array = unsafe { SafeArrayCreateVector(13, 0, count) };
    if array.is_null() {
        return Err(unavailable());
    }
    for (index, range) in ranges.iter().enumerate() {
        let at = i32::try_from(index).map_err(|_| Error::from_hresult(E_INVALIDARG))?;
        let status = unsafe { SafeArrayPutElement(array, &at, range.as_raw()) };
        if status < 0 {
            unsafe {
                SafeArrayDestroy(array);
            }
            return Err(Error::from_hresult(HRESULT(status)));
        }
    }
    Ok(array)
}
fn rectangle_array(rectangles: &[[f64; 4]]) -> Result<*mut SAFEARRAY> {
    let count = rectangles
        .len()
        .checked_mul(4)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
    let array = unsafe { SafeArrayCreateVector(5, 0, count) };
    if array.is_null() {
        return Err(unavailable());
    }
    for (index, value) in rectangles.iter().flatten().enumerate() {
        let at = i32::try_from(index).map_err(|_| Error::from_hresult(E_INVALIDARG))?;
        let status = unsafe { SafeArrayPutElement(array, &at, (value as *const f64).cast()) };
        if status < 0 {
            unsafe {
                SafeArrayDestroy(array);
            }
            return Err(Error::from_hresult(HRESULT(status)));
        }
    }
    Ok(array)
}

/// The shell implements this with a UI-thread marshal. Its methods use only
/// `EditorView`'s nonblocking hooks and convert UTF-16 with the public helpers.
pub trait EditorHost: Send + Sync {
    fn visible_range(&self) -> Result<TextRange>;
    fn range_from_point(&self, point: UiaPoint) -> Result<TextRange>;
    fn rectangles(&self, range: TextRange) -> Result<Vec<[f64; 4]>>;
    fn select(&self, range: TextRange) -> Result<()>;
    fn scroll_into_view(&self, range: TextRange, align_top: bool) -> Result<()>;
    fn expand_to_unit(&self, _: TextRange, _: TextUnit) -> Result<TextRange> {
        Err(unsupported())
    }
    fn move_range(&self, _: TextRange, _: TextUnit, _: i32) -> Result<(TextRange, i32)> {
        Err(unsupported())
    }
    fn move_endpoint(
        &self,
        _: TextRange,
        _: TextPatternRangeEndpoint,
        _: TextUnit,
        _: i32,
    ) -> Result<(TextRange, i32)> {
        Err(unsupported())
    }
}

#[derive(Clone)]
struct Shared {
    model: Arc<Mutex<TextModel>>,
    selection: Arc<Mutex<TextRange>>,
    select: Option<Arc<dyn Fn(TextRange) -> Result<()> + Send + Sync>>,
    editor_host: Option<Arc<dyn EditorHost>>,
    read_only: Arc<AtomicBool>,
    font: Arc<Mutex<Option<(String, f64)>>>,
}
impl Shared {
    fn model(&self) -> Result<TextModel> {
        Ok(self.model.lock().map_err(|_| unavailable())?.clone())
    }
    fn check_range(&self, range: TextRange) -> Result<TextRange> {
        let end = self.model()?.len_utf16().map_err(read_error)?;
        if range.start > range.end || range.end > end {
            Err(Error::from_hresult(E_INVALIDARG))
        } else {
            Ok(range)
        }
    }
}

/// A document provider holds an immutable snapshot. Replace it after an edit.
/// The caller must create the snapshot on the UI thread; all COM reads are bounded.
pub struct DocumentProvider {
    simple: IRawElementProviderSimple,
    shared: Shared,
}

// SAFETY: `simple` is this crate's COM object, whose reference count is atomic.
// Its methods touch only `Shared` data behind Mutex/AtomicBool and Send+Sync
// callbacks. No apartment-bound Windows object is stored in the provider.
unsafe impl Send for DocumentProvider {}
unsafe impl Sync for DocumentProvider {}

impl DocumentProvider {
    /// `read_only` must come from `EditorView::can_edit` on the UI thread.
    /// Reading that state from a mapped snapshot could wait for disk.
    pub fn new(model: TextModel, selection: TextRange, read_only: bool) -> Self {
        let shared = Shared {
            model: Arc::new(Mutex::new(model)),
            selection: Arc::new(Mutex::new(selection)),
            select: None,
            editor_host: None,
            read_only: Arc::new(AtomicBool::new(read_only)),
            font: Arc::new(Mutex::new(None)),
        };
        let simple = ComObject::new(Document {
            shared: shared.clone(),
        })
        .to_interface();
        Self { simple, shared }
    }
    /// Call on the UI thread. The snapshot may then answer later COM calls on
    /// the calling UIA thread, without touching the editor or waiting on disk.
    pub fn from_editor(view: &EditorView) -> std::result::Result<Self, ReadError> {
        let selection = view.selection();
        let start = view.byte_to_utf16(selection.start)?.0;
        let end = view.byte_to_utf16(selection.end)?.0;
        Ok(Self::new(
            TextModel::new(view.worker_snapshot()),
            TextRange::new(start, end),
            !view.can_edit(),
        ))
    }
    pub fn simple(&self) -> &IRawElementProviderSimple {
        &self.simple
    }
    /// The callback must marshal to the UI thread and use `HostCommand::Select`.
    pub fn with_select(
        mut self,
        callback: Arc<dyn Fn(TextRange) -> Result<()> + Send + Sync>,
    ) -> Self {
        self.shared.select = Some(callback);
        self.simple = ComObject::new(Document {
            shared: self.shared.clone(),
        })
        .to_interface();
        self
    }
    pub fn with_editor_host(mut self, host: Arc<dyn EditorHost>) -> Self {
        self.shared.editor_host = Some(host);
        self.simple = ComObject::new(Document {
            shared: self.shared.clone(),
        })
        .to_interface();
        self
    }
    pub fn set_selection(&self, selection: TextRange) -> Result<()> {
        *self.shared.selection.lock().map_err(|_| unavailable())? = selection;
        Ok(())
    }
    /// The shell supplies the actual font after a font or zoom change.
    pub fn set_font(&self, family: impl Into<String>, points: f64) -> Result<()> {
        if !points.is_finite() || points <= 0.0 {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        *self.shared.font.lock().map_err(|_| unavailable())? = Some((family.into(), points));
        Ok(())
    }
    /// Refresh after every edit or document replacement. Old range objects then
    /// see the current document and reject positions that are past its end.
    pub fn refresh_from_editor(&self, view: &EditorView) -> std::result::Result<(), ReadError> {
        let selection = view.selection();
        let start = view.byte_to_utf16(selection.start)?.0;
        let end = view.byte_to_utf16(selection.end)?.0;
        *self
            .shared
            .model
            .lock()
            .map_err(|_| ReadError::Failed("UIA document lock failed".into()))? =
            TextModel::new(view.worker_snapshot());
        *self
            .shared
            .selection
            .lock()
            .map_err(|_| ReadError::Failed("UIA selection lock failed".into()))? =
            TextRange::new(start, end);
        self.shared
            .read_only
            .store(!view.can_edit(), Ordering::Release);
        Ok(())
    }
    /// Call after one UI input batch. This checks for clients before reading
    /// the editor's change log or raising any event.
    pub fn raise_editor_events(
        &self,
        tracker: &mut EventTracker,
        view: &EditorView,
    ) -> Result<EventFlags> {
        let listening = unsafe { UiaClientsAreListening().as_bool() };
        let flags = tracker.poll(view, listening);
        if flags.text_changed {
            unsafe {
                UiaRaiseAutomationEvent(&self.simple, UIA_Text_TextChangedEventId)?;
            }
        }
        if flags.selection_changed {
            unsafe {
                UiaRaiseAutomationEvent(&self.simple, UIA_Text_TextSelectionChangedEventId)?;
            }
        }
        if flags.caret_moved {
            let selection = view.selection();
            let at = view
                .byte_to_utf16(selection.end)
                .map_err(|e| read_error(e.into()))?
                .0;
            let caret = make_range(&self.shared, TextRange::new(at, at), &self.simple);
            unsafe {
                UiaRaiseActiveTextPositionChangedEvent(&self.simple, &caret)?;
            }
        }
        Ok(flags)
    }
}

#[implement(IRawElementProviderSimple, ITextProvider2, IValueProvider)]
struct Document {
    shared: Shared,
}

impl IRawElementProviderSimple_Impl for Document_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }
    fn GetPatternProvider(&self, id: UIA_PATTERN_ID) -> Result<IUnknown> {
        if id == UIA_TextPatternId || id == UIA_TextPattern2Id {
            let provider: ITextProvider2 = self.to_object().to_interface();
            provider.cast()
        } else if id == UIA_ValuePatternId && self.shared.read_only.load(Ordering::Acquire) {
            self.to_object().to_interface::<IValueProvider>().cast()
        } else {
            Err(unsupported())
        }
    }
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        Ok(match id {
            UIA_ControlTypePropertyId => variant_int(UIA_DocumentControlTypeId.0),
            UIA_NamePropertyId => variant_string("Document"),
            UIA_IsEnabledPropertyId => variant_bool(true),
            UIA_IsKeyboardFocusablePropertyId => variant_bool(true),
            _ => VARIANT::default(),
        })
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        Err(unsupported())
    }
}

impl ITextProvider_Impl for Document_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let selection = *self.shared.selection.lock().map_err(|_| unavailable())?;
        range_array(&[make_range(
            &self.shared,
            selection,
            &self.to_object().to_interface(),
        )])
    }
    fn GetVisibleRanges(&self) -> Result<*mut SAFEARRAY> {
        let host = self.shared.editor_host.as_ref().ok_or_else(unavailable)?;
        range_array(&[make_range(
            &self.shared,
            host.visible_range()?,
            &self.to_object().to_interface(),
        )])
    }
    fn RangeFromChild(&self, _: Ref<IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        Err(unsupported())
    }
    fn RangeFromPoint(&self, point: &UiaPoint) -> Result<ITextRangeProvider> {
        let host = self.shared.editor_host.as_ref().ok_or_else(unavailable)?;
        Ok(make_range(
            &self.shared,
            host.range_from_point(*point)?,
            &self.to_object().to_interface(),
        ))
    }
    fn DocumentRange(&self) -> Result<ITextRangeProvider> {
        let end = self.shared.model()?.len_utf16().map_err(read_error)?;
        Ok(make_range(
            &self.shared,
            TextRange::new(0, end),
            &self.to_object().to_interface(),
        ))
    }
    fn SupportedTextSelection(&self) -> Result<SupportedTextSelection> {
        Ok(SupportedTextSelection_Single)
    }
}
impl ITextProvider2_Impl for Document_Impl {
    fn RangeFromAnnotation(&self, _: Ref<IRawElementProviderSimple>) -> Result<ITextRangeProvider> {
        Err(unsupported())
    }
    fn GetCaretRange(&self, active: *mut BOOL) -> Result<ITextRangeProvider> {
        if active.is_null() {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        unsafe {
            *active = BOOL(1);
        }
        let position = self.shared.selection.lock().map_err(|_| unavailable())?.end;
        Ok(make_range(
            &self.shared,
            TextRange::new(position, position),
            &self.to_object().to_interface(),
        ))
    }
}

impl IValueProvider_Impl for Document_Impl {
    fn SetValue(&self, _: &windows::core::PCWSTR) -> Result<()> {
        Err(Error::new(
            HRESULT(UIA_E_ELEMENTNOTENABLED as i32),
            "This document is read only.",
        ))
    }
    fn Value(&self) -> Result<BSTR> {
        let model = self.shared.model()?;
        let end = model.len_utf16().map_err(read_error)?;
        if end > crate::GET_TEXT_CAP {
            return Err(Error::new(
                HRESULT(UIA_E_TIMEOUT as i32),
                "The document is too large for a Value answer.",
            ));
        }
        model
            .read(TextRange::new(0, end), -1)
            .map(|s| BSTR::from(s.as_str()))
            .map_err(read_error)
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        Ok(BOOL(i32::from(
            self.shared.read_only.load(Ordering::Acquire),
        )))
    }
}

fn make_range(
    shared: &Shared,
    value: TextRange,
    enclosing: &IRawElementProviderSimple,
) -> ITextRangeProvider {
    ComObject::new(RangeProvider {
        shared: shared.clone(),
        value: Mutex::new(value),
        enclosing: enclosing.clone(),
    })
    .to_interface()
}

#[implement(ITextRangeProvider)]
struct RangeProvider {
    shared: Shared,
    value: Mutex<TextRange>,
    enclosing: IRawElementProviderSimple,
}

impl ITextRangeProvider_Impl for RangeProvider_Impl {
    fn Clone(&self) -> Result<ITextRangeProvider> {
        Ok(make_range(
            &self.shared,
            *self.value.lock().map_err(|_| unavailable())?,
            &self.enclosing,
        ))
    }
    fn Compare(&self, other: Ref<ITextRangeProvider>) -> Result<BOOL> {
        let target = other.ok()?.cast_object_ref::<RangeProvider>()?;
        if !Arc::ptr_eq(&self.shared.selection, &target.shared.selection) {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        let a = *self.value.lock().map_err(|_| unavailable())?;
        let b = *target.value.lock().map_err(|_| unavailable())?;
        Ok(BOOL(i32::from(a == b)))
    }
    fn CompareEndpoints(
        &self,
        endpoint: TextPatternRangeEndpoint,
        other: Ref<ITextRangeProvider>,
        other_endpoint: TextPatternRangeEndpoint,
    ) -> Result<i32> {
        let target = other.ok()?.cast_object_ref::<RangeProvider>()?;
        if !Arc::ptr_eq(&self.shared.selection, &target.shared.selection) {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        let a = *self.value.lock().map_err(|_| unavailable())?;
        let b = *target.value.lock().map_err(|_| unavailable())?;
        let left = endpoint_value(a, endpoint)?;
        let right = endpoint_value(b, other_endpoint)?;
        let distance = left.abs_diff(right).min(i32::MAX as usize) as i32;
        Ok(if left < right { -distance } else { distance })
    }
    fn ExpandToEnclosingUnit(&self, unit: TextUnit) -> Result<()> {
        if unit != TextUnit_Document {
            let host = self.shared.editor_host.as_ref().ok_or_else(unsupported)?;
            let current = *self.value.lock().map_err(|_| unavailable())?;
            let next = self
                .shared
                .check_range(host.expand_to_unit(current, unit)?)?;
            *self.value.lock().map_err(|_| unavailable())? = next;
            return Ok(());
        }
        let end = self.shared.model()?.len_utf16().map_err(read_error)?;
        *self.value.lock().map_err(|_| unavailable())? = TextRange::new(0, end);
        Ok(())
    }
    fn FindAttribute(
        &self,
        _: UIA_TEXTATTRIBUTE_ID,
        _: &VARIANT,
        _: BOOL,
    ) -> Result<ITextRangeProvider> {
        Err(unsupported())
    }
    fn FindText(
        &self,
        query: &BSTR,
        backward: BOOL,
        ignore_case: BOOL,
    ) -> Result<ITextRangeProvider> {
        let range = *self.value.lock().map_err(|_| unavailable())?;
        let found = self
            .shared
            .model()?
            .find_text(
                range,
                &query.to_string(),
                backward.as_bool(),
                ignore_case.as_bool(),
            )
            .map_err(read_error)?;
        found
            .map(|value| make_range(&self.shared, value, &self.enclosing))
            .ok_or_else(unavailable)
    }
    fn GetAttributeValue(&self, id: UIA_TEXTATTRIBUTE_ID) -> Result<VARIANT> {
        Ok(match id {
            UIA_IsReadOnlyAttributeId => {
                variant_bool(self.shared.read_only.load(Ordering::Acquire))
            }
            UIA_FontNameAttributeId => self
                .shared
                .font
                .lock()
                .map_err(|_| unavailable())?
                .as_ref()
                .map_or_else(VARIANT::default, |f| variant_string(&f.0)),
            UIA_FontSizeAttributeId => self
                .shared
                .font
                .lock()
                .map_err(|_| unavailable())?
                .as_ref()
                .map_or_else(VARIANT::default, |f| variant_double(f.1)),
            _ => VARIANT::default(),
        })
    }
    fn GetBoundingRectangles(&self) -> Result<*mut SAFEARRAY> {
        let host = self.shared.editor_host.as_ref().ok_or_else(unavailable)?;
        let range = *self.value.lock().map_err(|_| unavailable())?;
        rectangle_array(&host.rectangles(range)?)
    }
    fn GetEnclosingElement(&self) -> Result<IRawElementProviderSimple> {
        Ok(self.enclosing.clone())
    }
    fn GetText(&self, max: i32) -> Result<BSTR> {
        let range = *self.value.lock().map_err(|_| unavailable())?;
        self.shared
            .model()?
            .read(range, max)
            .map(|s| BSTR::from(s.as_str()))
            .map_err(read_error)
    }
    fn Move(&self, unit: TextUnit, count: i32) -> Result<i32> {
        if unit != TextUnit_Document {
            let host = self.shared.editor_host.as_ref().ok_or_else(unsupported)?;
            let current = *self.value.lock().map_err(|_| unavailable())?;
            let (next, moved) = host.move_range(current, unit, count)?;
            *self.value.lock().map_err(|_| unavailable())? = self.shared.check_range(next)?;
            return Ok(moved);
        }
        let end = self.shared.model()?.len_utf16().map_err(read_error)?;
        let mut range = self.value.lock().map_err(|_| unavailable())?;
        let next = if count < 0 {
            0
        } else if count > 0 {
            end
        } else {
            range.start
        };
        let moved = if next == range.start {
            0
        } else if count < 0 {
            -1
        } else {
            1
        };
        *range = TextRange::new(next, next);
        Ok(moved)
    }
    fn MoveEndpointByUnit(
        &self,
        endpoint: TextPatternRangeEndpoint,
        unit: TextUnit,
        count: i32,
    ) -> Result<i32> {
        let host = self.shared.editor_host.as_ref().ok_or_else(unsupported)?;
        let current = *self.value.lock().map_err(|_| unavailable())?;
        let (next, moved) = host.move_endpoint(current, endpoint, unit, count)?;
        *self.value.lock().map_err(|_| unavailable())? = self.shared.check_range(next)?;
        Ok(moved)
    }
    fn MoveEndpointByRange(
        &self,
        endpoint: TextPatternRangeEndpoint,
        other: Ref<ITextRangeProvider>,
        other_endpoint: TextPatternRangeEndpoint,
    ) -> Result<()> {
        let target = other.ok()?.cast_object_ref::<RangeProvider>()?;
        if !Arc::ptr_eq(&self.shared.selection, &target.shared.selection) {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        let value = endpoint_value(
            *target.value.lock().map_err(|_| unavailable())?,
            other_endpoint,
        )?;
        let mut range = self.value.lock().map_err(|_| unavailable())?;
        if endpoint == TextPatternRangeEndpoint_Start {
            range.start = value;
            if range.start > range.end {
                range.end = range.start;
            }
        } else if endpoint == TextPatternRangeEndpoint_End {
            range.end = value;
            if range.end < range.start {
                range.start = range.end;
            }
        } else {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        Ok(())
    }
    fn Select(&self) -> Result<()> {
        let value = *self.value.lock().map_err(|_| unavailable())?;
        if let Some(host) = &self.shared.editor_host {
            host.select(value)?;
        } else {
            self.shared.select.as_ref().ok_or_else(unsupported)?(value)?;
        }
        *self.shared.selection.lock().map_err(|_| unavailable())? = value;
        Ok(())
    }
    fn AddToSelection(&self) -> Result<()> {
        Err(unsupported())
    }
    fn RemoveFromSelection(&self) -> Result<()> {
        Err(unsupported())
    }
    fn ScrollIntoView(&self, align_top: BOOL) -> Result<()> {
        let host = self.shared.editor_host.as_ref().ok_or_else(unavailable)?;
        host.scroll_into_view(
            *self.value.lock().map_err(|_| unavailable())?,
            align_top.as_bool(),
        )
    }
    fn GetChildren(&self) -> Result<*mut SAFEARRAY> {
        range_array(&[])
    }
}

fn endpoint_value(range: TextRange, endpoint: TextPatternRangeEndpoint) -> Result<usize> {
    if endpoint == TextPatternRangeEndpoint_Start {
        Ok(range.start)
    } else if endpoint == TextPatternRangeEndpoint_End {
        Ok(range.end)
    } else {
        Err(Error::from_hresult(E_INVALIDARG))
    }
}

/// Call from WM_GETOBJECT. The factory runs only for a UIA root request.
/// Keep all window state on the UI thread; create a new snapshot in `factory`.
pub fn wm_getobject(
    hwnd: HWND,
    wparam: WPARAM,
    lparam: LPARAM,
    factory: impl FnOnce() -> DocumentProvider,
) -> Option<LRESULT> {
    if lparam.0 != UiaRootObjectId as isize {
        return None;
    }
    let provider = factory();
    Some(unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, provider.simple()) })
}
