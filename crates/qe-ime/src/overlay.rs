//! The text that TSF sees while a composition runs.
//!
//! TSF expects a composition's text to be in the document. The editor keeps that text out
//! of its store until the commit. So the store shows TSF the editor's text with one
//! replaced span, at the UTF-16 positions where TSF wrote it.
use std::ops::Range;

/// Where a TSF position falls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// At or before the span's start: the same position in the editor.
    Before(usize),
    /// Inside the span: units from its start.
    Inside(usize),
    /// At or after the span's end: this position in the editor.
    After(usize),
}

#[derive(Clone, Debug)]
pub(crate) struct Overlay {
    /// The editor's UTF-16 units that this span hides.
    pub(crate) base: Range<usize>,
    /// What TSF sees in their place.
    pub(crate) text: Vec<u16>,
    /// What the hidden units hold, to tell a real change from no change at the end.
    pub(crate) original: Vec<u16>,
    /// The selection, in TSF positions.
    pub(crate) selection: Range<usize>,
    pub(crate) active_start: bool,
    pub(crate) interim: bool,
}

impl Overlay {
    pub(crate) fn new(base: Range<usize>, original: Vec<u16>) -> Self {
        let end = base.start + original.len();
        Self {
            base,
            text: original.clone(),
            original,
            selection: end..end,
            active_start: false,
            interim: false,
        }
    }
    /// The span's end in TSF positions.
    pub(crate) fn end(&self) -> usize {
        self.base.start + self.text.len()
    }
    /// The whole document's length in TSF positions, given the editor's length.
    pub(crate) fn len(&self, editor_len: usize) -> usize {
        editor_len.saturating_sub(self.base.end - self.base.start) + self.text.len()
    }
    pub(crate) fn place(&self, position: usize) -> Place {
        if position <= self.base.start {
            Place::Before(position)
        } else if position < self.end() {
            Place::Inside(position - self.base.start)
        } else {
            Place::After(position - self.end() + self.base.end)
        }
    }
    /// An editor position in TSF positions. A position the span hides maps to its start.
    pub(crate) fn editor_to_tsf(&self, position: usize) -> usize {
        if position <= self.base.start {
            position
        } else if position < self.base.end {
            self.base.start
        } else {
            position - self.base.end + self.end()
        }
    }
    /// Grows the span so that it holds `before` to its left and `after` to its right.
    pub(crate) fn absorb(&mut self, before: &[u16], after: &[u16]) {
        self.base.start -= before.len();
        self.base.end += after.len();
        for side in [&mut self.text, &mut self.original] {
            side.splice(0..0, before.iter().copied());
            side.extend_from_slice(after);
        }
    }
    /// Replaces TSF positions inside the span. The selection stays with its text.
    pub(crate) fn replace(&mut self, range: Range<usize>, new: &[u16]) {
        let start = range
            .start
            .saturating_sub(self.base.start)
            .min(self.text.len());
        let end = range
            .end
            .saturating_sub(self.base.start)
            .clamp(start, self.text.len());
        self.text.splice(start..end, new.iter().copied());
        let replaced = self.base.start + start..self.base.start + end;
        self.selection = selection_after_write(&self.selection, &replaced, new.len());
        if self.selection.is_empty() {
            self.active_start = false;
        }
    }
    /// The composition's cursor, for `ime_update`.
    pub(crate) fn cursor(&self) -> usize {
        let at = if self.active_start {
            self.selection.start
        } else {
            self.selection.end
        };
        at.saturating_sub(self.base.start).min(self.text.len())
    }
}

/// Where a selection goes when a write replaces `range` with `new_len` units. A selection
/// away from the write stays with its text. One that is the write's own range lands after the
/// new text, and so does a place inside it.
pub(crate) fn selection_after_write(
    selection: &Range<usize>,
    range: &Range<usize>,
    new_len: usize,
) -> Range<usize> {
    let new_end = range.start + new_len;
    if selection == range {
        return new_end..new_end;
    }
    let place = |at: usize| {
        if at <= range.start {
            at
        } else if at >= range.end {
            at - range.end + new_end
        } else {
            new_end
        }
    };
    place(selection.start)..place(selection.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn positions_map_around_a_span_that_grew() {
        let mut o = Overlay::new(2..2, Vec::new());
        o.replace(2..2, &units("xyz"));
        assert_eq!(o.place(2), Place::Before(2));
        assert_eq!(o.place(3), Place::Inside(1));
        assert_eq!(o.place(5), Place::After(2));
        assert_eq!(o.place(7), Place::After(4));
        assert_eq!(o.len(10), 13);
        assert_eq!(o.selection, 5..5);
        assert_eq!(o.cursor(), 3);
        assert_eq!(o.editor_to_tsf(4), 7);
    }

    #[test]
    fn a_span_that_replaces_text_hides_it() {
        let mut o = Overlay::new(1..4, units("abc"));
        assert_eq!(o.len(10), 10);
        o.replace(1..4, &units("z"));
        assert_eq!(o.len(10), 8);
        assert_eq!(o.place(2), Place::After(4));
        assert_eq!(o.editor_to_tsf(2), 1);
        assert_eq!(o.editor_to_tsf(4), 2);
        assert_eq!(o.editor_to_tsf(9), 7);
    }

    #[test]
    fn absorbing_keeps_the_text_in_order() {
        let mut o = Overlay::new(3..3, Vec::new());
        o.replace(3..3, &units("k"));
        o.absorb(&units("ab"), &units("c"));
        assert_eq!(o.base, 1..4);
        assert_eq!(String::from_utf16_lossy(&o.text), "abkc");
        assert_eq!(String::from_utf16_lossy(&o.original), "abc");
    }
}
