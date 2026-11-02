#![allow(non_upper_case_globals)]
//! Seeded random moves, checked against a plain model of the text. The model is built from
//! known clusters, so its stops are known without any Unicode rules. It shares no code with
//! the crate.
mod support;
use qe_api::{ByteOffset, ViewContext};
use qe_editor::{EditorCommand, EditorView};
use support::*;
use windows::Win32::UI::Accessibility::*;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Word,
    Space,
    Other,
    Line,
}
const ATOMS: &[(&str, Kind)] = &[
    ("a", Kind::Word),
    ("Z", Kind::Word),
    ("9", Kind::Word),
    ("_", Kind::Word),
    ("\u{4e2d}", Kind::Word),
    ("e\u{301}", Kind::Word),
    ("o\u{308}\u{323}", Kind::Word),
    (" ", Kind::Space),
    ("\t", Kind::Space),
    (",", Kind::Other),
    ("\u{1f600}", Kind::Other),
    ("\u{1f44d}\u{1f3fd}", Kind::Other),
    ("\u{1f1eb}\u{1f1f7}", Kind::Other),
    ("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}", Kind::Other),
    ("\r\n", Kind::Line),
    ("\n", Kind::Line),
];

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn count(&mut self) -> i32 {
        self.below(9) as i32 - 4
    }
}

#[derive(Clone)]
struct Model {
    atoms: Vec<usize>,
    /// First bytes of wrapped rows, as extra line stops in UTF-16.
    rows: Vec<usize>,
}

fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

impl Model {
    fn text(&self) -> String {
        self.atoms.iter().map(|a| ATOMS[*a].0).collect()
    }
    fn wide(&self) -> Vec<u16> {
        self.text().encode_utf16().collect()
    }
    fn len(&self) -> usize {
        self.atoms.iter().map(|a| units(ATOMS[*a].0)).sum()
    }
    fn starts(&self) -> Vec<(usize, usize)> {
        let mut at = 0;
        self.atoms
            .iter()
            .map(|a| {
                let here = (at, *a);
                at += units(ATOMS[*a].0);
                here
            })
            .collect()
    }
    fn char_stops(&self) -> Vec<usize> {
        let mut stops: Vec<usize> = self.starts().iter().map(|s| s.0).collect();
        stops.push(self.len());
        stops
    }
    fn paragraph_stops(&self) -> Vec<usize> {
        let mut stops = vec![0];
        for (at, atom) in self.starts() {
            if ATOMS[atom].1 == Kind::Line {
                stops.push(at + units(ATOMS[atom].0));
            }
        }
        if *stops.last().unwrap_or(&0) != self.len() {
            stops.push(self.len());
        }
        stops
    }
    fn line_stops(&self) -> Vec<usize> {
        let mut stops = self.paragraph_stops();
        stops.extend(self.rows.iter().copied());
        stops.sort_unstable();
        stops.dedup();
        stops
    }
    /// A word unit starts each line, at each run of word characters, and at each other
    /// character. Blank text belongs to the unit before it.
    fn word_stops(&self) -> Vec<usize> {
        let mut stops = vec![0];
        let mut before: Option<Kind> = None;
        for (at, atom) in self.starts() {
            let kind = ATOMS[atom].1;
            let starts_unit = match kind {
                Kind::Word => before != Some(Kind::Word),
                Kind::Other => true,
                Kind::Space => false,
                Kind::Line => false,
            };
            let at_line_start = before.is_none();
            if starts_unit || at_line_start {
                stops.push(at);
            }
            if kind == Kind::Line {
                stops.push(at + units(ATOMS[atom].0));
                before = None;
            } else {
                before = Some(kind);
            }
        }
        stops.push(self.len());
        stops.sort_unstable();
        stops.dedup();
        stops
    }
    fn stops(&self, unit: TextUnit) -> Vec<usize> {
        match unit {
            TextUnit_Character => self.char_stops(),
            TextUnit_Word => self.word_stops(),
            TextUnit_Line => self.line_stops(),
            TextUnit_Paragraph => self.paragraph_stops(),
            _ => vec![0, self.len()],
        }
    }
}

fn next_stop(stops: &[usize], at: usize) -> Option<usize> {
    stops.iter().copied().find(|s| *s > at)
}
fn previous_stop(stops: &[usize], at: usize) -> Option<usize> {
    stops.iter().copied().rev().find(|s| *s < at)
}

