//! Transformations: `Make Upper Case`, `Make Lower Case` and `Capitalize`.
//!
//! The Mac calls `uppercaseWord:`, `lowercaseWord:` and `capitalizeWord:`. Their code is in
//! AppKit, so its source shows only the labels and that a read-only view refuses them. The
//! case rules here are Unicode's with no locale, as `NSString` has them.
use crate::text::{is_extend, read_failure};
use qe_api::{ByteOffset, TextSnapshot};
use qe_editor::{PreparedEdits, TextEdit, prepare_edits};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transform {
    Upper,
    Lower,
    Capitalize,
}

fn title_case(c: char, out: &mut String) {
    // The four digraphs have their own title forms, and `ß` titles as `Ss`.
    match c {
        '\u{1c4}'..='\u{1c6}' => out.push('\u{1c5}'),
        '\u{1c7}'..='\u{1c9}' => out.push('\u{1c8}'),
        '\u{1ca}'..='\u{1cc}' => out.push('\u{1cb}'),
        '\u{1f1}'..='\u{1f3}' => out.push('\u{1f2}'),
        _ => {
            for (i, upper) in c.to_uppercase().enumerate() {
                if i == 0 {
                    out.push(upper);
                } else {
                    out.extend(upper.to_lowercase());
                }
            }
        }
    }
}

/// True for a character that leaves a word open: marks and the apostrophes.
fn keeps_word(c: char) -> bool {
    is_extend(c) || matches!(c, '\'' | '\u{2019}' | '\u{2bc}' | '\u{b7}')
}

/// The new text. Letters start a word and any other character ends it, so `1st` gives
/// `1St`, and an apostrophe leaves a word open, so `don't` gives `Don't`.
pub fn transform_text(text: &str, transform: Transform) -> String {
    match transform {
        Transform::Upper => text.to_uppercase(),
        Transform::Lower => text.to_lowercase(),
        Transform::Capitalize => {
            let mut out = String::with_capacity(text.len());
            let mut rest = text;
            while let Some(c) = rest.chars().next() {
                let tail = rest.get(c.len_utf8()..).unwrap_or("");
                if !c.is_alphabetic() {
                    out.push(c);
                    rest = tail;
                    continue;
                }
                let word_len = tail
                    .char_indices()
                    .find(|(_, n)| !n.is_alphabetic() && !keeps_word(*n))
                    .map_or(tail.len(), |(at, _)| at);
                title_case(c, &mut out);
                // Lower case as one piece, so a final sigma stays final.
                out.push_str(&tail.get(..word_len).unwrap_or("").to_lowercase());
                rest = tail.get(word_len..).unwrap_or("");
            }
            out
        }
    }
}

/// Reads the range and makes one edit of the new text. Run it on a worker for a long range.
pub fn prepare_transform(
    snapshot: &dyn TextSnapshot,
    range: Range<ByteOffset>,
    transform: Transform,
    cancel: &AtomicBool,
) -> Result<PreparedEdits, String> {
    if range.start >= range.end {
        return Err("Select text to transform.".to_string());
    }
    let mut text = String::new();
    text.try_reserve_exact(range.end.0 - range.start.0)
        .map_err(|_| "There is not enough memory to transform this text.".to_string())?;
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
    prepare_edits(
        snapshot,
        &[TextEdit {
            range,
            text: transform_text(&text, transform),
        }],
        cancel,
    )
}
