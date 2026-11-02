//! Replace All's worker step, and a small worker for one result.
use crate::search::{Matcher, ScanEnd, Wake, scan_forward};
use crate::text::read_failure;
use qe_api::{ByteOffset, TextSnapshot};
use qe_editor::{PreparedEdits, TextEdit, prepare_edits};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Matches gathered between copies of the text between them.
const BATCH: usize = 4096;

/// A finished Replace All, ready for `EditorView::apply_edits`.
#[derive(Clone, Debug)]
pub struct ReplaceAllReady {
    pub prepared: PreparedEdits,
    /// How many matches it replaces. `PreparedEdits::edit_count` says 1, because the merged
    /// text goes in as one edit.
    pub count: u64,
    /// The offset just after the last replacement, in the new text.
    pub caret_after: ByteOffset,
}

/// Finds every match and builds the replaced text as it goes, with no list of matches. So
/// its memory follows the text, and not the number of matches: a query that matches every
/// byte costs no more than one that matches a few. `None` means no match.
pub fn prepare_replace_all(
    snapshot: &dyn TextSnapshot,
    matcher: &Matcher,
    replacement: &str,
    cancel: &AtomicBool,
) -> Result<Option<ReplaceAllReady>, String> {
    let len = snapshot.len_bytes();
    let mut batch: Vec<Range<ByteOffset>> = Vec::with_capacity(BATCH);
    let mut merged = String::new();
    let mut first: Option<ByteOffset> = None;
    let mut next_copy = ByteOffset(0);
    let mut from = ByteOffset(0);
    let mut count = 0_u64;
    loop {
        batch.clear();
        let end = scan_forward(snapshot, matcher, from..len, cancel, &mut |found| {
            batch.push(found);
            batch.len() < BATCH
        });
        match end {
            ScanEnd::Finished | ScanEnd::Stopped => {}
            ScanEnd::Cancelled => return Err("Replace All was cancelled.".to_string()),
            ScanEnd::Failed(message) => return Err(message),
            ScanEnd::NotYetIndexed { .. } => {
                return Err("The file is still being read.".to_string());
            }
        }
        for found in &batch {
            if cancel.load(Ordering::Acquire) {
                return Err("Replace All was cancelled.".to_string());
            }
            match first {
                None => first = Some(found.start),
                Some(_) => {
                    let mut good = true;
                    let mut problem = None;
                    good &= snapshot.visit_chunks(next_copy..found.start, &mut |part| {
                        if merged.try_reserve(part.len()).is_err() {
                            problem = Some(());
                        } else {
                            merged.push_str(part);
                        }
                    });
                    if problem.is_some() {
                        return Err("There is not enough memory to replace this text.".to_string());
                    }
                    if !good {
                        return Err(read_failure(snapshot));
                    }
                }
            }
            if merged.try_reserve(replacement.len()).is_err() {
                return Err("There is not enough memory to replace this text.".to_string());
            }
            merged.push_str(replacement);
            next_copy = found.end;
            count += 1;
        }
        // A batch that is not full means that the scan reached the end.
        let Some(last) = batch.last() else { break };
        from = last.end;
        if batch.len() < BATCH {
            break;
        }
    }
    let Some(start) = first else {
        return Ok(None);
    };
    let caret_after = ByteOffset(start.0 + merged.len());
    let prepared = prepare_edits(
        snapshot,
        &[TextEdit {
            range: start..next_copy,
            text: merged,
        }],
        cancel,
    )?;
    Ok(Some(ReplaceAllReady {
        prepared,
        count,
        caret_after,
    }))
}

/// One piece of work on its own thread, with a cancel and a wake.
pub struct Worker<T> {
    slot: Arc<Mutex<Option<Result<T, String>>>>,
    cancel: Arc<AtomicBool>,
}
impl<T: Send + 'static> Worker<T> {
    pub fn start(
        name: &str,
        wake: Option<Wake>,
        work: impl FnOnce(&AtomicBool) -> Result<T, String> + Send + 'static,
    ) -> Self {
        let slot = Arc::new(Mutex::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        let (inner, flag) = (slot.clone(), cancel.clone());
        let spawned = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                let result = work(&flag);
                if let Ok(mut place) = inner.lock() {
                    *place = Some(result);
                }
                if let Some(wake) = wake {
                    wake();
                }
            });
        if let Err(error) = spawned
            && let Ok(mut place) = slot.lock()
        {
            *place = Some(Err(format!("A worker could not start: {error}")));
        }
        Self { slot, cancel }
    }
    /// The result, once it has come.
    pub fn take(&self) -> Option<Result<T, String>> {
        self.slot.lock().ok()?.take()
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