fn model_move(
    m: &Model,
    range: (usize, usize),
    unit: TextUnit,
    count: i32,
) -> ((usize, usize), i32) {
    if count == 0 {
        return (range, 0);
    }
    let stops = m.stops(unit);
    let len = m.len();
    let empty = range.0 == range.1;
    let mut at = range.0;
    let mut moved = 0;
    let whole = matches!(unit, TextUnit_Document | TextUnit_Page | TextUnit_Format);
    while moved != count {
        let step = if count > 0 {
            next_stop(&stops, at)
        } else {
            previous_stop(&stops, at)
        };
        let Some(step) = step else { break };
        let final_line =
            matches!(unit, TextUnit_Line | TextUnit_Paragraph) && len > 0 && m.ends_with_line();
        if !empty && count > 0 && step >= len && !final_line {
            break;
        }
        at = step;
        moved += count.signum();
        if whole {
            break;
        }
    }
    if moved == 0 {
        return (range, 0);
    }
    if empty {
        return ((at, at), moved);
    }
    ((at, next_stop(&stops, at).unwrap_or(len)), moved)
}

fn model_endpoint(
    m: &Model,
    range: (usize, usize),
    start: bool,
    unit: TextUnit,
    count: i32,
) -> ((usize, usize), i32) {
    let stops = m.stops(unit);
    let whole = matches!(unit, TextUnit_Document | TextUnit_Page | TextUnit_Format);
    let mut at = if start { range.0 } else { range.1 };
    let mut moved = 0;
    while moved != count {
        let step = if count > 0 {
            next_stop(&stops, at)
        } else {
            previous_stop(&stops, at)
        };
        let Some(step) = step else { break };
        at = step;
        moved += count.signum();
        if whole {
            break;
        }
    }
    let answer = if start {
        (at, range.1.max(at))
    } else {
        (range.0.min(at), at)
    };
    (answer, moved)
}

fn model_expand(m: &Model, at: usize, unit: TextUnit) -> (usize, usize) {
    let len = m.len();
    if matches!(unit, TextUnit_Document | TextUnit_Page | TextUnit_Format) {
        return (0, len);
    }
    let stops = m.stops(unit);
    if at >= len {
        let start = if matches!(unit, TextUnit_Line | TextUnit_Paragraph) {
            stops
                .iter()
                .copied()
                .rev()
                .find(|s| *s < len && !m.ends_with_line())
                .unwrap_or(len)
        } else {
            len
        };
        return (start, len);
    }
    let start = stops.iter().copied().rev().find(|s| *s <= at).unwrap_or(0);
    (start, next_stop(&stops, start).unwrap_or(len))
}

impl Model {
    fn ends_with_line(&self) -> bool {
        self.atoms.last().is_none_or(|a| ATOMS[*a].1 == Kind::Line)
    }
}

const UNITS: &[TextUnit] = &[
    TextUnit_Character,
    TextUnit_Format,
    TextUnit_Word,
    TextUnit_Line,
    TextUnit_Paragraph,
    TextUnit_Page,
    TextUnit_Document,
];

fn slice(wide: &[u16], range: (usize, usize)) -> String {
    String::from_utf16(&wide[range.0..range.1]).expect("a range inside a surrogate pair")
}

fn check(
    range: &ITextRangeProvider,
    doc: &ITextRangeProvider,
    wide: &[u16],
    want: (usize, usize),
    what: &str,
) {
    assert_eq!(ends(range, doc), want, "{what}");
    let text = unsafe { range.GetText(-1) }.expect("GetText").to_string();
    assert_eq!(text, slice(wide, want), "{what}");
}

/// Every range holds one model position, and the model moves it by the same rule.
fn run_moves(seed: u64, atoms: Vec<usize>, steps: usize) {
    let mut rng = Rng(seed);
    let model = Model {
        atoms,
        rows: Vec::new(),
    };
    let window = TestWindow::from_text(&model.text());
    let ui = window.accessibility();
    let document = ui.document().unwrap();
    let doc = document_range(&document);
    let wide = model.wide();
    let stops = model.char_stops();
    for step in 0..steps {
        let a = stops[rng.below(stops.len())];
        let b = stops[rng.below(stops.len())];
        let mut position = (a.min(b), a.max(b));
        let range = range_at(&document, position.0, position.1);
        for inner in 0..4 {
            let unit = UNITS[rng.below(UNITS.len())];
            let count = rng.count();
            let what =
                format!("seed {seed} step {step}.{inner} {unit:?} {count} from {position:?}");
            match rng.below(4) {
                0 => {
                    let (want, moved) = model_move(&model, position, unit, count);
                    assert_eq!(
                        unsafe { range.Move(unit, count) }.unwrap(),
                        moved,
                        "Move {what}"
                    );
                    position = want;
                }
                1 => {
                    let start = rng.below(2) == 0;
                    let (want, moved) = model_endpoint(&model, position, start, unit, count);
                    let end = if start {
                        TextPatternRangeEndpoint_Start
                    } else {
                        TextPatternRangeEndpoint_End
                    };
                    assert_eq!(
                        unsafe { range.MoveEndpointByUnit(end, unit, count) }.unwrap(),
                        moved,
                        "MoveEndpointByUnit start={start} {what}"
                    );
                    position = want;
                }
                2 => {
                    position = model_expand(&model, position.0, unit);
                    unsafe { range.ExpandToEnclosingUnit(unit) }.unwrap();
                }
                _ => {
                    let limit = rng.below(wide.len() + 3);
                    let got = unsafe { range.GetText(limit as i32) }.unwrap().to_string();
                    let want = slice_limited(&wide, position, limit);
                    assert_eq!(got, want, "GetText({limit}) {what}");
                }
            }
            check(&range, &doc, &wide, position, &what);
        }
    }
}

