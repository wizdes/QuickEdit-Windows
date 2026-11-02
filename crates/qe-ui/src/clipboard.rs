use std::fmt::Debug;
use std::sync::{Arc, Mutex};

/// Where cut, copy and paste go. The default is `SystemClipboard`, the Windows
/// clipboard. A test passes one `MemoryClipboard` to several fields.
pub trait ClipboardAccess: Send + Sync + Debug {
    /// `Ok(None)` means the clipboard holds no text.
    fn read_text(&self) -> Result<Option<String>, String>;
    fn write_text(&self, text: &str) -> Result<(), String>;
}
pub type SharedClipboard = Arc<dyn ClipboardAccess>;

/// A clipboard held in memory, for tests.
#[derive(Debug, Default)]
pub struct MemoryClipboard {
    text: Mutex<Option<String>>,
}
impl MemoryClipboard {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }
}
impl ClipboardAccess for MemoryClipboard {
    fn read_text(&self) -> Result<Option<String>, String> {
        self.text
            .lock()
            .map(|text| text.clone())
            .map_err(|_| "the memory clipboard lock is poisoned".to_string())
    }
    fn write_text(&self, text: &str) -> Result<(), String> {
        let mut slot = self
            .text
            .lock()
            .map_err(|_| "the memory clipboard lock is poisoned".to_string())?;
        *slot = Some(text.to_owned());
        Ok(())
    }
}
