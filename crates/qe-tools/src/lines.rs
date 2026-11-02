//! Select Line's parser and its line rules, from `LinePanelController.swift` and the
//! `LinePanelController.m` it ports.
use qe_api::{ByteOffset, LineIndex, LookupError};
use qe_editor::EditorView;
use std::ops::Range;

pub const SELECT_LINE_SIZE: (f32, f32) = (253.0, 62.0);
pub const SELECT_LINE_FIELD: (f32, f32, f32, f32) = (20.0, 20.0, 138.0, 22.0);
pub const SELECT_LINE_BUTTON: (f32, f32, f32, f32) = (162.0, 13.0, 77.0, 32.0);
pub const LINE_PLACEHOLDER: &str = "Line number";
pub const LINE_TOOLTIP: &str = "Type a line number (e.g., 5), line number range (e.g., 10-20), or relative line number (e.g., -5).";
pub const LINE_SUGGESTION: &str = "Enter a line number or a range of line numbers to select.";

/// An entry: `N`, `N-M`, `+N`, `-N`, `+N-M` or `-N-M`. `relative` is 1 for `+`, -1 for
/// `-`, and 0 for an absolute line, where the first line is 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineSpec {
    pub from: i64,
    pub to: i64,
    pub relative: i8,
}

/// `NSScanner` skips white space before each item, and `isAtEnd` ignores it after the last.
struct Scanner<'a>(&'a str);
impl Scanner<'_> {
    fn skip(&mut self) {
        self.0 = self.0.trim_start();
    }
    fn take(&mut self, sign: char) -> bool {
        self.skip();
        match self.0.strip_prefix(sign) {
            Some(rest) => {
                self.0 = rest;
                true
            }
            None => false,
        }
    }
    /// `scanInt`: an optional sign and digits. On overflow it gives the largest or smallest
    /// value and moves past all the digits.
    fn int(&mut self) -> Option<i64> {
        self.skip();
        let negative = self.0.starts_with('-');
        let unsigned = self.0.strip_prefix(['+', '-']).unwrap_or(self.0);
        let digits = unsigned.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let mut value: i64 = 0;
        for digit in unsigned.bytes().take(digits) {
            let digit = i64::from(digit - b'0');
            value = value.saturating_mul(10);
            value = if negative {
                value.saturating_sub(digit)
            } else {
                value.saturating_add(digit)
            };
        }
        self.0 = unsigned.get(digits..)?;
        Some(value)
    }
}

impl LineSpec {
    /// A port of `parseLineDescription:`. Anything left over is an error.
    pub fn parse(input: &str) -> Option<Self> {
        let mut scan = Scanner(input);
        let relative = if scan.take('+') {
            1
        } else if scan.take('-') {
            -1
        } else {
            0
        };
        let from = scan.int()?;
        let mut to = from;
        if scan.take('-') {
            let end = scan.int()?;
            if end < from {
                return None;
            }
            to = end;
        }
        scan.skip();
        scan.0.is_empty().then_some(Self { from, to, relative })
    }
}

/// `Invalid line specification “…”.`, cut as TextEdit's `truncatedString` cuts it: at 100
/// UTF-16 units or more, the first 90 and an ellipsis.
pub fn line_error_message(input: &str) -> String {
    let units = input.encode_utf16().count();
    let shown = if units < 100 {
        input.to_string()
    } else {
        let mut taken = 0;
        let mut out = String::new();
        for c in input.chars() {
            taken += c.len_utf16();
            if taken > 90 {
                break;
            }
            out.push(c);
        }
        out.push('\u{2026}');
        out
    };
    format!("Invalid line specification \u{201c}{shown}\u{201d}.")
}

/// Why lines could not be chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineError {
    /// The entry is bad, or those lines are not in the text.
    Invalid,
    /// The index of a mapped file has not reached those lines. Ask again later.
    NotYetKnown {
        indexed_up_to: ByteOffset,
    },
    Failed(String),
}
impl From<LookupError> for LineError {
    fn from(error: LookupError) -> Self {
        match error {
            LookupError::NotYetKnown { indexed_up_to } => Self::NotYetKnown { indexed_up_to },
            LookupError::OutOfRange | LookupError::TooLong { .. } => Self::Invalid,
            LookupError::Failed(failure) => Self::Failed(failure.message),
        }
    }
}

/// The bytes of the lines an entry names. A port of `LineSpec.range(in:selection:)`, on
/// the line index in place of a scan, so its cost does not grow with the number. It never
/// waits for disk: a mapped file still indexing gives `NotYetKnown`.
pub fn lines_for_spec(view: &EditorView, spec: LineSpec) -> Result<Range<ByteOffset>, LineError> {
    let (from, to) = (i128::from(spec.from), i128::from(spec.to));
    // The Mac refuses a span that overflows `Int`.
    if to - from >= i128::from(i64::MAX) {
        return Err(LineError::Invalid);
    }
    let count = to - from + 1;
    let mut current: i128 = 0;
    if spec.relative != 0 {
        let selection = view.selection();
        let at = if spec.relative > 0 {
            selection.end
        } else {
            selection.start
        };
        let mut line = view.line_of_byte(at)?.0;
        // The Mac looks one unit back from the end of a selection, which is the same as
        // the line before when the selection ends at the start of a line.
        if spec.relative > 0
            && selection.start < selection.end
            && line > 0
            && view.line_range(LineIndex(line))?.start == at
        {
            line -= 1;
        }
        current = line as i128;
    } else if from == 0 {
        return Err(LineError::Invalid);
    }

    let index = |line: i128| -> Result<LineIndex, LineError> {
        usize::try_from(line)
            .map(LineIndex)
            .map_err(|_| LineError::Invalid)
    };
    if spec.relative < 0 {
        let back = if from > 0 { from - 1 } else { 0 };
        let end_line = current - back;
        let begin_line = end_line - count;
        if begin_line < 0 {
            return Err(LineError::Invalid);
        }
        let begin = view.line_range(index(begin_line)?)?.start;
        let end = view.line_range(index(end_line)?)?.start;
        return Ok(begin..end);
    }
    let skip = if spec.relative == 0 { from - 1 } else { from }.max(0);
    let first = current + skip;
    let last = first + count - 1;
    let last_range = view.line_range(index(last)?)?;
    // Nothing follows a last line ending: the store's empty last line is not a line here.
    if last_range.start >= last_range.end {
        return Err(LineError::Invalid);
    }
    let begin = view.line_range(index(first)?)?.start;
    Ok(begin..last_range.end)
}
