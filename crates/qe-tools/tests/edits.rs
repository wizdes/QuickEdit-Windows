//! Replace All against a plain string, transformations, Complete's list and substitutions.
use qe_api::{ByteOffset, ViewContext};
use qe_editor::{EditorCommand, EditorView};
use qe_tools::{
    FindOptions, MatchKind, Matcher, Substitutions, Transform, TypingEdit, complete_candidates,
    prepare_replace_all, prepare_transform, smart_delete_range, smart_paste, substitute_typed,
    transform_text,
};
use std::sync::atomic::AtomicBool;

static NOT_CANCELLED: AtomicBool = AtomicBool::new(false);

fn replace_all(view: &mut EditorView, query: &str, with: &str, o: FindOptions) -> Option<u64> {
    let snapshot = view.worker_snapshot();
    let matcher = Matcher::new(query, o).unwrap();
    let ready = prepare_replace_all(snapshot.as_ref(), &matcher, with, &NOT_CANCELLED).unwrap()?;
    let mut ctx = ViewContext::default();
    view.apply_edits(
        &ready.prepared,
        ready.caret_after..ready.caret_after,
        &mut ctx,
    )
    .unwrap();
    Some(ready.count)
}

#[test]
fn replace_all_ignoring_case_as_in_the_mac_ui_test() {
    let mut view = EditorView::from_text("1 Fox 2 fox 3 FOX").unwrap();
    assert_eq!(
        replace_all(&mut view, "fox", "cat", FindOptions::default()),
        Some(3)
    );
    assert_eq!(view.text().as_deref(), Some("1 cat 2 cat 3 cat"));
}

#[test]
fn replace_all_matches_a_plain_string_on_seeded_text() {
    let parts = [
        "foo",
        "FOO",
        "fo",
        " ",
        "\r\n",
        "e\u{301}",
        "\u{1f600}",
        "x",
        "\u{4e2d}",
        "oof",
    ];
    let mut seed = 0x1234_5678_u64;
    for round in 0..60 {
        let mut text = String::new();
        for _ in 0..(10 + round * 40) {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            text.push_str(parts[seed as usize % parts.len()]);
        }
        for with in ["", "z", "foofoo", "\u{1f600}"] {
            let exact = FindOptions {
                ignore_case: false,
                wrap: true,
                kind: MatchKind::Contains,
            };
            let mut view = EditorView::from_text(&text).unwrap();
            let want_count = text.matches("foo").count() as u64;
            let got = replace_all(&mut view, "foo", with, exact);
            assert_eq!(got.unwrap_or(0), want_count);
            assert_eq!(
                view.text().unwrap(),
                text.replace("foo", with),
                "{with:?} in {text:?}"
            );
        }
    }
}

#[test]
fn replace_all_over_a_batch_edge_counts_and_keeps_all() {
    // More matches than one batch holds, with text between them.
    let text = "ab-".repeat(10_000);
    let mut view = EditorView::from_text(&text).unwrap();
    assert_eq!(
        replace_all(&mut view, "ab", "XYZ", FindOptions::default()),
        Some(10_000)
    );
    assert_eq!(view.text().unwrap(), "XYZ-".repeat(10_000));
}

#[test]
fn replace_all_is_one_undo_step_and_redo_gives_it_back() {
    let start = "aa bb aa \u{4e2d}aa";
    let mut view = EditorView::from_text(start).unwrap();
    assert_eq!(
        replace_all(&mut view, "aa", "x", FindOptions::default()),
        Some(3)
    );
    let done = "x bb x \u{4e2d}x";
    assert_eq!(view.text().as_deref(), Some(done));
    let mut ctx = ViewContext::default();
    view.run(EditorCommand::Undo, &mut ctx);
    assert_eq!(view.text().as_deref(), Some(start));
    view.run(EditorCommand::Redo, &mut ctx);
    assert_eq!(view.text().as_deref(), Some(done));
}

#[test]
fn replace_all_with_no_match_prepares_nothing() {
    let view = EditorView::from_text("abc").unwrap();
    let snapshot = view.worker_snapshot();
    let matcher = Matcher::new("zzz", FindOptions::default()).unwrap();
    let ready = prepare_replace_all(snapshot.as_ref(), &matcher, "x", &NOT_CANCELLED).unwrap();
    assert!(ready.is_none());
}

#[test]
fn a_stale_or_read_only_result_is_refused_and_changes_nothing() {
    let mut view = EditorView::from_text("aa bb aa").unwrap();
    let snapshot = view.worker_snapshot();
    let matcher = Matcher::new("aa", FindOptions::default()).unwrap();
    let ready = prepare_replace_all(snapshot.as_ref(), &matcher, "x", &NOT_CANCELLED)
        .unwrap()
        .unwrap();
    let mut ctx = ViewContext::default();
    view.replace_range(ByteOffset(2)..ByteOffset(2), "!", &mut ctx);
    assert!(
        view.apply_edits(&ready.prepared, ByteOffset(0)..ByteOffset(0), &mut ctx)
            .is_err()
    );
    assert_eq!(view.text().as_deref(), Some("aa! bb aa"));

    let mut view = EditorView::from_text("aa").unwrap();
    let snapshot = view.worker_snapshot();
    let ready = prepare_replace_all(snapshot.as_ref(), &matcher, "x", &NOT_CANCELLED)
        .unwrap()
        .unwrap();
    view.set_prevent_editing(true);
    assert!(
        view.apply_edits(&ready.prepared, ByteOffset(0)..ByteOffset(0), &mut ctx)
            .is_err()
    );
    assert_eq!(view.text().as_deref(), Some("aa"));
    assert_eq!(view.refused_edits(), 1);
}

