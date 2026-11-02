//! Scroll positions for lines of uneven height.
//!
//! The map stores runs of lines that share one height, in a treap: a binary tree
//! kept balanced by a random priority on each run. A file with few measured lines
//! stays a few runs, whatever its line count. Set, insert, remove and both lookups
//! take O(log runs) expected time. Splits and merges recurse, to the tree's depth,
//! which `height_map_stays_balanced_at_a_million_runs` holds under a fixed limit.
//!
//! Removing lines only puts the root of the removed subtree on a free list, so it does
//! not visit the removed runs. Each new run takes one free node and puts that node's
//! children on the list. The memory stays with the map until new runs reuse it, so the
//! map's memory is bounded by the most runs it has ever held, at 48 bytes each.
use qe_api::{Dip, LineIndex};
use std::{fmt, ops::Range};

const NIL: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct Node {
    left: u32,
    right: u32,
    priority: u32,
    height: f32,
    count: u64,
    total_count: u64,
    total_height: f64,
    total_runs: u32,
}

/// Why the map refused a change. The map keeps its old value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HeightError {
    /// The height is NaN, infinite or negative.
    InvalidHeight(f32),
    /// The line or range is outside the map.
    OutOfRange { line: usize, len: usize },
}
impl fmt::Display for HeightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHeight(h) => write!(f, "invalid line height {h}"),
            Self::OutOfRange { line, len } => write!(f, "line {line} is outside {len} lines"),
        }
    }
}
impl std::error::Error for HeightError {}

fn checked(height: Dip) -> Result<f32, HeightError> {
    if height.0.is_finite() && height.0 >= 0.0 {
        // -0.0 becomes 0.0, so that equal heights compare equal by bits.
        Ok(height.0 + 0.0)
    } else {
        Err(HeightError::InvalidHeight(height.0))
    }
}

/// Line heights and scroll offsets. Offsets are `f64`, because an `f32` scroll
/// offset cannot tell lines apart in a large file. A view draws relative to the
/// top line, so `Dip` values stay small.
pub struct HeightMap {
    nodes: Vec<Node>,
    free: Vec<u32>,
    root: u32,
    estimate: f32,
    seed: u64,
}

