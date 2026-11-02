//! Replace All hands a finished tree to the UI thread. The budget test needs a release build.
use qe_api::{ByteOffset, Dip, InputEvent, InputKind, Modifiers, Rect, View, ViewContext};
use qe_editor::{ChangesSince, DecorationKind, EditorView, PreparedEdits, TextEdit, prepare_edits};
use qe_layout::DirectWriteLayout;
use std::{ops::Range, sync::atomic::AtomicBool, time::Duration};

static GO_ON: AtomicBool = AtomicBool::new(false);

fn open(text: &str) -> EditorView {
    let mut e = EditorView::from_text(text).unwrap();
    let measure = DirectWriteLayout::new().unwrap();
    e.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(800.0),
            height: Dip(600.0),
        },
        &measure,
    );
    settle(&mut e);
    e
}
fn settle(e: &mut EditorView) {
    let mut context = ViewContext::default();
    e.poll_layout(&mut context);
    for _ in 0..5000 {
        if !e.pending_layout() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
        context.requests.clear();
        e.poll_layout(&mut context);
    }
    panic!("the view did not settle");
}
fn ctrl_key(e: &mut EditorView, code: u16) {
    e.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::KeyDown {
                virtual_key: code,
                modifiers: Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
                repeat: false,
            },
        },
        &mut ViewContext::default(),
    );
}
const UNDO: u16 = 0x5a;
const REDO: u16 = 0x59;
fn edit(start: usize, end: usize, text: &str) -> TextEdit {
    TextEdit {
        range: ByteOffset(start)..ByteOffset(end),
        text: text.to_string(),
    }
}
fn prepared(e: &EditorView, edits: &[TextEdit]) -> PreparedEdits {
    let snapshot = e.worker_snapshot();
    prepare_edits(snapshot.as_ref(), edits, &GO_ON).unwrap()
}
fn caret(at: usize) -> std::ops::Range<ByteOffset> {
    ByteOffset(at)..ByteOffset(at)
}
const LINE: &str =
    "the quick brown fox jumps over the lazy dog, and \u{4e2d}\u{6587}\u{1f600} too\n";

/// Each match in `text`, replaced, with a plain `String` to compare against.
fn replaced_by_hand(text: &str, from: &str, to: &str) -> (Vec<TextEdit>, String) {
    let edits = text
        .match_indices(from)
        .map(|(at, _)| edit(at, at + from.len(), to))
        .collect();
    (edits, text.replace(from, to))
}

#[test]
fn a_replace_all_over_many_leaves_gives_exact_text_and_one_undo_step() {
    // About 300 KiB, which is many leaves of the store, with characters of 1, 3 and 4 bytes,
    // so that a cut between leaves would fall inside one if it could.
    let text = LINE.repeat(4_000);
    let (edits, expected) = replaced_by_hand(&text, "fox", "\u{1f98a}\u{4e2d}");
    let mut e = open(&text);
    let prepared = prepared(&e, &edits);
    assert_eq!(prepared.edit_count(), 4_000);
    let first = edits[0].range.start;
    let last = edits[edits.len() - 1].range.end;
    assert_eq!(prepared.replaced_range(), first..last);
    let before = e.revision();
    e.apply_edits(&prepared, caret(3), &mut ViewContext::default())
        .unwrap();
    assert_eq!(e.text().unwrap(), expected);
    assert_eq!(e.selection(), caret(3));
    assert_eq!(e.line_count(), 4_001);
    assert_eq!(
        prepared.new_len(),
        last.0 - first.0 + (expected.len() - text.len())
    );
    // A reader of the log sees one change, with the right sizes in bytes and in UTF-16.
    let ChangesSince::Changes(changes) = e.changes_since(before) else {
        panic!("the log has this change");
    };
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].change.replaced, first..last);
    assert_eq!(changes[0].change.new_len, prepared.new_len());
    let new_units = expected[first.0..first.0 + prepared.new_len()]
        .encode_utf16()
        .count();
    assert_eq!(
        changes[0].utf16.new_end.0 - changes[0].utf16.start.0,
        new_units
    );
    // One undo step for all 4,000, and the redo of it.
    ctrl_key(&mut e, UNDO);
    assert_eq!(e.text().unwrap(), text);
    ctrl_key(&mut e, REDO);
    assert_eq!(e.text().unwrap(), expected);
    ctrl_key(&mut e, UNDO);
    assert_eq!(e.text().unwrap(), text);
}

#[test]
fn a_copy_of_a_prepared_edit_holds_the_same_text() {
    // `HostCommand::ApplyEdits` clones its edits.
    let mut e = open("aa bb aa");
    let edits = [edit(0, 2, "x"), edit(6, 8, "yy")];
    let prepared = prepared(&e, &edits);
    let copy = prepared.clone();
    assert_eq!(copy, prepared);
    e.apply_edits(&copy, caret(0), &mut ViewContext::default())
        .unwrap();
    assert_eq!(e.text().unwrap(), "x bb yy");
    assert_eq!(prepared.new_len(), 7);
    assert!(
        e.apply_edits(&prepared, caret(0), &mut ViewContext::default())
            .is_err(),
        "the text moved on, so the same edits are refused"
    );
}

