//! The native Open and Save dialogs, with the encoding list, the `.txt` checkbox and the PDF boxes
//! added through `IFileDialogCustomize` (parity rows P4, P20, P23, P60, I40, I59).
//!
//! Each function needs a thread with COM started as single-threaded. `FileDialogSetup`
//! builds the dialog without showing it, so tests can read the controls with no window.
use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::{E_NOINTERFACE, E_NOTIMPL, ERROR_CANCELLED, HWND, S_OK};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::{
    CDCS_ENABLEDVISIBLE, CDCS_VISIBLE, FDE_OVERWRITE_RESPONSE, FDE_SHAREVIOLATION_RESPONSE,
    FOS_FORCEFILESYSTEM, FileOpenDialog, FileSaveDialog, IFileDialog, IFileDialogControlEvents,
    IFileDialogControlEvents_Vtbl, IFileDialogCustomize, IFileDialogEvents, IFileDialogEvents_Vtbl,
    IFileOpenDialog, IFileSaveDialog, SIGDN_FILESYSPATH,
};
use windows::core::{
    BOOL, GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, Interface, PCWSTR, Result, w,
};

const GROUP: u32 = 100;
const ENCODING_LIST: u32 = 101;
const TXT_CHECK: u32 = 102;
const HEADER_CHECK: u32 = 103;
const REWRAP_CHECK: u32 = 104;

/// A row of the encoding list. A disabled row stays in view and cannot be chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodingRow {
    pub name: String,
    pub enabled: bool,
}
impl EncodingRow {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: true,
        }
    }
}

/// The two boxes of the PDF dialog, as on the Mac's PDF Save panel (rows P60, I59). `rewrap` is
/// `None` in Wrap to Page, whose pages are fixed: then that box does not show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PdfChecks {
    pub header: bool,
    pub rewrap: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct FileDialogOptions {
    pub title: Option<String>,
    pub file_name: Option<String>,
    /// For Open, the first row is usually "Automatic". For Save it has no such row.
    pub encodings: Vec<EncodingRow>,
    pub selected: usize,
    /// `Some` adds the `.txt` checkbox, with this starting state. Save only.
    pub add_txt: Option<bool>,
    /// `Some` adds the PDF dialog's boxes, with these starting states. Save only.
    pub pdf: Option<PdfChecks>,
}

/// What the user chose. Each `Option` is `None` when the dialog did not have that control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDialogChoice {
    pub path: String,
    /// The chosen row. `None` when there is no list, or no row may be used: then
    /// `encoding_note` says why, and the caller must not save with any encoding.
    pub encoding: Option<usize>,
    pub encoding_note: Option<String>,
    pub add_txt: Option<bool>,
    pub pdf: Option<PdfChecks>,
}

/// Which row may be used, from what the dialog reports. A row that cannot hold the text
/// is refused here too (row P23), so that a lossy encoding cannot get through even if
/// the dialog let it be chosen. A failure does not cost the user the path they chose.
pub fn checked_encoding_row(
    selected: std::result::Result<usize, String>,
    rows: &[EncodingRow],
) -> (Option<usize>, Option<String>) {
    match selected {
        Ok(row) => match rows.get(row) {
            Some(found) if found.enabled => (Some(row), None),
            Some(found) => (None, Some(format!("{} cannot hold this text", found.name))),
            None => (None, Some(format!("the list has no row {row}"))),
        },
        Err(error) => (None, Some(format!("no encoding could be read: {error}"))),
    }
}

/// The dialog's own check for an existing file looks at the name with its default
/// extension. It must see the name that will be saved, so this follows the checkbox.
fn apply_extension(dialog: &IFileDialog, add_txt: bool, shared: &Cell<bool>) -> Result<()> {
    // SAFETY: a live COM object, and a static string or null.
    unsafe {
        if add_txt {
            dialog.SetDefaultExtension(w!("txt"))?;
        } else {
            dialog.SetDefaultExtension(PCWSTR::null())?;
        }
    }
    shared.set(add_txt);
    Ok(())
}