#[test]
fn a_cancelled_replace_all_says_so() {
    let view = EditorView::from_text(&"aa ".repeat(1000)).unwrap();
    let snapshot = view.worker_snapshot();
    let matcher = Matcher::new("aa", FindOptions::default()).unwrap();
    let cancelled = AtomicBool::new(true);
    let result = prepare_replace_all(snapshot.as_ref(), &matcher, "x", &cancelled);
    assert!(result.unwrap_err().contains("cancelled"));
}

#[test]
fn transform_table() {
    use Transform::{Capitalize, Lower, Upper};
    let cases = [
        ("hello wORLD", Upper, "HELLO WORLD"),
        ("HELLO WORLD", Lower, "hello world"),
        ("hello world", Capitalize, "Hello World"),
        ("hello wORLD", Capitalize, "Hello World"),
        // `ß`, with no locale.
        ("stra\u{df}e", Upper, "STRASSE"),
        ("\u{df}", Capitalize, "Ss"),
        ("\u{1e9e}", Lower, "\u{df}"),
        // Turkish letters, with no locale: the Windows language does not matter.
        ("i\u{131}", Upper, "II"),
        ("\u{130}", Lower, "i\u{307}"),
        ("I", Lower, "i"),
        // A final sigma stays final, in Capitalize too.
        ("\u{3a3}\u{391}\u{3a3}", Lower, "\u{3c3}\u{3b1}\u{3c2}"),
        (
            "\u{3a3}\u{391}\u{3a3} x",
            Capitalize,
            "\u{3a3}\u{3b1}\u{3c2} X",
        ),
        // Digraphs have a title form.
        ("\u{1c6}x", Capitalize, "\u{1c5}x"),
        // Marks and emoji stay.
        ("e\u{301}cole \u{1f600}x", Upper, "E\u{301}COLE \u{1f600}X"),
        ("e\u{301}COLE", Capitalize, "E\u{301}cole"),
        // Word edges.
        ("a-b c_d\te\nf", Capitalize, "A-B C_D\tE\nF"),
        ("1st don't", Capitalize, "1St Don't"),
        ("", Upper, ""),
    ];
    for (text, how, want) in cases {
        assert_eq!(transform_text(text, how), want, "{text:?} {how:?}");
    }
}

#[test]
fn a_transform_is_one_edit_on_a_real_view_and_keeps_crlf() {
    let start = "ab\r\ncd\u{df}";
    let mut view = EditorView::from_text(start).unwrap();
    let snapshot = view.worker_snapshot();
    let range = ByteOffset(0)..ByteOffset(start.len());
    let ready =
        prepare_transform(snapshot.as_ref(), range, Transform::Upper, &NOT_CANCELLED).unwrap();
    let mut ctx = ViewContext::default();
    view.apply_edits(&ready, ByteOffset(0)..ByteOffset(ready.new_len()), &mut ctx)
        .unwrap();
    assert_eq!(view.text().as_deref(), Some("AB\r\nCDSS"));
    view.run(EditorCommand::Undo, &mut ctx);
    assert_eq!(view.text().as_deref(), Some(start));
}

#[test]
fn complete_words_table() {
    let text = "Hello help helmet he hello HELP other h\u{e9}llo e\u{301}x help_me";
    let view = EditorView::from_text(text).unwrap();
    let snapshot = view.worker_snapshot();
    let words =
        complete_candidates(snapshot.as_ref(), "hel", ByteOffset(0), &NOT_CANCELLED).unwrap();
    assert_eq!(
        words,
        ["Hello", "hello", "helmet", "HELP", "help", "help_me"].map(String::from)
    );
    assert!(
        complete_candidates(snapshot.as_ref(), "", ByteOffset(0), &NOT_CANCELLED)
            .unwrap()
            .is_empty()
    );
    let none =
        complete_candidates(snapshot.as_ref(), "zzz", ByteOffset(0), &NOT_CANCELLED).unwrap();
    assert!(none.is_empty());
}