/// A limit that ends inside a surrogate pair stops before the pair.
fn slice_limited(wide: &[u16], range: (usize, usize), limit: usize) -> String {
    let mut end = range.0.saturating_add(limit).min(range.1);
    if end > range.0 && end < wide.len() && (0xDC00..0xE000).contains(&wide[end]) {
        end -= 1;
    }
    slice(wide, (range.0, end))
}

fn random_atoms(rng: &mut Rng, count: usize) -> Vec<usize> {
    (0..count).map(|_| rng.below(ATOMS.len())).collect()
}

#[test]
fn random_moves_match_the_model() {
    for seed in [1u64, 2, 3, 0xDEAD, 0xBEEF, 77_777] {
        let mut rng = Rng(seed * 7919 + 1);
        let count = 3 + rng.below(40);
        let atoms = random_atoms(&mut rng, count);
        run_moves(seed, atoms, 30);
    }
}

#[test]
fn random_moves_match_the_model_on_an_empty_document() {
    run_moves(11, Vec::new(), 10);
}

#[test]
fn random_moves_match_the_model_on_one_cluster() {
    run_moves(12, vec![13], 20);
    run_moves(13, vec![14], 20);
}

#[test]
fn random_line_moves_match_wrapped_rows() {
    let mut rng = Rng(424242);
    for round in 0..3 {
        let mut atoms = Vec::new();
        for _ in 0..6 {
            for _ in 0..(40 + rng.below(50)) {
                // Words, blanks, and emoji, with no line endings, so rows must wrap.
                atoms.push([0usize, 1, 4, 5, 7, 7, 10, 11][rng.below(8)]);
            }
            atoms.push(14 + rng.below(2));
        }
        let mut model = Model {
            atoms,
            rows: Vec::new(),
        };
        let mut view = EditorView::from_text(&model.text()).unwrap();
        view.set_wrap(true).unwrap();
        let window = TestWindow::new(view);
        window.lay_out(160.0, 3000.0);
        {
            let view = window.ui.view.borrow();
            let shown = view.visible_rows();
            assert!(shown.complete, "the layout settled");
            let wraps = shown.rows.iter().filter(|r| r.continuation).count();
            assert!(wraps >= 6, "round {round}: only {wraps} wrapped rows");
            model.rows = shown
                .rows
                .iter()
                .map(|r| view.byte_to_utf16(r.first_byte).unwrap().0)
                .collect();
        }
        let ui = window.accessibility();
        let document = ui.document().unwrap();
        let doc = document_range(&document);
        let wide = model.wide();
        // Rows break between clusters, so a row stop is a character stop.
        let chars = model.char_stops();
        for row in &model.rows {
            assert!(
                chars.contains(row),
                "a row starts inside a cluster at {row}"
            );
        }
        let stops = model.line_stops();
        for step in 0..120 {
            let at = stops[rng.below(stops.len())];
            let range = range_at(&document, at, at);
            let count = rng.count();
            let what = format!("round {round} step {step} from {at} count {count}");
            let (want, moved) = model_move(&model, (at, at), TextUnit_Line, count);
            assert_eq!(
                unsafe { range.Move(TextUnit_Line, count) }.unwrap(),
                moved,
                "{what}"
            );
            check(&range, &doc, &wide, want, &what);
            if want.0 < model.len() {
                unsafe { range.ExpandToEnclosingUnit(TextUnit_Line) }.unwrap();
                let end = next_stop(&stops, want.0).unwrap_or(model.len());
                check(&range, &doc, &wide, (want.0, end), &what);
                let (want, moved) = model_move(&model, (want.0, end), TextUnit_Line, count);
                assert_eq!(
                    unsafe { range.Move(TextUnit_Line, count) }.unwrap(),
                    moved,
                    "{what}"
                );
                check(&range, &doc, &wide, want, &what);
            }
        }
    }
}

