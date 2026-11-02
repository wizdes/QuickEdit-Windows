//! Transformations: `Make Upper Case`, `Make Lower Case` and `Capitalize`.
//!
//! The Mac calls `uppercaseWord:`, `lowercaseWord:` and `capitalizeWord:`. Their code is in
//! AppKit, so its source shows only the labels and that a read-only view refuses them. The
//! case rules here are Unicode's with no locale, as `NSString` has them.
//!
//! These never use the standard `to_uppercase` on a long text, because that aborts the app
//! when memory runs out, which loses the user's text. Every step here can say so instead.
use crate::text::{is_extend, read_failure};
use qe_api::{ByteOffset, TextSnapshot};
use qe_editor::{PreparedEdits, TextEdit, prepare_edits};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

const NO_MEMORY: &str = "There is not enough memory to transform this text.";
/// How far around a sigma the final-sigma rule looks. It needs only the nearest letters.
const SIGMA_CONTEXT_CHARS: usize = 32;
const CAPITAL_SIGMA: char = '\u{3a3}';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transform {
    Upper,
    Lower,
    Capitalize,
}

fn push(out: &mut String, c: char) -> Result<(), String> {
    out.try_reserve(c.len_utf8()).map_err(|_| NO_MEMORY)?;
    out.push(c);
    Ok(())
}

fn title_case(c: char, out: &mut String) -> Result<(), String> {
    // The four digraphs have their own title forms, and `ß` titles as `Ss`.
    match c {
        '\u{1c4}'..='\u{1c6}' => push(out, '\u{1c5}'),
        '\u{1c7}'..='\u{1c9}' => push(out, '\u{1c8}'),
        '\u{1ca}'..='\u{1cc}' => push(out, '\u{1cb}'),
        '\u{1f1}'..='\u{1f3}' => push(out, '\u{1f2}'),
        _ => {
            for (i, upper) in c.to_uppercase().enumerate() {
                if i == 0 {
                    push(out, upper)?;
                } else {
                    for lower in upper.to_lowercase() {
                        push(out, lower)?;
                    }
                }
            }
            Ok(())
        }
    }
}

/// What a capital sigma becomes where it stands: `ς` at the end of a word, or `σ`.
fn lower_sigma(text: &str, at: usize) -> char {
    let before: Vec<char> = text
        .get(..at)
        .unwrap_or("")
        .chars()
        .rev()
        .take(SIGMA_CONTEXT_CHARS)
        .collect();
    let mut small = String::with_capacity(SIGMA_CONTEXT_CHARS * 8);
    small.extend(before.iter().rev());
    let lead = small.to_lowercase().len();
    small.extend(
        text.get(at..)
            .unwrap_or("")
            .chars()
            .take(SIGMA_CONTEXT_CHARS),
    );
    small
        .to_lowercase()
        .get(lead..)
        .and_then(|s| s.chars().next())
        .unwrap_or('\u{3c3}')
}

/// As `str::to_lowercase`, without its abort.
fn lower_into(text: &str, out: &mut String) -> Result<(), String> {
    for (at, c) in text.char_indices() {
        if c == CAPITAL_SIGMA {
            push(out, lower_sigma(text, at))?;
        } else {
            for lower in c.to_lowercase() {
                push(out, lower)?;
            }
        }
    }
    Ok(())
}

/// True for a character that leaves a word open: marks and the apostrophes.
fn keeps_word(c: char) -> bool {
    is_extend(c) || matches!(c, '\'' | '\u{2019}' | '\u{2bc}' | '\u{b7}')
}

/// The new text, or a sentence if memory runs out. Letters start a word and any other
/// character ends it, so `1st` gives `1St`, and an apostrophe leaves a word open, so `don't`
/// gives `Don't`.
pub fn transform_text(text: &str, transform: Transform) -> Result<String, String> {
    let mut out = String::new();
    out.try_reserve(text.len()).map_err(|_| NO_MEMORY)?;
    match transform {
        Transform::Upper => {
            for c in text.chars() {
                for upper in c.to_uppercase() {
                    push(&mut out, upper)?;
                }
            }
        }
        Transform::Lower => lower_into(text, &mut out)?,
        Transform::Capitalize => {
            let mut rest = text;
            while let Some(c) = rest.chars().next() {
                let tail = rest.get(c.len_utf8()..).unwrap_or("");
                if !c.is_alphabetic() {
                    push(&mut out, c)?;
                    rest = tail;
                    continue;
                }
                let word_len = tail
                    .char_indices()
                    .find(|(_, n)| !n.is_alphabetic() && !keeps_word(*n))
                    .map_or(tail.len(), |(at, _)| at);
                title_case(c, &mut out)?;
                // The first letter is part of the word for the sigma rule, so look at the
                // whole word, and keep only what follows the first letter.
                let word_end = c.len_utf8() + word_len;
                let word = rest.get(..word_end).unwrap_or("");
                for (at, n) in word.char_indices().skip(1) {
                    if n == CAPITAL_SIGMA {
                        push(&mut out, lower_sigma(word, at))?;
                    } else {
                        for lower in n.to_lowercase() {
                            push(&mut out, lower)?;
                        }
                    }
                }
                rest = rest.get(word_end..).unwrap_or("");
            }
        }
    }
    Ok(out)
}

/// Reads the range and makes one edit of the new text. Run it on a worker for a long range.
/// `None` means that the text would not change, so there is nothing to do and no undo step.
pub fn prepare_transform(
    snapshot: &dyn TextSnapshot,
    range: Range<ByteOffset>,
    transform: Transform,
    cancel: &AtomicBool,
) -> Result<Option<PreparedEdits>, String> {
    if range.start >= range.end {
        return Err("Select text to transform.".to_string());
    }
    let mut text = String::new();
    text.try_reserve_exact(range.end.0 - range.start.0)
        .map_err(|_| NO_MEMORY)?;
    let good = snapshot.visit_chunks_while(range.clone(), &mut |part| {
        text.push_str(part);
        !cancel.load(Ordering::Acquire)
    });
    if cancel.load(Ordering::Acquire) {
        return Err("The transformation was cancelled.".to_string());
    }
    if !good {
        return Err(read_failure(snapshot));
    }
    let changed = transform_text(&text, transform)?;
    if changed == text {
        return Ok(None);
    }
    drop(text);
    prepare_edits(
        snapshot,
        &[TextEdit {
            range,
            text: changed,
        }],
        cancel,
    )
    .map(Some)
}