/// A COM object that hears the `.txt` checkbox. It is written by hand because the
/// `implement` macro needs `windows-core` as a dependency of this crate, which adds a
/// line to Cargo.lock.
#[repr(C)]
struct DialogEvents {
    events: *const IFileDialogEvents_Vtbl,
    control: *const IFileDialogControlEvents_Vtbl,
    refs: AtomicU32,
    extension_on: Rc<Cell<bool>>,
}
const CONTROL_OFFSET: usize = std::mem::size_of::<*const c_void>();
unsafe fn object_of_events(this: *mut c_void) -> *mut DialogEvents {
    this.cast()
}
unsafe fn object_of_control(this: *mut c_void) -> *mut DialogEvents {
    // SAFETY: `this` is the second field of the object.
    unsafe { this.cast::<u8>().sub(CONTROL_OFFSET).cast() }
}
unsafe fn query(object: *mut DialogEvents, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    // SAFETY: COM passes valid pointers, and `object` is live.
    unsafe {
        let iid = *iid;
        if iid == IUnknown::IID || iid == IFileDialogEvents::IID {
            *out = object.cast();
        } else if iid == IFileDialogControlEvents::IID {
            *out = object.cast::<u8>().add(CONTROL_OFFSET).cast();
        } else {
            *out = std::ptr::null_mut();
            return E_NOINTERFACE;
        }
        (*object).refs.fetch_add(1, Ordering::Relaxed);
    }
    S_OK
}
unsafe fn add_ref(object: *mut DialogEvents) -> u32 {
    // SAFETY: `object` is live.
    unsafe { (*object).refs.fetch_add(1, Ordering::Relaxed) + 1 }
}
unsafe fn release(object: *mut DialogEvents) -> u32 {
    // SAFETY: `object` is live until its last reference goes, here.
    unsafe {
        let left = (*object).refs.fetch_sub(1, Ordering::AcqRel) - 1;
        if left == 0 {
            drop(Box::from_raw(object));
        }
        left
    }
}
unsafe extern "system" fn events_query(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: called by COM on a live object.
    unsafe { query(object_of_events(this), iid, out) }
}
unsafe extern "system" fn events_add_ref(this: *mut c_void) -> u32 {
    // SAFETY: called by COM on a live object.
    unsafe { add_ref(object_of_events(this)) }
}
unsafe extern "system" fn events_release(this: *mut c_void) -> u32 {
    // SAFETY: called by COM on a live object.
    unsafe { release(object_of_events(this)) }
}
unsafe extern "system" fn control_query(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: called by COM on a live object.
    unsafe { query(object_of_control(this), iid, out) }
}
unsafe extern "system" fn control_add_ref(this: *mut c_void) -> u32 {
    // SAFETY: called by COM on a live object.
    unsafe { add_ref(object_of_control(this)) }
}
unsafe extern "system" fn control_release(this: *mut c_void) -> u32 {
    // SAFETY: called by COM on a live object.
    unsafe { release(object_of_control(this)) }
}
unsafe extern "system" fn not_used_1(_: *mut c_void, _: *mut c_void) -> HRESULT {
    E_NOTIMPL
}
unsafe extern "system" fn not_used_2(_: *mut c_void, _: *mut c_void, _: *mut c_void) -> HRESULT {
    E_NOTIMPL
}
unsafe extern "system" fn not_used_share(
    _: *mut c_void,
    _: *mut c_void,
    _: *mut c_void,
    _: *mut FDE_SHAREVIOLATION_RESPONSE,
) -> HRESULT {
    E_NOTIMPL
}
unsafe extern "system" fn not_used_overwrite(
    _: *mut c_void,
    _: *mut c_void,
    _: *mut c_void,
    _: *mut FDE_OVERWRITE_RESPONSE,
) -> HRESULT {
    E_NOTIMPL
}
unsafe extern "system" fn file_ok(_: *mut c_void, _: *mut c_void) -> HRESULT {
    S_OK
}
unsafe extern "system" fn not_used_item(_: *mut c_void, _: *mut c_void, _: u32, _: u32) -> HRESULT {
    E_NOTIMPL
}
unsafe extern "system" fn not_used_control(_: *mut c_void, _: *mut c_void, _: u32) -> HRESULT {
    E_NOTIMPL
}
unsafe extern "system" fn check_toggled(
    this: *mut c_void,
    customize: *mut c_void,
    control: u32,
    checked: BOOL,
) -> HRESULT {
    if control != TXT_CHECK {
        return S_OK;
    }
    // SAFETY: called by COM on a live object, with a live customize object that this
    // only borrows.
    unsafe {
        let object = object_of_control(this);
        let borrowed = std::mem::ManuallyDrop::new(IFileDialogCustomize::from_raw(customize));
        let done = borrowed.cast::<IFileDialog>().and_then(|dialog| {
            apply_extension(&dialog, checked.as_bool(), &(*object).extension_on)
        });
        match done {
            Ok(()) => S_OK,
            Err(error) => error.code(),
        }
    }
}
static EVENTS_TABLE: IFileDialogEvents_Vtbl = IFileDialogEvents_Vtbl {
    base__: IUnknown_Vtbl {
        QueryInterface: events_query,
        AddRef: events_add_ref,
        Release: events_release,
    },
    OnFileOk: file_ok,
    OnFolderChanging: not_used_2,
    OnFolderChange: not_used_1,
    OnSelectionChange: not_used_1,
    OnShareViolation: not_used_share,
    OnTypeChange: not_used_1,
    OnOverwrite: not_used_overwrite,
};
static CONTROL_TABLE: IFileDialogControlEvents_Vtbl = IFileDialogControlEvents_Vtbl {
    base__: IUnknown_Vtbl {
        QueryInterface: control_query,
        AddRef: control_add_ref,
        Release: control_release,
    },
    OnItemSelected: not_used_item,
    OnButtonClicked: not_used_control,
    OnCheckButtonToggled: check_toggled,
    OnControlActivating: not_used_control,
};
fn new_events(extension_on: Rc<Cell<bool>>) -> IFileDialogEvents {
    let object = Box::into_raw(Box::new(DialogEvents {
        events: &EVENTS_TABLE,
        control: &CONTROL_TABLE,
        refs: AtomicU32::new(1),
        extension_on,
    }));
    // SAFETY: the object starts with this interface's table and holds one reference,
    // which the returned value now owns.
    unsafe { IFileDialogEvents::from_raw(object.cast()) }
}

