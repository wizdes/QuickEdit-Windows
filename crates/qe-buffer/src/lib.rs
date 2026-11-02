//! UTF-8 text storage with immutable tree nodes.
use qe_api::{
    ByteOffset, EditError, LineIndex, TextChange, TextPosition, TextSnapshot, TextStore,
    Utf16Offset,
};
use std::ops::Range;
use std::sync::Arc;

const CHUNK: usize = 4096;
#[derive(Clone, Copy, Default)]
struct Sum {
    bytes: usize,
    units: usize,
    breaks: usize,
    first_lf: bool,
    last_cr: bool,
}
impl Sum {
    fn add(self, b: Self) -> Self {
        if self.bytes == 0 {
            return b;
        }
        if b.bytes == 0 {
            return self;
        }
        Self {
            bytes: self.bytes + b.bytes,
            units: self.units + b.units,
            breaks: self.breaks + b.breaks - usize::from(self.last_cr && b.first_lf),
            first_lf: self.first_lf,
            last_cr: b.last_cr,
        }
    }
}
fn measure(s: &str) -> Sum {
    Sum {
        bytes: s.len(),
        units: s.encode_utf16().count(),
        breaks: s
            .chars()
            .filter(|c| matches!(c, '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}'))
            .count()
            - s.as_bytes().windows(2).filter(|w| *w == b"\r\n").count(),
        first_lf: s.starts_with('\n'),
        last_cr: s.ends_with('\r'),
    }
}
#[derive(Clone)]
struct Piece {
    text: Arc<str>,
    start: usize,
    end: usize,
}
impl Piece {
    fn get(&self) -> &str {
        &self.text[self.start..self.end]
    }
}
enum Kind {
    Leaf(Piece),
    Branch(Arc<Node>, Arc<Node>),
}
struct Node {
    kind: Kind,
    sum: Sum,
    height: u8,
}
type Tree = Option<Arc<Node>>;
fn leaf(p: Piece) -> Arc<Node> {
    Arc::new(Node {
        sum: measure(p.get()),
        kind: Kind::Leaf(p),
        height: 1,
    })
}
fn branch(a: Arc<Node>, b: Arc<Node>) -> Arc<Node> {
    Arc::new(Node {
        sum: a.sum.add(b.sum),
        height: a.height.max(b.height) + 1,
        kind: Kind::Branch(a, b),
    })
}
fn join(a: Tree, b: Tree) -> Tree {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => {
            if a.height > b.height + 1
                && let Kind::Branch(ref l, ref r) = a.kind
            {
                let mid = join(Some(r.clone()), Some(b)).unwrap();
                if mid.height > l.height + 1
                    && let Kind::Branch(ref ml, ref mr) = mid.kind
                {
                    return Some(branch(branch(l.clone(), ml.clone()), mr.clone()));
                }
                return Some(branch(l.clone(), mid));
            }
            if b.height > a.height + 1
                && let Kind::Branch(ref l, ref r) = b.kind
            {
                let mid = join(Some(a), Some(l.clone())).unwrap();
                if mid.height > r.height + 1
                    && let Kind::Branch(ref ml, ref mr) = mid.kind
                {
                    return Some(branch(ml.clone(), branch(mr.clone(), r.clone())));
                }
                return Some(branch(mid, r.clone()));
            }
            Some(branch(a, b))
        }
    }
}
fn split(tree: Tree, at: usize) -> (Tree, Tree) {
    let Some(n) = tree else { return (None, None) };
    if at == 0 {
        return (None, Some(n));
    }
    if at >= n.sum.bytes {
        return (Some(n), None);
    }
    match &n.kind {
        Kind::Leaf(p) => (
            Some(leaf(Piece {
                text: p.text.clone(),
                start: p.start,
                end: p.start + at,
            })),
            Some(leaf(Piece {
                text: p.text.clone(),
                start: p.start + at,
                end: p.end,
            })),
        ),
        Kind::Branch(l, r) => {
            if at < l.sum.bytes {
                let (a, b) = split(Some(l.clone()), at);
                (a, join(b, Some(r.clone())))
            } else {
                let (a, b) = split(Some(r.clone()), at - l.sum.bytes);
                (join(Some(l.clone()), a), b)
            }
        }
    }
}
fn from_text(text: Arc<str>) -> Tree {
    let mut parts = Vec::with_capacity(text.len() / CHUNK + 1);
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + CHUNK).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        parts.push(leaf(Piece {
            text: text.clone(),
            start,
            end,
        }));
        start = end;
    }
    fn build(p: &[Arc<Node>]) -> Tree {
        match p.len() {
            0 => None,
            1 => Some(p[0].clone()),
            n => Some(branch(
                build(&p[..n / 2]).unwrap(),
                build(&p[n / 2..]).unwrap(),
            )),
        }
    }
    build(&parts)
}
fn prefix(tree: &Tree, at: usize) -> Option<Sum> {
    let n = tree.as_ref()?;
    if at > n.sum.bytes {
        return None;
    }
    if at == n.sum.bytes {
        return Some(n.sum);
    }
    match &n.kind {
        Kind::Leaf(p) => p.get().get(..at).map(measure),
        Kind::Branch(l, r) => {
            if at <= l.sum.bytes {
                prefix(&Some(l.clone()), at)
            } else {
                Some(l.sum.add(prefix(&Some(r.clone()), at - l.sum.bytes)?))
            }
        }
    }
}
fn byte_at(tree: &Tree, at: usize) -> Option<u8> {
    let n = tree.as_ref()?;
    match &n.kind {
        Kind::Leaf(p) => p.get().as_bytes().get(at).copied(),
        Kind::Branch(l, r) => {
            if at < l.sum.bytes {
                byte_at(&Some(l.clone()), at)
            } else {
                byte_at(&Some(r.clone()), at - l.sum.bytes)
            }
        }
    }
}
fn visit(tree: &Tree, start: usize, end: usize, f: &mut dyn FnMut(&str)) {
    let Some(n) = tree else {
        return;
    };
    if start == end {
        return;
    }
    match &n.kind {
        Kind::Leaf(p) => f(&p.get()[start..end]),
        Kind::Branch(l, r) => {
            if start < l.sum.bytes {
                visit(&Some(l.clone()), start, end.min(l.sum.bytes), f);
            }
            if end > l.sum.bytes {
                visit(
                    &Some(r.clone()),
                    start.saturating_sub(l.sum.bytes),
                    end - l.sum.bytes,
                    f,
                );
            }
        }
    }
}
#[derive(Clone, Default)]
pub struct PieceStore {
    root: Tree,
    revision: u64,
}
impl PieceStore {
    pub fn new(text: &str) -> Self {
        Self {
            root: from_text(Arc::from(text)),
            revision: 0,
        }
    }
    /// Converts UTF-8 and builds balanced chunks on a worker thread.
    pub fn build_async(
        bytes: Vec<u8>,
    ) -> std::thread::JoinHandle<Result<Self, std::string::FromUtf8Error>> {
        std::thread::spawn(move || String::from_utf8(bytes).map(|s| Self::new(&s)))
    }
    fn sum(&self) -> Sum {
        self.root.as_ref().map_or(Sum::default(), |n| n.sum)
    }
    fn breaks_before(&self, at: usize) -> Option<usize> {
        let s = if at == 0 {
            Sum::default()
        } else {
            prefix(&self.root, at)?
        };
        Some(s.breaks - usize::from(s.last_cr && byte_at(&self.root, at) == Some(b'\n')))
    }
    fn line_start(&self, line: usize) -> Option<usize> {
        if line == 0 {
            return Some(0);
        }
        if line > self.sum().breaks {
            return None;
        }
        fn find(n: &Node, mut rank: usize, next_lf: bool, base: usize) -> Option<usize> {
            match &n.kind {
                Kind::Leaf(p) => {
                    let mut chars = p.get().char_indices().peekable();
                    while let Some((at, c)) = chars.next() {
                        let next = chars.peek().is_some_and(|(_, c)| *c == '\n')
                            || (chars.peek().is_none() && next_lf);
                        if matches!(c, '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}')
                            && !(c == '\r' && next)
                        {
                            rank -= 1;
                            if rank == 0 {
                                return Some(base + at + c.len_utf8());
                            }
                        }
                    }
                    None
                }
                Kind::Branch(l, r) => {
                    let left_breaks = l.sum.breaks - usize::from(l.sum.last_cr && r.sum.first_lf);
                    if rank <= left_breaks {
                        find(l, rank, r.sum.first_lf, base)
                    } else {
                        find(r, rank - left_breaks, next_lf, base + l.sum.bytes)
                    }
                }
            }
        }
        find(self.root.as_ref()?, line, false, 0)
    }
}
impl TextSnapshot for PieceStore {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn len_bytes(&self) -> ByteOffset {
        ByteOffset(self.sum().bytes)
    }
    fn len_utf16(&self) -> Utf16Offset {
        Utf16Offset(self.sum().units)
    }
    fn line_count(&self) -> usize {
        self.sum().breaks + 1
    }
    fn byte_to_utf16(&self, byte: ByteOffset) -> Option<Utf16Offset> {
        if byte.0 == 0 {
            return Some(Utf16Offset(0));
        }
        prefix(&self.root, byte.0).map(|s| Utf16Offset(s.units))
    }
    fn utf16_to_byte(&self, utf16: Utf16Offset) -> Option<ByteOffset> {
        if utf16.0 > self.sum().units {
            return None;
        }
        if self.root.is_none() {
            return Some(ByteOffset(0));
        }
        let mut n = self.root.as_ref().unwrap();
        let mut units = utf16.0;
        let mut base = 0;
        loop {
            match &n.kind {
                Kind::Leaf(p) => {
                    let mut count = 0;
                    for (i, c) in p.get().char_indices() {
                        if count == units {
                            return Some(ByteOffset(base + i));
                        }
                        count += c.len_utf16();
                        if count > units {
                            return None;
                        }
                    }
                    return (count == units).then_some(ByteOffset(base + p.get().len()));
                }
                Kind::Branch(l, r) => {
                    if units <= l.sum.units {
                        n = l;
                    } else {
                        units -= l.sum.units;
                        base += l.sum.bytes;
                        n = r;
                    }
                }
            }
        }
    }
    fn byte_to_position(&self, byte: ByteOffset) -> Option<TextPosition> {
        let units = self.byte_to_utf16(byte)?.0;
        let line = self.breaks_before(byte.0)?;
        let start = self.line_start(line)?;
        Some(TextPosition {
            line: LineIndex(line),
            column: Utf16Offset(units - self.byte_to_utf16(ByteOffset(start))?.0),
        })
    }
    fn position_to_byte(&self, pos: TextPosition) -> Option<ByteOffset> {
        let range = self.line_range(pos.line)?;
        let units = self
            .byte_to_utf16(range.start)?
            .0
            .checked_add(pos.column.0)?;
        let byte = self.utf16_to_byte(Utf16Offset(units))?;
        (byte <= range.end).then_some(byte)
    }
    fn line_range(&self, line: LineIndex) -> Option<Range<ByteOffset>> {
        let start = self.line_start(line.0)?;
        let end = self.line_start(line.0 + 1).unwrap_or(self.sum().bytes);
        Some(ByteOffset(start)..ByteOffset(end))
    }
    fn visit_chunks(&self, range: Range<ByteOffset>, f: &mut dyn FnMut(&str)) -> bool {
        if range.start > range.end
            || range.end.0 > self.sum().bytes
            || self.byte_to_utf16(range.start).is_none()
            || self.byte_to_utf16(range.end).is_none()
        {
            return false;
        }
        visit(&self.root, range.start.0, range.end.0, f);
        true
    }
    fn is_read_only(&self) -> bool {
        false
    }
}
impl TextStore for PieceStore {
    fn snapshot(&self) -> Arc<dyn TextSnapshot> {
        Arc::new(self.clone())
    }
    fn replace(&mut self, range: Range<ByteOffset>, text: &str) -> Result<TextChange, EditError> {
        if range.start > range.end
            || range.end.0 > self.sum().bytes
            || self.byte_to_utf16(range.start).is_none()
            || self.byte_to_utf16(range.end).is_none()
        {
            return Err(EditError::InvalidRange);
        }
        let before = self.line_count();
        let (left, rest) = split(self.root.clone(), range.start.0);
        let (_, right) = split(rest, range.end.0 - range.start.0);
        self.root = join(join(left, from_text(Arc::from(text))), right);
        self.revision += 1;
        Ok(TextChange {
            replaced: range,
            new_len: text.len(),
            line_count_delta: self.line_count() as isize - before as isize,
            revision: self.revision,
        })
    }
}

