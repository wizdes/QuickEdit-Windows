//! Smart Copy/Paste, Smart Quotes and Smart Dashes.
//!
//! The Mac turns these on with `smartInsertDeleteEnabled`, `isAutomaticQuoteSubstitutionEnabled`
//! and `isAutomaticDashSubstitutionEnabled`. Their rules are in AppKit. Its source shows only
//! the defaults and one fact about Smart Copy: `FindAndLineTests` cuts a word selected from the
//! keyboard and pastes it, and no spaces change. So Smart Copy acts only on a word selected by
//! double-click.
use qe_api::ByteOffset;
use std::ops::Range;

/// Text over this size turns quotes and dashes off, as `largeFileThreshold` does on the Mac.
pub const SUBSTITUTION_LIMIT_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Substitutions {
    pub smart_copy: bool,
    pub quotes: bool,
    pub dashes: bool,
}
impl Default for Substitutions {
    /// Smart Copy on, quotes and dashes off, as in `Defaults.swift`.
    fn default() -> Self {
        Self {
            smart_copy: true,
            quotes: false,
            dashes: false,
        }
    }
}
impl Substitutions {
    /// What applies to a document of this size. Quotes and dashes turn off over 50 MiB, and
    /// the settings stay as they are. Smart Copy stays, as `turnOffChecks` leaves it on.
    pub fn for_document(self, bytes: u64) -> Self {
        if bytes > SUBSTITUTION_LIMIT_BYTES {
            Self {
                quotes: false,
                dashes: false,
                ..self
            }
        } else {
            self
        }
    }
}

/// A change to what was just typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypingEdit {
    /// Bytes to remove just before the caret.
    pub remove_before: usize,
    /// The text to put in place of those bytes and the typed text.
    pub insert: String,
}

/// True when a quote typed after this text opens.
fn opens_after(before: &str) -> bool {
    before.chars().next_back().is_none_or(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '(' | '['
                    | '{'
                    | '<'
                    | '\u{2014}'
                    | '\u{2013}'
                    | '\u{201c}'
                    | '\u{2018}'
                    | '-'
                    | '/'
            )
    })
}

/// What typing `typed` after `before` becomes, or `None` to keep it as it is. `before` needs
/// only its last characters. `--` gives `—`, and a quote opens at the start of the text,
/// after white space or after an opening mark.
pub fn substitute_typed(typed: &str, before: &str, options: Substitutions) -> Option<TypingEdit> {
    let edit = |remove_before: usize, insert: &str| {
        Some(TypingEdit {
            remove_before,
            insert: insert.to_string(),
        })
    };
    let opens = opens_after(before);
    match typed {
        "-" if options.dashes && before.ends_with('-') => edit(1, "\u{2014}"),
        "\"" if options.quotes => edit(0, if opens { "\u{201c}" } else { "\u{201d}" }),
        "'" if options.quotes => edit(0, if opens { "\u{2018}" } else { "\u{2019}" }),
        _ => None,
    }
}

fn opening_mark(c: char) -> bool {
    matches!(
        c,
        '(' | '[' | '{' | '<' | '"' | '\'' | '\u{201c}' | '\u{2018}' | '$' | '#'
    )
}
fn closing_mark(c: char) -> bool {
    matches!(
        c,
        ')' | ']'
            | '}'
            | '>'
            | '"'
            | '\''
            | '\u{201d}'
            | '\u{2019}'
            | '.'
            | ','
            | ';'
            | ':'
            | '!'
            | '?'
            | '%'
    )
}

/// The text a Paste puts in, with one space added on a side that needs it. `was_word` says
/// that the text was copied from a word selection. Anything else pastes as it is.
pub fn smart_paste(
    text: &str,
    before: Option<char>,
    after: Option<char>,
    options: Substitutions,
    was_word: bool,
) -> String {
    if !options.smart_copy || !was_word || text.is_empty() {
        return text.to_string();
    }
    let starts_clean = text
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace() && !closing_mark(c));
    let ends_clean = text
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace() && !opening_mark(c));
    let space_before =
        starts_clean && before.is_some_and(|c| !c.is_whitespace() && !opening_mark(c));
    let space_after = ends_clean && after.is_some_and(|c| !c.is_whitespace() && !closing_mark(c));
    let mut out = String::with_capacity(text.len() + 2);
    if space_before {
        out.push(' ');
    }
    out.push_str(text);
    if space_after {
        out.push(' ');
    }
    out
}

/// The bytes a Cut or Delete of `selected` removes. With a word selection, it also takes the
/// one space that would otherwise stand doubled or alone.
pub fn smart_delete_range(
    selected: Range<ByteOffset>,
    before: Option<char>,
    after: Option<char>,
    options: Substitutions,
    was_word: bool,
) -> Range<ByteOffset> {
    if !options.smart_copy || !was_word || selected.start >= selected.end {
        return selected;
    }
    let space = |c: Option<char>| c.is_some_and(|c| c == ' ' || c == '\t');
    let ends_after =
        after.is_none_or(|c| space(Some(c)) || c == '\r' || c == '\n' || closing_mark(c));
    if space(before) && ends_after {
        return ByteOffset(selected.start.0.saturating_sub(1))..selected.end;
    }
    if before.is_none_or(|c| c == '\n' || c == '\r') && space(after) {
        return selected.start..ByteOffset(selected.end.0 + 1);
    }
    selected
}
