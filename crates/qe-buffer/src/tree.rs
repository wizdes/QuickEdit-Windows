//! An immutable balanced tree of small text leaves.
//!
//! Balance rule: the two children of a branch differ in height by at most 1.
//! `join` keeps that rule with single and double rotations. A leaf owns its
//! text, so deleting text releases it once nothing else holds that leaf.
use crate::measure::{Sum, measure};
use std::sync::Arc;

/// The most bytes a leaf holds.
pub(crate) const CHUNK: usize = 4096;

pub(crate) enum Kind {
    Leaf(Box<str>),
    Branch(Arc<Node>, Arc<Node>),
}

pub(crate) struct Node {
    pub kind: Kind,
    pub sum: Sum,
    pub height: u32,
    pub leaves: usize,
}

pub(crate) type Tree = Option<Arc<Node>>;

fn leaf_with(text: Box<str>, sum: Sum) -> Arc<Node> {
    Arc::new(Node {
        kind: Kind::Leaf(text),
        sum,
        height: 1,
        leaves: 1,
    })
}

fn leaf(text: &str) -> Arc<Node> {
    leaf_with(text.into(), measure(text))
}

fn branch(a: Arc<Node>, b: Arc<Node>) -> Arc<Node> {
    Arc::new(Node {
        sum: a.sum.add(b.sum),
        height: a.height.max(b.height) + 1,
        leaves: a.leaves + b.leaves,
        kind: Kind::Branch(a, b),
    })
}

fn children(n: &Node) -> (&Arc<Node>, &Arc<Node>) {
    match &n.kind {
        Kind::Branch(l, r) => (l, r),
        Kind::Leaf(_) => unreachable!("only a node taller than its sibling is opened"),
    }
}

/// Builds a branch from two balanced trees whose heights differ by at most 2.
fn balanced(l: Arc<Node>, r: Arc<Node>) -> Arc<Node> {
    if l.height > r.height + 1 {
        let (ll, lr) = children(&l);
        if ll.height >= lr.height {
            return branch(ll.clone(), branch(lr.clone(), r));
        }
        let (lrl, lrr) = children(lr);
        return branch(branch(ll.clone(), lrl.clone()), branch(lrr.clone(), r));
    }
    if r.height > l.height + 1 {
        let (rl, rr) = children(&r);
        if rr.height >= rl.height {
            return branch(branch(l, rl.clone()), rr.clone());
        }
        let (rll, rlr) = children(rl);
        return branch(branch(l, rll.clone()), branch(rlr.clone(), rr.clone()));
    }
    branch(l, r)
}

fn join_nodes(a: Arc<Node>, b: Arc<Node>) -> Arc<Node> {
    if a.height > b.height + 1 {
        let (l, r) = children(&a);
        return balanced(l.clone(), join_nodes(r.clone(), b));
    }
    if b.height > a.height + 1 {
        let (l, r) = children(&b);
        return balanced(join_nodes(a, l.clone()), r.clone());
    }
    branch(a, b)
}

pub(crate) fn join(a: Tree, b: Tree) -> Tree {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => Some(join_nodes(a, b)),
    }
}