struct Edit {
    start: usize,
    old: String,
    new: String,
}
struct Group {
    edits: Vec<Edit>,
}

/// Stores changed text only. Consecutive typing is one undo step.
#[derive(Default)]
pub struct UndoHistory {
    undo: Vec<Group>,
    redo: Vec<Group>,
    typing_end: Option<usize>,
}
impl UndoHistory {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn break_group(&mut self) {
        self.typing_end = None;
    }
    pub fn cursor_moved(&mut self) {
        self.break_group();
    }
    pub fn save_point(&mut self) {
        self.break_group();
    }
    pub fn undo_groups(&self) -> usize {
        self.undo.len()
    }
    pub fn memory_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(&self.redo)
            .flat_map(|g| &g.edits)
            .map(|e| e.old.len() + e.new.len() + std::mem::size_of::<Edit>())
            .sum()
    }
    pub fn replace(
        &mut self,
        store: &mut PieceStore,
        range: Range<ByteOffset>,
        text: &str,
        typing: bool,
    ) -> Result<TextChange, EditError> {
        let mut old = String::new();
        if !store.visit_chunks(range.clone(), &mut |s| old.push_str(s)) {
            return Err(EditError::InvalidRange);
        }
        let change = store.replace(range.clone(), text)?;
        let continuous = typing && old.is_empty() && self.typing_end == Some(range.start.0);
        if !continuous {
            self.undo.push(Group { edits: Vec::new() });
        }
        self.undo.last_mut().unwrap().edits.push(Edit {
            start: range.start.0,
            old,
            new: text.to_owned(),
        });
        self.typing_end = if typing && range.start == range.end {
            Some(range.start.0 + text.len())
        } else {
            None
        };
        self.redo.clear();
        Ok(change)
    }
    pub fn undo(&mut self, store: &mut PieceStore) -> Result<bool, EditError> {
        self.break_group();
        let Some(group) = self.undo.pop() else {
            return Ok(false);
        };
        for e in group.edits.iter().rev() {
            store.replace(
                ByteOffset(e.start)..ByteOffset(e.start + e.new.len()),
                &e.old,
            )?;
        }
        self.redo.push(group);
        Ok(true)
    }
    pub fn redo(&mut self, store: &mut PieceStore) -> Result<bool, EditError> {
        self.break_group();
        let Some(group) = self.redo.pop() else {
            return Ok(false);
        };
        for e in &group.edits {
            store.replace(
                ByteOffset(e.start)..ByteOffset(e.start + e.old.len()),
                &e.new,
            )?;
        }
        self.undo.push(group);
        Ok(true)
    }
}

#[cfg(windows)]
mod mapped;
#[cfg(windows)]
pub use mapped::MappedStore;
