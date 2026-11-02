use qe_api::{ByteOffset, ViewContext};
use qe_editor::{EditorCommand, EditorView};
use qe_tools::{
    Substitutions, Transform, prepare_replace_all, smart_paste, substitute_typed, transform_text,
};
use std::sync::atomic::AtomicBool;

#[test]
fn replace_all_is_one_undo_and_stale_is_refused() {
    let mut view = EditorView::from_text("aa bb aa").unwrap();
    let snap = view.worker_snapshot();
    let matches = vec![ByteOffset(0)..ByteOffset(2), ByteOffset(6)..ByteOffset(8)];
    let ready = prepare_replace_all(snap.as_ref(), &matches, "x", &AtomicBool::new(false)).unwrap();
    let mut ctx = ViewContext::default();
    view.apply_edits(&ready, ByteOffset(0)..ByteOffset(0), &mut ctx)
        .unwrap();
    assert_eq!(view.text().as_deref(), Some("x bb x"));
    view.run(EditorCommand::Undo, &mut ctx);
    assert_eq!(view.text().as_deref(), Some("aa bb aa"));
    assert!(
        view.apply_edits(&ready, ByteOffset(0)..ByteOffset(0), &mut ctx)
            .is_err()
    );
}

#[test]
fn unicode_transform_and_substitutions() {
    assert_eq!(
        transform_text("stra\u{df}e \u{130}", Transform::Upper),
        "STRASSE \u{130}"
    );
    assert_eq!(
        transform_text("\u{130} \u{df}", Transform::Lower),
        "i\u{307} \u{df}"
    );
    let opts = Substitutions {
        smart_copy: true,
        quotes: true,
        dashes: true,
    };
    assert_eq!(substitute_typed("-", "a-", opts), "\u{2014}");
    assert_eq!(substitute_typed("\"", "a ", opts), "\u{201c}");
    assert_eq!(smart_paste("hello", "a", "b", opts), " hello ");
}

#[test]
fn select_line_crlf_and_empty_and_read_only() {
    let mut ctx = ViewContext::default();
    let mut view = EditorView::from_text("a\r\nb\nc").unwrap();
    view.set_prevent_editing(true);
    qe_tools::select_lines(&mut view, "2-3", &mut ctx).unwrap();
    assert_eq!(view.selection(), ByteOffset(3)..ByteOffset(6));
    assert_eq!(view.text().as_deref(), Some("a\r\nb\nc"));
    let mut empty = EditorView::from_text("").unwrap();
    assert!(qe_tools::select_lines(&mut empty, "0", &mut ctx).is_err());
}

#[test]
fn read_only_refuses_replace_all() {
    let mut view = EditorView::from_text("aa aa").unwrap();
    let snap = view.worker_snapshot();
    let ready = prepare_replace_all(
        snap.as_ref(),
        &[ByteOffset(0)..ByteOffset(2), ByteOffset(3)..ByteOffset(5)],
        "x",
        &AtomicBool::new(false),
    )
    .unwrap();
    view.set_prevent_editing(true);
    let mut ctx = ViewContext::default();
    assert!(
        view.apply_edits(&ready, ByteOffset(0)..ByteOffset(0), &mut ctx)
            .is_err()
    );
    assert_eq!(view.text().as_deref(), Some("aa aa"));
}

#[test]
fn relative_line_forms_select_expected_bytes() {
    let mut view = EditorView::from_text("one\r\ntwo\nthree\u{2028}four").unwrap();
    let mut ctx = ViewContext::default();
    view.select(ByteOffset(5)..ByteOffset(5), &mut ctx);
    qe_tools::select_lines(&mut view, "+1", &mut ctx).unwrap();
    assert_eq!(view.selection(), ByteOffset(9)..ByteOffset(17));
    view.select(ByteOffset(9)..ByteOffset(9), &mut ctx);
    qe_tools::select_lines(&mut view, "-1", &mut ctx).unwrap();
    assert_eq!(view.selection(), ByteOffset(5)..ByteOffset(9));
}

#[test]
fn smart_cut_takes_one_adjacent_space() {
    let opts = Substitutions::default();
    assert_eq!(qe_tools::smart_cut_range("one two three", 4..7, opts), 3..7);
    assert_eq!(qe_tools::smart_cut_range("one two", 0..3, opts), 0..4);
    assert_eq!(
        qe_tools::smart_cut_range(
            "one two",
            4..7,
            Substitutions {
                smart_copy: false,
                ..opts
            }
        ),
        4..7
    );
}

#[test]
fn select_line_reports_mac_error_text() {
    let mut view = EditorView::from_text("one\ntwo").unwrap();
    let mut ctx = ViewContext::default();
    assert_eq!(
        qe_tools::select_lines(&mut view, "99", &mut ctx).unwrap_err(),
        "Invalid line specification \u{201c}99\u{201d}."
    );
}