/// A built native dialog that is not on screen yet.
pub struct FileDialogSetup {
    dialog: IFileDialog,
    custom: IFileDialogCustomize,
    rows: Vec<EncodingRow>,
    has_check: bool,
    pdf: Option<PdfChecks>,
    extension_on: Rc<Cell<bool>>,
    events: Option<(IFileDialogEvents, u32)>,
}
fn configure(dialog: IFileDialog, options: &FileDialogOptions) -> Result<FileDialogSetup> {
    let custom: IFileDialogCustomize = dialog.cast()?;
    let extension_on = Rc::new(Cell::new(false));
    // SAFETY: each call gets a live COM object and strings that outlive the call.
    unsafe {
        // A virtual item has no file path, and it would fail only after the user chose it.
        dialog.SetOptions(dialog.GetOptions()? | FOS_FORCEFILESYSTEM)?;
        if let Some(title) = &options.title {
            dialog.SetTitle(&HSTRING::from(title))?;
        }
        if let Some(name) = &options.file_name {
            dialog.SetFileName(&HSTRING::from(name))?;
        }
        if !options.encodings.is_empty() {
            custom.StartVisualGroup(GROUP, &HSTRING::from("Plain Text Encoding:"))?;
            custom.AddComboBox(ENCODING_LIST)?;
            for (i, row) in options.encodings.iter().enumerate() {
                custom.AddControlItem(ENCODING_LIST, i as u32, &HSTRING::from(&row.name))?;
                if !row.enabled {
                    custom.SetControlItemState(ENCODING_LIST, i as u32, CDCS_VISIBLE)?;
                }
            }
            let start = options
                .encodings
                .get(options.selected)
                .filter(|row| row.enabled)
                .map(|_| options.selected)
                .or_else(|| options.encodings.iter().position(|row| row.enabled));
            if let Some(start) = start {
                custom.SetSelectedControlItem(ENCODING_LIST, start as u32)?;
            }
            custom.EndVisualGroup()?;
        }
        let mut events = None;
        if let Some(on) = options.add_txt {
            custom.AddCheckButton(
                TXT_CHECK,
                &HSTRING::from("If no extension is provided, use .txt"),
                on,
            )?;
            apply_extension(&dialog, on, &extension_on)?;
            let listener = new_events(extension_on.clone());
            let cookie = dialog.Advise(&listener)?;
            events = Some((listener, cookie));
        }
        if let Some(checks) = options.pdf {
            custom.AddCheckButton(
                HEADER_CHECK,
                &HSTRING::from("Print header and footer"),
                checks.header,
            )?;
            if let Some(rewrap) = checks.rewrap {
                custom.AddCheckButton(
                    REWRAP_CHECK,
                    &HSTRING::from("Rewrap contents to fit page"),
                    rewrap,
                )?;
            }
        }
        Ok(FileDialogSetup {
            dialog,
            custom,
            rows: options.encodings.clone(),
            has_check: options.add_txt.is_some(),
            pdf: options.pdf,
            extension_on,
            events,
        })
    }
}
impl FileDialogSetup {
    pub fn open(options: &FileDialogOptions) -> Result<Self> {
        // SAFETY: creates an in-process COM object on this thread.
        let dialog: IFileOpenDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }?;
        configure(dialog.cast()?, options)
    }
    pub fn save(options: &FileDialogOptions) -> Result<Self> {
        // SAFETY: creates an in-process COM object on this thread.
        let dialog: IFileSaveDialog =
            unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) }?;
        configure(dialog.cast()?, options)
    }
    /// The chosen row of the encoding list.
    pub fn selected_encoding(&self) -> Result<Option<usize>> {
        if self.rows.is_empty() {
            return Ok(None);
        }
        // SAFETY: the control was added in `configure`.
        unsafe { self.custom.GetSelectedControlItem(ENCODING_LIST) }.map(|item| Some(item as usize))
    }
    /// True when a row of the encoding list can be chosen.
    pub fn encoding_enabled(&self, row: usize) -> Result<bool> {
        // SAFETY: the control was added in `configure`.
        let state = unsafe { self.custom.GetControlItemState(ENCODING_LIST, row as u32) }?;
        Ok(state == CDCS_ENABLEDVISIBLE)
    }
    /// The chosen row, if it may be used, or the reason that none may.
    pub fn checked_encoding(&self) -> (Option<usize>, Option<String>) {
        if self.rows.is_empty() {
            return (None, None);
        }
        // SAFETY: the control was added in `configure`.
        let selected = unsafe { self.custom.GetSelectedControlItem(ENCODING_LIST) }
            .map(|item| item as usize)
            .map_err(|error| error.to_string());
        checked_encoding_row(selected, &self.rows)
    }
    /// The state of the `.txt` checkbox.
    pub fn add_txt(&self) -> Result<Option<bool>> {
        if !self.has_check {
            return Ok(None);
        }
        // SAFETY: the control was added in `configure`.
        unsafe { self.custom.GetCheckButtonState(TXT_CHECK) }.map(|on: BOOL| Some(on.as_bool()))
    }
    /// The state of the PDF dialog's boxes.
    pub fn pdf_checks(&self) -> Result<Option<PdfChecks>> {
        let Some(start) = self.pdf else {
            return Ok(None);
        };
        // SAFETY: each control was added in `configure`.
        unsafe {
            let header = self.custom.GetCheckButtonState(HEADER_CHECK)?.as_bool();
            let rewrap = match start.rewrap {
                Some(_) => Some(self.custom.GetCheckButtonState(REWRAP_CHECK)?.as_bool()),
                None => None,
            };
            Ok(Some(PdfChecks { header, rewrap }))
        }
    }
    /// The dialog's option flags, as Windows reports them.
    pub fn options(&self) -> Result<u32> {
        // SAFETY: a live COM object.
        unsafe { self.dialog.GetOptions() }.map(|flags| flags.0)
    }
    /// True while the dialog's default extension is `txt`. The overwrite check then
    /// looks at `notes.txt` when the user types `notes`.
    pub fn default_extension_is_txt(&self) -> bool {
        self.extension_on.get()
    }
    /// What the dialog does when the user flips the `.txt` checkbox: it goes through
    /// the same COM call, for tests with no window.
    #[doc(hidden)]
    pub fn send_txt_toggle(&self, on: bool) -> Result<()> {
        let Some((events, _)) = &self.events else {
            return Ok(());
        };
        let control: IFileDialogControlEvents = events.cast()?;
        // SAFETY: both objects are live.
        unsafe { control.OnCheckButtonToggled(&self.custom, TXT_CHECK, on) }
    }
    /// Shows the dialog on screen and waits. `Ok(None)` means the user cancelled.
    pub fn show(&self, owner: Option<HWND>) -> Result<Option<FileDialogChoice>> {
        // SAFETY: the dialog is live. The returned path string is freed once below.
        unsafe {
            if let Err(error) = self.dialog.Show(owner) {
                if error.code() == ERROR_CANCELLED.to_hresult() {
                    return Ok(None);
                }
                return Err(error);
            }
            let item = self.dialog.GetResult()?;
            let raw = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = raw.to_string();
            CoTaskMemFree(Some(PCWSTR(raw.0).0 as _));
            let (encoding, encoding_note) = self.checked_encoding();
            Ok(Some(FileDialogChoice {
                path: path.map_err(|e| {
                    windows::core::Error::new(
                        windows::Win32::Foundation::E_FAIL,
                        format!("the chosen path is not valid text: {e}"),
                    )
                })?,
                encoding,
                encoding_note,
                add_txt: self.add_txt()?,
                pdf: self.pdf_checks()?,
            }))
        }
    }
}
impl Drop for FileDialogSetup {
    fn drop(&mut self) {
        if let Some((_, cookie)) = &self.events {
            // SAFETY: the cookie came from Advise on this dialog.
            let _ = unsafe { self.dialog.Unadvise(*cookie) };
        }
    }
}
