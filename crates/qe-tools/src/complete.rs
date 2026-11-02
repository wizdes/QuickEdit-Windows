//! Complete's word list. On the Mac, `complete:` asks AppKit, whose default list comes from
//! the system dictionary (`NSSpellChecker`). Its source shows only that it asks for the word
//! before the caret. Windows has no such list, so this one comes from the document.
use crate::text::{align_down, align_up, fold_char, is_word, read_failure};
use qe_api::{ByteOffset, TextSnapshot};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

/// How far on each side of the caret the list looks, which bounds it on a big file.
pub const COMPLETE_WINDOW_BYTES: usize = 256 * 1024;
pub const COMPLETE_MAX_WORDS: usize = 100;
const LONGEST_WORD_BYTES: usize = 256;

fn folded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        fold_char(c, &mut |f| out.push(f));
    }
    out
}

/// Words near `at` that start with `prefix`, whatever their case, sorted without regard to
/// case. Words equal to the prefix are left out.
pub fn complete_candidates(
    snapshot: &dyn TextSnapshot,
    prefix: &str,
    at: ByteOffset,
    cancel: &AtomicBool,
) -> Result<Vec<String>, String> {
    if prefix.is_empty() {
        return Ok(Vec::new());
    }
    let start = align_down(snapshot, at.0.saturating_sub(COMPLETE_WINDOW_BYTES));
    let end = align_up(snapshot, at.0.saturating_add(COMPLETE_WINDOW_BYTES));
    let want = folded(prefix);
    let mut words: BTreeSet<(String, String)> = BTreeSet::new();
    let mut word = String::new();
    let mut first_piece = start > 0;
    let mut add = |word: &mut String, whole: bool| {
        if whole && !word.is_empty() && word.len() <= LONGEST_WORD_BYTES {
            let key = folded(word);
            if key.starts_with(&want) && key != want {
                words.insert((key, word.clone()));
            }
        }
        word.clear();
    };
    let good = snapshot.visit_chunks_while(ByteOffset(start)..ByteOffset(end), &mut |part| {
        for c in part.chars() {
            if is_word(c) {
                word.push(c);
            } else {
                add(&mut word, !first_piece);
                first_piece = false;
            }
        }
        !cancel.load(Ordering::Acquire)
    });
    add(&mut word, !first_piece && end == snapshot.len_bytes().0);
    if cancel.load(Ordering::Acquire) {
        return Err("Complete was cancelled.".to_string());
    }
    if !good {
        return Err(read_failure(snapshot));
    }
    Ok(words
        .into_iter()
        .map(|(_, word)| word)
        .take(COMPLETE_MAX_WORDS)
        .collect())
}