fn shift(position: usize, start: usize, old_end: usize, new_end: usize) -> usize {
    if position <= start {
        position
    } else if position >= old_end {
        position - old_end + new_end
    } else {
        start
    }
}

/// Kept ranges through random edits, undo and redo. Each edit is checked against the model.
#[test]
fn ranges_kept_across_edits_and_undo_match_the_model() {
    for seed in [5u64, 6, 7, 8] {
        let mut rng = Rng(seed * 104_729 + 3);
        let count = 20 + rng.below(30);
        let mut model = Model {
            atoms: random_atoms(&mut rng, count),
            rows: Vec::new(),
        };
        let window = TestWindow::from_text(&model.text());
        let ui = window.accessibility();
        let document = ui.document().unwrap();
        let mut kept: Vec<(ITextRangeProvider, (usize, usize))> = Vec::new();
        for _ in 0..6 {
            let stops = model.char_stops();
            let a = stops[rng.below(stops.len())];
            let b = stops[rng.below(stops.len())];
            let spot = (a.min(b), a.max(b));
            kept.push((range_at(&document, spot.0, spot.1), spot));
        }
        // What the document looked like before each edit, and the edit in UTF-16, for undo.
        let mut undo: Vec<(Vec<usize>, (usize, usize, usize))> = Vec::new();
        let mut redo: Vec<(Vec<usize>, (usize, usize, usize))> = Vec::new();
        let mut ctx = ViewContext::default();
        for step in 0..60 {
            let what = format!("seed {seed} step {step}");
            let (start, old_end, new_end) = match rng.below(5) {
                0..=2 => {
                    // Replace some atoms with some others.
                    let at = rng.below(model.atoms.len() + 1);
                    let gone = rng.below(4).min(model.atoms.len() - at);
                    let fresh: Vec<usize> =
                        random_atoms(&mut rng, rng_small(&mut Rng(seed + step as u64)));
                    let before = model.atoms.clone();
                    let prefix: String = model.atoms[..at].iter().map(|a| ATOMS[*a].0).collect();
                    let old: String = model.atoms[at..at + gone]
                        .iter()
                        .map(|a| ATOMS[*a].0)
                        .collect();
                    let new: String = fresh.iter().map(|a| ATOMS[*a].0).collect();
                    if old.is_empty() && new.is_empty() {
                        continue;
                    }
                    window.ui.view.borrow_mut().replace_range(
                        ByteOffset(prefix.len())..ByteOffset(prefix.len() + old.len()),
                        &new,
                        &mut ctx,
                    );
                    model.atoms.splice(at..at + gone, fresh);
                    let change = (
                        units(&prefix),
                        units(&prefix) + units(&old),
                        units(&prefix) + units(&new),
                    );
                    undo.push((before, change));
                    redo.clear();
                    change
                }
                3 if !undo.is_empty() => {
                    window
                        .ui
                        .view
                        .borrow_mut()
                        .run(EditorCommand::Undo, &mut ctx);
                    let Some((before, change)) = undo.pop() else {
                        continue;
                    };
                    redo.push((model.atoms.clone(), change));
                    model.atoms = before;
                    // An undo swaps the new text back for the old.
                    (change.0, change.2, change.1)
                }
                4 if !redo.is_empty() => {
                    window
                        .ui
                        .view
                        .borrow_mut()
                        .run(EditorCommand::Redo, &mut ctx);
                    let Some((before, change)) = redo.pop() else {
                        continue;
                    };
                    undo.push((model.atoms.clone(), change));
                    model.atoms = before;
                    change
                }
                _ => continue,
            };
            assert_eq!(
                window.ui.view.borrow().text().unwrap(),
                model.text(),
                "{what}"
            );
            let wide = model.wide();
            let doc = document_range(&document);
            for (range, spot) in &mut kept {
                *spot = (
                    shift(spot.0, start, old_end, new_end),
                    shift(spot.1, start, old_end, new_end),
                );
                if spot.0 > spot.1 {
                    spot.1 = spot.0;
                }
                let found = ends(range, &doc);
                assert_eq!(found, *spot, "{what}");
                let text = unsafe { range.GetText(-1) }.unwrap().to_string();
                assert_eq!(text, slice(&wide, found), "{what}");
            }
        }
    }
}

fn rng_small(rng: &mut Rng) -> usize {
    rng.below(4)
}