pub(crate) fn split(tree: Tree, at: usize) -> (Tree, Tree) {
    let Some(n) = tree else { return (None, None) };
    if at == 0 {
        return (None, Some(n));
    }
    if at >= n.sum.bytes {
        return (Some(n), None);
    }
    match &n.kind {
        Kind::Leaf(text) => (Some(leaf(&text[..at])), Some(leaf(&text[at..]))),
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

pub(crate) fn from_text(text: &str) -> Tree {
    let mut parts = Vec::with_capacity(text.len() / CHUNK + 1);
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + CHUNK).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        parts.push(leaf(&text[start..end]));
        start = end;
    }
    build(&parts)
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

/// Makes a tree from text that arrives in parts, with no copy of the whole text.
#[derive(Default)]
pub(crate) struct TreeBuilder {
    parts: Vec<Arc<Node>>,
    pending: String,
}

impl TreeBuilder {
    pub fn push_str(&mut self, mut text: &str) {
        while !text.is_empty() {
            let room = CHUNK - self.pending.len();
            if text.len() < room {
                self.pending.push_str(text);
                return;
            }
            let mut end = room;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if self.pending.is_empty() {
                self.parts.push(leaf(&text[..end]));
            } else {
                self.pending.push_str(&text[..end]);
                self.parts.push(leaf(&self.pending));
                self.pending.clear();
            }
            text = &text[end..];
        }
    }
    pub fn finish(mut self) -> Tree {
        if !self.pending.is_empty() {
            self.parts.push(leaf(&self.pending));
        }
        build(&self.parts)
    }
}

fn last_leaf(mut n: &Node) -> (&str, Sum) {
    loop {
        match &n.kind {
            Kind::Leaf(text) => return (text, n.sum),
            Kind::Branch(_, r) => n = r,
        }
    }
}

fn first_leaf(mut n: &Node) -> (&str, Sum) {
    loop {
        match &n.kind {
            Kind::Leaf(text) => return (text, n.sum),
            Kind::Branch(l, _) => n = l,
        }
    }
}

/// Joins two trees and merges the two leaves that meet when they fit in one.
fn merge_join(a: Tree, b: Tree) -> Tree {
    let (a, b) = match (a, b) {
        (None, x) | (x, None) => return x,
        (Some(a), Some(b)) => (a, b),
    };
    let (left, left_sum) = last_leaf(&a);
    let (right, right_sum) = first_leaf(&b);
    if left.len() + right.len() > CHUNK {
        return Some(join_nodes(a, b));
    }
    let mut merged = String::with_capacity(left.len() + right.len());
    merged.push_str(left);
    merged.push_str(right);
    let middle = leaf_with(merged.into_boxed_str(), left_sum.add(right_sum));
    let keep_left = a.sum.bytes - left.len();
    let skip_right = right.len();
    let (rest_a, _) = split(Some(a.clone()), keep_left);
    let (_, rest_b) = split(Some(b.clone()), skip_right);
    join(join(rest_a, Some(middle)), rest_b)
}

/// Puts `middle` between two trees. Neighbour leaves merge when they fit.
pub(crate) fn splice(left: Tree, middle: Tree, right: Tree) -> Tree {
    merge_join(merge_join(left, middle), right)
}

/// `at` must not be 0. Returns `None` when `at` is past the end or inside a
/// character.
pub(crate) fn prefix(tree: &Tree, mut at: usize) -> Option<Sum> {
    let mut n = tree.as_ref()?;
    if at > n.sum.bytes {
        return None;
    }
    let mut before = Sum::default();
    loop {
        if at == n.sum.bytes {
            return Some(before.add(n.sum));
        }
        match &n.kind {
            Kind::Leaf(text) => return text.get(..at).map(|s| before.add(measure(s))),
            Kind::Branch(l, r) => {
                if at <= l.sum.bytes {
                    n = l;
                } else {
                    before = before.add(l.sum);
                    at -= l.sum.bytes;
                    n = r;
                }
            }
        }
    }
}

pub(crate) fn byte_at(tree: &Tree, mut at: usize) -> Option<u8> {
    let mut n = tree.as_ref()?;
    loop {
        match &n.kind {
            Kind::Leaf(text) => return text.as_bytes().get(at).copied(),
            Kind::Branch(l, r) => {
                if at < l.sum.bytes {
                    n = l;
                } else {
                    at -= l.sum.bytes;
                    n = r;
                }
            }
        }
    }
}

/// True when `at` is inside the text and on a character edge.
pub(crate) fn is_boundary(tree: &Tree, mut at: usize) -> bool {
    let Some(mut n) = tree.as_ref() else {
        return at == 0;
    };
    if at > n.sum.bytes {
        return false;
    }
    loop {
        if at == 0 || at == n.sum.bytes {
            return true;
        }
        match &n.kind {
            Kind::Leaf(text) => return text.is_char_boundary(at),
            Kind::Branch(l, r) => {
                if at <= l.sum.bytes {
                    n = l;
                } else {
                    at -= l.sum.bytes;
                    n = r;
                }
            }
        }
    }
}

/// Gives each chunk to `f` in order. Returns false as soon as `f` does.
pub(crate) fn visit(n: &Node, start: usize, end: usize, f: &mut dyn FnMut(&str) -> bool) -> bool {
    if start >= end {
        return true;
    }
    match &n.kind {
        Kind::Leaf(text) => f(&text[start..end]),
        Kind::Branch(l, r) => {
            if start < l.sum.bytes && !visit(l, start, end.min(l.sum.bytes), f) {
                return false;
            }
            if end > l.sum.bytes {
                return visit(r, start.saturating_sub(l.sum.bytes), end - l.sum.bytes, f);
            }
            true
        }
    }
}

/// The shape of a tree, as measured by `check`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeShape {
    pub leaves: usize,
    pub height: u32,
    pub longest_leaf: usize,
}

/// Walks the whole tree and checks every rule. It costs time in proportion to
/// the tree, so it is for tests.
pub(crate) fn check(tree: &Tree) -> Result<TreeShape, String> {
    fn walk(n: &Node, shape: &mut TreeShape) -> Result<(), String> {
        match &n.kind {
            Kind::Leaf(text) => {
                if text.is_empty() {
                    return Err("empty leaf".into());
                }
                if text.len() > CHUNK {
                    return Err(format!("leaf of {} bytes", text.len()));
                }
                if n.height != 1 || n.leaves != 1 {
                    return Err("wrong leaf height or count".into());
                }
                if n.sum != measure(text) {
                    return Err(format!("wrong leaf sum on {text:?}"));
                }
                shape.leaves += 1;
                shape.longest_leaf = shape.longest_leaf.max(text.len());
            }
            Kind::Branch(l, r) => {
                if l.height.abs_diff(r.height) > 1 {
                    return Err(format!(
                        "heights {} and {} under a branch",
                        l.height, r.height
                    ));
                }
                if n.height != l.height.max(r.height) + 1 {
                    return Err("wrong branch height".into());
                }
                if n.leaves != l.leaves + r.leaves {
                    return Err("wrong branch leaf count".into());
                }
                if n.sum != l.sum.add(r.sum) {
                    return Err("wrong branch sum".into());
                }
                walk(l, shape)?;
                walk(r, shape)?;
            }
        }
        Ok(())
    }
    let mut shape = TreeShape::default();
    if let Some(n) = tree {
        shape.height = n.height;
        walk(n, &mut shape)?;
    }
    Ok(shape)
}