impl HeightMap {
    pub fn new(lines: usize, estimate: Dip) -> Result<Self, HeightError> {
        let estimate = checked(estimate)?;
        let mut map = Self {
            nodes: Vec::new(),
            free: Vec::new(),
            root: NIL,
            estimate,
            seed: 0x9E37_79B9_7F4A_7C15,
        };
        map.root = map.node(lines as u64, estimate);
        Ok(map)
    }
    fn random(&mut self) -> u32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed >> 32) as u32
    }
    fn node(&mut self, count: u64, height: f32) -> u32 {
        if count == 0 {
            return NIL;
        }
        let priority = self.random();
        let value = Node {
            left: NIL,
            right: NIL,
            priority,
            height,
            count,
            total_count: count,
            total_height: count as f64 * height as f64,
            total_runs: 1,
        };
        if let Some(at) = self.free.pop() {
            let old = self.nodes[at as usize];
            if old.left != NIL {
                self.free.push(old.left);
            }
            if old.right != NIL {
                self.free.push(old.right);
            }
            self.nodes[at as usize] = value;
            at
        } else {
            self.nodes.push(value);
            (self.nodes.len() - 1) as u32
        }
    }
    fn lines_in(&self, at: u32) -> u64 {
        if at == NIL {
            0
        } else {
            self.nodes[at as usize].total_count
        }
    }
    fn height_in(&self, at: u32) -> f64 {
        if at == NIL {
            0.0
        } else {
            self.nodes[at as usize].total_height
        }
    }
    fn runs_in(&self, at: u32) -> u32 {
        if at == NIL {
            0
        } else {
            self.nodes[at as usize].total_runs
        }
    }
    fn pull(&mut self, at: u32) {
        let n = self.nodes[at as usize];
        let count = self.lines_in(n.left) + n.count + self.lines_in(n.right);
        let height =
            self.height_in(n.left) + n.count as f64 * n.height as f64 + self.height_in(n.right);
        let runs = self.runs_in(n.left) + 1 + self.runs_in(n.right);
        let node = &mut self.nodes[at as usize];
        node.total_count = count;
        node.total_height = height;
        node.total_runs = runs;
    }
    /// Splits off the first `lines` lines.
    fn split(&mut self, at: u32, lines: u64) -> (u32, u32) {
        if at == NIL {
            return (NIL, NIL);
        }
        let n = self.nodes[at as usize];
        let before = self.lines_in(n.left);
        if lines <= before {
            let (a, b) = self.split(n.left, lines);
            self.nodes[at as usize].left = b;
            self.pull(at);
            (a, at)
        } else if lines >= before + n.count {
            let (a, b) = self.split(n.right, lines - before - n.count);
            self.nodes[at as usize].right = a;
            self.pull(at);
            (at, b)
        } else {
            let keep = lines - before;
            // The second half gets its own random priority and goes through a real
            // merge. Copying this node's priority would make the tree a long chain.
            let rest = self.node(n.count - keep, n.height);
            let node = &mut self.nodes[at as usize];
            node.count = keep;
            node.right = NIL;
            self.pull(at);
            let tail = self.merge(rest, n.right);
            (at, tail)
        }
    }
    fn merge(&mut self, a: u32, b: u32) -> u32 {
        if a == NIL {
            return b;
        }
        if b == NIL {
            return a;
        }
        if self.nodes[a as usize].priority >= self.nodes[b as usize].priority {
            let right = self.merge(self.nodes[a as usize].right, b);
            self.nodes[a as usize].right = right;
            self.pull(a);
            a
        } else {
            let left = self.merge(a, self.nodes[b as usize].left);
            self.nodes[b as usize].left = left;
            self.pull(b);
            b
        }
    }
    fn edge_run(&self, mut at: u32, last: bool) -> Node {
        loop {
            let n = self.nodes[at as usize];
            let next = if last { n.right } else { n.left };
            if next == NIL {
                return n;
            }
            at = next;
        }
    }
    /// Joins two trees and folds the two runs at the seam when their heights match.
    fn join(&mut self, a: u32, b: u32) -> u32 {
        if a == NIL || b == NIL {
            return self.merge(a, b);
        }
        let last = self.edge_run(a, true);
        let first = self.edge_run(b, false);
        if last.height.to_bits() != first.height.to_bits() {
            return self.merge(a, b);
        }
        let (rest_a, run_a) = self.split(a, self.lines_in(a) - last.count);
        let (run_b, rest_b) = self.split(b, first.count);
        let node = &mut self.nodes[run_a as usize];
        node.count += first.count;
        self.pull(run_a);
        self.release(run_b);
        let joined = self.merge(rest_a, run_a);
        self.merge(joined, rest_b)
    }
    /// O(1). The next new runs take these nodes one at a time.
    fn release(&mut self, at: u32) {
        if at != NIL {
            self.free.push(at);
        }
    }
    pub fn len(&self) -> usize {
        self.lines_in(self.root) as usize
    }
    pub fn is_empty(&self) -> bool {
        self.root == NIL
    }
    /// The height of every line together.
    pub fn total_height(&self) -> f64 {
        self.height_in(self.root)
    }
    /// The depth of the tree. Tests use it to check the balance.
    #[doc(hidden)]
    pub fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut stack = vec![(self.root, 1)];
        while let Some((at, depth)) = stack.pop() {
            if at != NIL {
                deepest = deepest.max(depth);
                let n = self.nodes[at as usize];
                stack.push((n.left, depth + 1));
                stack.push((n.right, depth + 1));
            }
        }
        deepest
    }
    /// How many runs of equal height the map holds.
    pub fn run_count(&self) -> usize {
        self.runs_in(self.root) as usize
    }
    /// Bytes the map holds, including its spare capacity.
    pub fn memory_bytes(&self) -> usize {
        size_of::<Self>()
            + self.nodes.capacity() * size_of::<Node>()
            + self.free.capacity() * size_of::<u32>()
    }
    pub fn line_height(&self, line: LineIndex) -> Option<Dip> {
        let mut at = self.root;
        let mut skip = line.0 as u64;
        while at != NIL {
            let n = self.nodes[at as usize];
            let before = self.lines_in(n.left);
            if skip < before {
                at = n.left;
            } else if skip < before + n.count {
                return Some(Dip(n.height));
            } else {
                skip -= before + n.count;
                at = n.right;
            }
        }
        None
    }
    /// Sets one measured height. A NaN, infinite or negative height is an error
    /// and leaves the map unchanged.
    pub fn set(&mut self, line: LineIndex, height: Dip) -> Result<(), HeightError> {
        let value = checked(height)?;
        let Some(old) = self.line_height(line) else {
            return Err(HeightError::OutOfRange {
                line: line.0,
                len: self.len(),
            });
        };
        if old.0.to_bits() == value.to_bits() {
            return Ok(());
        }
        let (before, rest) = self.split(self.root, line.0 as u64);
        let (one, after) = self.split(rest, 1);
        self.nodes[one as usize].height = value;
        self.pull(one);
        let joined = self.join(before, one);
        self.root = self.join(joined, after);
        Ok(())
    }
    /// Inserts `count` lines of the estimated height before line `at`. Other
    /// lines keep their measured heights.
    pub fn insert_lines(&mut self, at: LineIndex, count: usize) -> Result<(), HeightError> {
        if at.0 > self.len() {
            return Err(HeightError::OutOfRange {
                line: at.0,
                len: self.len(),
            });
        }
        let (before, after) = self.split(self.root, at.0 as u64);
        let new = self.node(count as u64, self.estimate);
        let joined = self.join(before, new);
        self.root = self.join(joined, after);
        Ok(())
    }
    /// Removes a range of lines. Other lines keep their measured heights.
    pub fn remove_lines(&mut self, lines: Range<usize>) -> Result<(), HeightError> {
        if lines.start > lines.end || lines.end > self.len() {
            return Err(HeightError::OutOfRange {
                line: lines.end,
                len: self.len(),
            });
        }
        let (before, rest) = self.split(self.root, lines.start as u64);
        let (gone, after) = self.split(rest, (lines.end - lines.start) as u64);
        self.release(gone);
        self.root = self.join(before, after);
        Ok(())
    }
    /// The top of a line. A line past the end gives the total height.
    pub fn line_to_scroll(&self, line: LineIndex) -> f64 {
        let mut at = self.root;
        let mut skip = line.0 as u64;
        let mut top = 0.0;
        while at != NIL {
            let n = self.nodes[at as usize];
            let before = self.lines_in(n.left);
            if skip < before {
                at = n.left;
            } else if skip < before + n.count {
                return top + self.height_in(n.left) + (skip - before) as f64 * n.height as f64;
            } else {
                skip -= before + n.count;
                top += self.height_in(n.left) + n.count as f64 * n.height as f64;
                at = n.right;
            }
        }
        top
    }
    /// The line that holds a scroll offset. Offsets outside the map give the
    /// first or last line, and NaN gives the first. Uses the same sums as
    /// `line_to_scroll`, so `scroll_to_line(line_to_scroll(l))` gives `l` back
    /// for any line with a height above zero.
    pub fn scroll_to_line(&self, scroll: f64) -> LineIndex {
        if scroll.is_nan() {
            return LineIndex(0);
        }
        let mut at = self.root;
        let mut line = 0u64;
        let mut top = 0.0;
        while at != NIL {
            let n = self.nodes[at as usize];
            let start = top + self.height_in(n.left);
            let before = self.lines_in(n.left);
            if n.left != NIL && scroll < start {
                at = n.left;
                continue;
            }
            let height = n.height as f64;
            let end = start + n.count as f64 * height;
            if scroll < end {
                let mut row = (((scroll - start).max(0.0) / height) as u64).min(n.count - 1);
                if row > 0 && start + row as f64 * height > scroll {
                    row -= 1;
                } else if row + 1 < n.count && start + (row + 1) as f64 * height <= scroll {
                    row += 1;
                }
                return LineIndex((line + before + row) as usize);
            }
            line += before + n.count;
            top = end;
            at = n.right;
        }
        LineIndex((line as usize).min(self.len().saturating_sub(1)))
    }
}
