//! The native Open and Save dialogs, with the encoding list and the `.txt` checkbox
//! added through `IFileDialogCustomize` (parity rows P4, P20, P23, I40).
//!
//! Each function needs a thread with COM started as single-threaded. `FileDialogSetup`
//! builds the dialog without showing it, so tests can read the controls with no window.
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::{
    CDCS_ENABLEDVISIBLE, CDCS_VISIBLE, FileOpenDialog, FileSaveDialog, IFileDialog,
    IFileDialogCustomize, IFileOpenDialog, IFileSaveDialog, SIGDN_FILESYSPATH,
};
use windows::core::{BOOL, HSTRING, Interface, PCWSTR, Result};

const GROUP: u32 = 100;
const ENCODING_LIST: u32 = 101;
const TXT_CHECK: u32 = 102;

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

#[derive(Clone, Debug, Default)]
pub struct FileDialogOptions {
    pub title: Option<String>,
    pub file_name: Option<String>,
    /// For Open, the first row is usually "Automatic". For Save it has no such row.
    pub encodings: Vec<EncodingRow>,
    pub selected: usize,
    /// `Some` adds the `.txt` checkbox, with this starting state. Save only.
    pub add_txt: Option<bool>,
}

/// What the user chose. Each `Option` is `None` when the dialog did not have that control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDialogChoice {
    pub path: String,
    pub encoding: Option<usize>,
    pub add_txt: Option<bool>,
}

/// A built native dialog that is not on screen yet.
pub struct FileDialogSetup {
    dialog: IFileDialog,
    custom: IFileDialogCustomize,
    has_list: bool,
    has_check: bool,
}
fn configure(dialog: IFileDialog, options: &FileDialogOptions) -> Result<FileDialogSetup> {
    let custom: IFileDialogCustomize = dialog.cast()?;
    // SAFETY: each call gets a live COM object and strings that outlive the call.
    unsafe {
        if let Some(title) = &options.title {
            dialog.SetTitle(&HSTRING::from(title))?;
        }
        if let Some(name) = &options.file_name {
            dialog.SetFileName(&HSTRING::from(name))?;
        }
        let has_list = !options.encodings.is_empty();
        if has_list {
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
        if let Some(on) = options.add_txt {
            custom.AddCheckButton(
                TXT_CHECK,
                &HSTRING::from("If no extension is provided, use .txt"),
                on,
            )?;
        }
        Ok(FileDialogSetup {
            dialog,
            custom,
            has_list,
            has_check: options.add_txt.is_some(),
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
        if !self.has_list {
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
    /// The state of the `.txt` checkbox.
    pub fn add_txt(&self) -> Result<Option<bool>> {
        if !self.has_check {
            return Ok(None);
        }
        // SAFETY: the control was added in `configure`.
        unsafe { self.custom.GetCheckButtonState(TXT_CHECK) }.map(|on: BOOL| Some(on.as_bool()))
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
            Ok(Some(FileDialogChoice {
                path: path.map_err(|e| {
                    windows::core::Error::new(
                        windows::Win32::Foundation::E_FAIL,
                        format!("the chosen path is not valid text: {e}"),
                    )
                })?,
                encoding: self.selected_encoding()?,
                add_txt: self.add_txt()?,
            }))
        }
    }
}
