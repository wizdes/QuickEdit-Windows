//! Opens a file off the UI thread, so the window appears before the file has loaded.
//!
//! `FileLoad::start` returns at once. A thread reads the file and sends one result.
//! Dropping the `FileLoad`, as a closing window does, cancels the read: the thread
//! checks a flag between reads and stops.
use crate::preferred_line_ending_bytes;
use qe_buffer::{MappedStore, Notify, PieceStore};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::JoinHandle,
    time::Duration,
};

/// Files above this size open read-only, as the plan says.
pub const EDITABLE_FILE_LIMIT: u64 = 512 * 1024 * 1024;
const READ_BYTES: u64 = 16 * 1024 * 1024;
/// A tree of 4 KiB leaves costs about one part in this much more than its text.
const TREE_COST_DIVISOR: usize = 16;

/// A file that has finished loading.
pub enum LoadedFile {
    Editable {
        store: PieceStore,
        ending: &'static str,
    },
    ReadOnly(MappedStore),
}

/// What the loading thread reports.
pub enum LoadResult {
    Loaded(LoadedFile),
    Failed(String),
    Cancelled,
}

#[derive(Clone, Default)]
pub struct LoadOptions {
    /// Opens the file read-only whatever its size.
    pub force_read_only: bool,
    /// A pause before each read, for tests of cancelling.
    #[doc(hidden)]
    pub test_read_delay: Option<Duration>,
    /// A lower size limit, so a test can grow a file past it cheaply.
    #[doc(hidden)]
    pub test_editable_limit: Option<u64>,
}

pub struct FileLoad {
    results: Receiver<LoadResult>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FileLoad {
    /// `wake` runs on the loading thread when the result is ready, and on store
    /// progress for a read-only file. The result then waits in `poll`.
    pub fn start(
        path: impl AsRef<Path>,
        options: LoadOptions,
        wake: Notify,
    ) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let (sender, results) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("qe-editor-load".into())
            .spawn(move || {
                let result = load(&path, &options, &flag, wake.clone());
                if sender.send(result).is_ok() {
                    wake();
                }
            })?;
        Ok(Self {
            results,
            cancel,
            thread: Some(thread),
        })
    }
    /// The result, once. It never waits.
    pub fn poll(&mut self) -> Option<LoadResult> {
        match self.results.try_recv() {
            Ok(result) => {
                // The one result has arrived. Nothing more can come.
                self.thread = None;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                if self.thread.is_some() {
                    self.thread = None;
                    Some(LoadResult::Failed(
                        "The file could not be read because its thread stopped.".into(),
                    ))
                } else {
                    None
                }
            }
        }
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    /// Cancels and waits for the thread to end, for tests and for a clean exit.
    pub fn cancel_and_wait(mut self) -> Result<(), String> {
        self.cancel();
        match self.thread.take() {
            Some(thread) => thread
                .join()
                .map_err(|_| "the loading thread panicked".to_string()),
            None => Ok(()),
        }
    }
}

impl Drop for FileLoad {
    fn drop(&mut self) {
        self.cancel();
        // A result nobody took can hold a whole file, so another thread frees it. If that
        // thread cannot start, the closure comes back and it is freed here instead.
        let (_, empty) = mpsc::channel();
        let results = std::mem::replace(&mut self.results, empty);
        let freeing = std::thread::Builder::new()
            .name("qe-editor-load-free".into())
            .spawn(move || drop(results));
        if let Err(e) = freeing {
            eprintln!("freeing a finished load on this thread, as no thread started: {e}");
        }
    }
}

fn describe(path: &Path, error: impl std::fmt::Display) -> String {
    format!("Could not open {}: {error}", path.display())
}

fn load_read_only(path: &PathBuf, cancel: &AtomicBool, wake: Notify) -> LoadResult {
    match MappedStore::open_with_notify(path, wake) {
        Ok(store) if cancel.load(Ordering::Acquire) => {
            drop(store);
            LoadResult::Cancelled
        }
        Ok(store) => LoadResult::Loaded(LoadedFile::ReadOnly(store)),
        Err(e) => LoadResult::Failed(describe(path, e)),
    }
}

/// `PieceStore::new` copies the text and cannot report a failed allocation, which would
/// end the app. So this asks for that much first, plus the tree's own cost, and says so
/// in words if it is not there. Another program could still take the memory between this
/// and the copy, and only a change in qe-buffer can close that gap.
pub(crate) fn check_room_for_tree(path: &Path, bytes: usize) -> Result<(), String> {
    let needed = bytes.saturating_add(bytes / TREE_COST_DIVISOR);
    let mut probe: Vec<u8> = Vec::new();
    probe.try_reserve_exact(needed).map_err(|_| {
        format!(
            "Could not open {}: there is not enough memory for its {} MiB.",
            path.display(),
            bytes >> 20
        )
    })
}

fn load(path: &PathBuf, options: &LoadOptions, cancel: &AtomicBool, wake: Notify) -> LoadResult {
    let size = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(e) => return LoadResult::Failed(describe(path, e)),
    };
    let limit = options.test_editable_limit.unwrap_or(EDITABLE_FILE_LIMIT);
    if options.force_read_only || size > limit {
        return load_read_only(path, cancel, wake);
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) => return LoadResult::Failed(describe(path, e)),
    };
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(size as usize + 1).is_err() {
        return LoadResult::Failed(format!(
            "Could not open {}: there is not enough memory for its {} MiB.",
            path.display(),
            size >> 20
        ));
    }
    loop {
        if cancel.load(Ordering::Acquire) {
            return LoadResult::Cancelled;
        }
        if let Some(delay) = options.test_read_delay {
            std::thread::sleep(delay);
        }
        match file.by_ref().take(READ_BYTES).read_to_end(&mut bytes) {
            Ok(0) => break,
            // The file grew past the limit, so it must not stay editable.
            Ok(_) if bytes.len() as u64 > limit => {
                drop(bytes);
                return load_read_only(path, cancel, wake);
            }
            Ok(_) => {}
            Err(e) => return LoadResult::Failed(describe(path, e)),
        }
    }
    let ending = preferred_line_ending_bytes(&bytes);
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => {
            return LoadResult::Failed(describe(
                path,
                format!(
                    "it is not valid UTF-8 at byte {}. This window reads UTF-8 only",
                    e.utf8_error().valid_up_to()
                ),
            ));
        }
    };
    if cancel.load(Ordering::Acquire) {
        return LoadResult::Cancelled;
    }
    if let Err(message) = check_room_for_tree(path, text.len()) {
        return LoadResult::Failed(message);
    }
    let store = PieceStore::new(&text);
    if cancel.load(Ordering::Acquire) {
        return LoadResult::Cancelled;
    }
    LoadResult::Loaded(LoadedFile::Editable { store, ending })
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_tree_too_big_for_memory_gives_a_message_and_not_an_abort() {
        let path = std::path::Path::new("big.txt");
        let message = super::check_room_for_tree(path, usize::MAX / 2).unwrap_err();
        assert!(message.contains("not enough memory"), "{message}");
        assert!(message.contains("big.txt"), "{message}");
        assert!(super::check_room_for_tree(path, 1024).is_ok());
    }
}