#[test]
fn complete_stays_near_the_caret_and_bounded() {
    // A word far from the caret is out of the window, and the list has a limit.
    let far = qe_tools::COMPLETE_WINDOW_BYTES + 1000;
    let mut text = String::from("alphafar ");
    text.push_str(&"x ".repeat(far / 2));
    let at = text.len();
    text.push_str("alphanear");
    let view = EditorView::from_text(&text).unwrap();
    let snapshot = view.worker_snapshot();
    let words =
        complete_candidates(snapshot.as_ref(), "alpha", ByteOffset(at), &NOT_CANCELLED).unwrap();
    assert_eq!(words, ["alphanear"]);

    let many: String = (0..1000).map(|i| format!("w{i} ")).collect();
    let view = EditorView::from_text(&many).unwrap();
    let snapshot = view.worker_snapshot();
    let words = complete_candidates(snapshot.as_ref(), "w", ByteOffset(0), &NOT_CANCELLED).unwrap();
    assert_eq!(words.len(), qe_tools::COMPLETE_MAX_WORDS);
}

fn typed(remove_before: usize, insert: &str) -> Option<TypingEdit> {
    Some(TypingEdit {
        remove_before,
        insert: insert.to_string(),
    })
}

#[test]
fn defaults_are_the_macs() {
    let d = Substitutions::default();
    assert_eq!(
        (d.smart_copy, d.quotes, d.dashes),
        (true, false, false),
        "Smart Copy on, quotes and dashes off"
    );
    assert_eq!(substitute_typed("\"", "", d), None);
    assert_eq!(substitute_typed("-", "-", d), None);
}

#[test]
fn typing_substitutions_table() {
    let on = Substitutions {
        smart_copy: true,
        quotes: true,
        dashes: true,
    };
    let cases = [
        ("-", "a-", typed(1, "\u{2014}")),
        ("-", "a", None),
        ("-", "", None),
        ("\"", "", typed(0, "\u{201c}")),
        ("\"", "a ", typed(0, "\u{201c}")),
        ("\"", "a", typed(0, "\u{201d}")),
        ("\"", "(", typed(0, "\u{201c}")),
        ("\"", "a\r\n", typed(0, "\u{201c}")),
        ("\"", "\u{2014}", typed(0, "\u{201c}")),
        ("'", "", typed(0, "\u{2018}")),
        ("'", "don", typed(0, "\u{2019}")),
        ("'", "\u{1f600}", typed(0, "\u{2019}")),
        ("x", "a", None),
        ("--", "a", None),
    ];
    for (input, before, want) in cases {
        assert_eq!(
            substitute_typed(input, before, on),
            want,
            "{input:?} after {before:?}"
        );
    }
}

#[test]
fn over_50_mib_turns_off_quotes_and_dashes_and_leaves_the_setting() {
    let on = Substitutions {
        smart_copy: true,
        quotes: true,
        dashes: true,
    };
    let limit = qe_tools::SUBSTITUTION_LIMIT_BYTES;
    assert_eq!(on.for_document(limit), on);
    let large = on.for_document(limit + 1);
    assert!(!large.quotes && !large.dashes && large.smart_copy);
    assert!(on.quotes, "the setting stays");
}

#[test]
fn smart_paste_table() {
    let on = Substitutions::default();
    let off = Substitutions {
        smart_copy: false,
        ..on
    };
    let cases = [
        ("hello", Some('a'), Some('b'), on, true, " hello "),
        ("hello", Some(' '), Some(' '), on, true, "hello"),
        ("hello", None, None, on, true, "hello"),
        ("hello", Some('('), Some(')'), on, true, "hello"),
        ("hello", Some('a'), Some('.'), on, true, " hello"),
        (" hello ", Some('a'), Some('b'), on, true, " hello "),
        ("hello", Some('a'), Some('b'), off, true, "hello"),
        // The Mac's UI test: text not copied as a word pastes as it is.
        ("alpha", Some('a'), None, on, false, "alpha"),
        ("\u{1f600}", Some('a'), Some('b'), on, true, " \u{1f600} "),
        ("", Some('a'), Some('b'), on, true, ""),
    ];
    for (text, before, after, o, word, want) in cases {
        assert_eq!(smart_paste(text, before, after, o, word), want, "{text:?}");
    }
}

#[test]
fn smart_delete_table() {
    let on = Substitutions::default();
    let off = Substitutions {
        smart_copy: false,
        ..on
    };
    let word = ByteOffset(4)..ByteOffset(7);
    let b = |a: usize, z: usize| ByteOffset(a)..ByteOffset(z);
    let cases = [
        // "one two three": the space before goes with the word.
        (word.clone(), Some(' '), Some(' '), on, true, b(3, 7)),
        (word.clone(), Some(' '), None, on, true, b(3, 7)),
        (word.clone(), Some(' '), Some('.'), on, true, b(3, 7)),
        (word.clone(), Some(' '), Some('x'), on, true, b(4, 7)),
        (b(0, 3), None, Some(' '), on, true, b(0, 4)),
        (word.clone(), Some('x'), Some('y'), on, true, b(4, 7)),
        (word.clone(), Some(' '), Some(' '), off, true, b(4, 7)),
        // The Mac's UI test: a keyboard selection leaves its spaces, "1  2 beta".
        (word.clone(), Some(' '), Some(' '), on, false, b(4, 7)),
        (b(4, 4), Some(' '), Some(' '), on, true, b(4, 4)),
    ];
    for (range, before, after, o, was_word, want) in cases {
        assert_eq!(smart_delete_range(range, before, after, o, was_word), want);
    }
}
