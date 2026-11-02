//! Which bytes have been checked. Sorted, with no overlap.
use std::ops::Range;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckedRanges(Vec<Range<usize>>);

impl CheckedRanges {
    pub fn from_ranges(ranges: Vec<Range<usize>>) -> Self {
        let mut out = Self::default();
        for range in ranges {
            out.add(range);
        }
        out
    }
    pub fn ranges(&self) -> &[Range<usize>] {
        &self.0
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
    pub fn add(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let first = self.0.partition_point(|r| r.end < range.start);
        let past = self.0.partition_point(|r| r.start <= range.end);
        let mut joined = range;
        if first < past {
            joined.start = joined.start.min(self.0[first].start);
            joined.end = joined.end.max(self.0[past - 1].end);
        }
        self.0.splice(first..past, [joined]);
    }
    /// The parts of `window` that are not checked.
    pub fn gaps(&self, window: &Range<usize>) -> Vec<Range<usize>> {
        let mut gaps = Vec::new();
        let mut at = window.start;
        let first = self.0.partition_point(|r| r.end <= window.start);
        for r in &self.0[first..] {
            if r.start >= window.end {
                break;
            }
            if r.start > at {
                gaps.push(at..r.start);
            }
            at = at.max(r.end);
        }
        if at < window.end {
            gaps.push(at..window.end);
        }
        gaps
    }
    /// An edit moves what is after it, and makes the bytes around it unchecked. One byte on
    /// each side is enough, because the worker grows a range out to whole words.
    pub fn edit(&mut self, start: usize, old_end: usize, new_len: usize) {
        let dirty_start = start.saturating_sub(1);
        let dirty_end = old_end.saturating_add(1);
        let shift = |x: usize| (x + new_len).saturating_sub(old_end - start);
        let mut out = Vec::with_capacity(self.0.len() + 1);
        for r in self.0.drain(..) {
            if r.start < dirty_start {
                out.push(r.start..r.end.min(dirty_start));
            }
            if r.end > dirty_end {
                out.push(shift(r.start.max(dirty_end))..shift(r.end));
            }
        }
        self.0 = out;
    }
    pub fn keep_only(&mut self, keep: &Range<usize>) {
        self.0 = self
            .0
            .iter()
            .filter_map(|r| {
                let cut = r.start.max(keep.start)..r.end.min(keep.end);
                (!cut.is_empty()).then_some(cut)
            })
            .collect();
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[test]
    fn adding_joins_what_touches() {
        let mut c = CheckedRanges::default();
        c.add(10..20);
        c.add(30..40);
        c.add(20..30);
        assert_eq!(c.ranges(), &[10..40]);
        c.add(0..5);
        c.add(3..11);
        assert_eq!(c.ranges(), &[0..40]);
        c.add(50..50);
        assert_eq!(c.ranges(), &[0..40]);
    }

    #[test]
    fn gaps_are_what_the_window_still_needs() {
        let mut c = CheckedRanges::default();
        assert_eq!(c.gaps(&(5..10)), vec![5..10]);
        c.add(0..7);
        c.add(9..12);
        assert_eq!(c.gaps(&(5..10)), vec![7..9]);
        assert_eq!(c.gaps(&(0..7)), Vec::<Range<usize>>::new());
        assert_eq!(c.gaps(&(11..20)), vec![12..20]);
    }

    #[test]
    fn an_edit_cuts_a_hole_and_moves_the_rest() {
        let mut c = CheckedRanges::from_ranges(vec![0..100]);
        // Replaces 2 bytes at 50 with 5.
        c.edit(50, 52, 5);
        assert_eq!(c.ranges(), &[0..49, 56..103]);
        assert_eq!(c.gaps(&(0..103)), vec![49..56]);
        // A range wholly before stays, and one wholly after moves.
        let mut c = CheckedRanges::from_ranges(vec![0..10, 60..70]);
        c.edit(40, 40, 3);
        assert_eq!(c.ranges(), &[0..10, 63..73]);
        // A deletion at the very start.
        let mut c = CheckedRanges::from_ranges(vec![0..10]);
        c.edit(0, 2, 0);
        assert_eq!(c.ranges(), &[1..8]);
    }
}