#[test]
fn edits_that_empty_a_range_or_only_insert_still_work() {
    let mut e = open("abc");
    let cut = prepared(&e, &[edit(0, 3, "")]);
    assert_eq!(cut.new_len(), 0);
    e.apply_edits(&cut, caret(0), &mut ViewContext::default())
        .unwrap();
    assert_eq!(e.text().unwrap(), "");
    let add = prepared(&e, &[edit(0, 0, "\r\nz")]);
    e.apply_edits(&add, caret(2), &mut ViewContext::default())
        .unwrap();
    assert_eq!(e.text().unwrap(), "\r\nz");
    assert_eq!(e.line_count(), 2);
    ctrl_key(&mut e, UNDO);
    ctrl_key(&mut e, UNDO);
    assert_eq!(e.text().unwrap(), "abc");
}

#[test]
fn text_placed_after_a_cr_joins_it_into_one_line_ending() {
    let mut e = open("a\rb\nc");
    let joined = prepared(&e, &[edit(2, 3, "\n")]);
    assert_eq!(e.line_count(), 3);
    e.apply_edits(&joined, caret(0), &mut ViewContext::default())
        .unwrap();
    assert_eq!(e.text().unwrap(), "a\r\n\nc");
    assert_eq!(e.line_count(), 3);
    settle(&mut e);
    ctrl_key(&mut e, UNDO);
    assert_eq!(e.text().unwrap(), "a\rb\nc");
    assert_eq!(e.line_count(), 3);
}

#[test]
fn marks_follow_the_same_rule_as_a_plain_list_after_any_edits() {
    // A small seeded generator, so that a failure repeats.
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |below: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % below.max(1) as u64) as usize
    };
    for _ in 0..60 {
        let mut e = open(&"abcdefghij".repeat(30));
        let mut model: Vec<Range<usize>> = Vec::new();
        let mut at = 0;
        while at < 280 {
            at += next(12);
            let end = at + 1 + next(6);
            model.push(at..end);
            at = end + 1 + next(4);
        }
        e.set_decorations(
            DecorationKind::FindMatch,
            model
                .iter()
                .map(|r| ByteOffset(r.start)..ByteOffset(r.end))
                .collect(),
            &mut ViewContext::default(),
        );
        let mut len = e.text().unwrap().len();
        for _ in 0..40 {
            let start = next(len + 1);
            let end = (start + next(25)).min(len);
            let added = next(5);
            if start == end && added == 0 {
                // No edit at all, so no mark moves.
                continue;
            }
            e.replace_range(
                ByteOffset(start)..ByteOffset(end),
                &"Z".repeat(added),
                &mut ViewContext::default(),
            );
            len = len - (end - start) + added;
            // The rule in `HOOKS.md`.
            let delta = added as isize - (end - start) as isize;
            model.retain_mut(|mark| {
                if mark.end <= start {
                    true
                } else if mark.start >= end {
                    mark.start = (mark.start as isize + delta) as usize;
                    mark.end = (mark.end as isize + delta) as usize;
                    true
                } else {
                    false
                }
            });
            let got: Vec<Range<usize>> = e
                .decorations(DecorationKind::FindMatch)
                .iter()
                .map(|r| r.start.0..r.end.0)
                .collect();
            assert_eq!(got, model, "after {start}..{end} with {added}");
        }
    }
}

#[test]
fn a_refused_edit_can_go_to_a_worker_to_free() {
    let mut e = open("abc");
    let stale = prepared(&e, &[edit(0, 1, "x")]);
    e.replace_range(
        ByteOffset(2)..ByteOffset(2),
        "y",
        &mut ViewContext::default(),
    );
    assert!(
        e.apply_edits(&stale, caret(0), &mut ViewContext::default())
            .is_err()
    );
    e.discard_edits(stale);
    assert_eq!(e.text().unwrap(), "abyc");
    assert_eq!(e.warning(), None);
}

/// The budget itself. On a loaded PC one run can be slow, so the best of 5 counts.
#[cfg(not(debug_assertions))]
#[test]
fn release_budget_replace_all_is_under_2_ms_on_the_ui_thread_and_33_ms_to_the_screen() {
    use std::time::Instant;
    let line =
        "the quick brown fox jumps over the lazy dog 0123456789 and then some more words go here\n";
    let lines = 100_000_000 / line.len();
    let text = line.repeat(lines);
    let step = lines / 100_000;
    let edits: Vec<_> = (0..100_000)
        .map(|i| {
            let at = i * step * line.len() + 4;
            edit(at, at + 5, "slow")
        })
        .collect();
    let mut e = open(&text);
    let marks: Vec<_> = edits.iter().map(|e| e.range.clone()).collect();
    let (mut apply, mut screen, mut undo) = (f64::MAX, f64::MAX, f64::MAX);
    for _ in 0..5 {
        e.set_decorations(
            DecorationKind::FindMatch,
            marks.clone(),
            &mut ViewContext::default(),
        );
        let ready = prepared(&e, &edits);
        let started = Instant::now();
        e.apply_edits(&ready, caret(0), &mut ViewContext::default())
            .unwrap();
        drop(ready);
        apply = apply.min(started.elapsed().as_secs_f64() * 1000.0);
        settle(&mut e);
        screen = screen.min(started.elapsed().as_secs_f64() * 1000.0);
        let started = Instant::now();
        ctrl_key(&mut e, UNDO);
        undo = undo.min(started.elapsed().as_secs_f64() * 1000.0);
        settle(&mut e);
    }
    eprintln!("best of 5: apply {apply:.3} ms, to the screen {screen:.3} ms, undo {undo:.3} ms");
    assert!(apply < 2.0, "apply took {apply:.3} ms");
    assert!(screen < 33.0, "the screen took {screen:.3} ms");
    assert!(undo < 8.0, "undo took {undo:.3} ms");
}
