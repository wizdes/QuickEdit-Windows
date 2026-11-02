//! Select Line, against the tables in the Mac's `LinePanelTests.swift` and a line-for-line
//! port of TextEdit's own loop.
use qe_api::{ByteOffset, ViewContext};
use qe_editor::EditorView;
use qe_tools::{LineError, LineSpec, line_error_message, lines_for_spec};

fn spec(from: i64, to: i64, relative: i8) -> Option<LineSpec> {
    Some(LineSpec { from, to, relative })
}

#[test]
fn parse_accepts_the_mac_forms() {
    let cases = [
        ("5", spec(5, 5, 0)),
        ("10-20", spec(10, 20, 0)),
        ("  10 - 20  ", spec(10, 20, 0)),
        ("7-7", spec(7, 7, 0)),
        ("+3", spec(3, 3, 1)),
        ("-3", spec(3, 3, -1)),
        ("+1-2", spec(1, 2, 1)),
        ("-2-4", spec(2, 4, -1)),
        ("0", spec(0, 0, 0)),
        // A sign after the first sign belongs to the number, as in `scanInt`.
        ("--3", spec(-3, -3, -1)),
        ("+-3", spec(-3, -3, 1)),
        ("--9223372036854775808", spec(i64::MIN, i64::MIN, -1)),
        ("++4", spec(4, 4, 1)),
        // Overflow gives the largest value, and the range step then refuses it.
        ("99999999999999999999999", spec(i64::MAX, i64::MAX, 0)),
    ];
    for (input, want) in cases {
        assert_eq!(LineSpec::parse(input), want, "{input:?}");
    }
}

#[test]
fn parse_rejects_what_the_mac_rejects() {
    for bad in [
        "", " ", "abc", "5-3", "5x", "1:3", "1-", "-", "+", "1 2", "1-2-3", "\u{661}",
    ] {
        assert_eq!(LineSpec::parse(bad), None, "{bad:?}");
    }
}

/// The range an entry gives, as `start..end` in bytes.
fn range(entry: &str, text: &str, selection: std::ops::Range<usize>) -> Option<(usize, usize)> {
    let mut view = EditorView::from_text(text).unwrap();
    let mut ctx = ViewContext::default();
    view.select(
        ByteOffset(selection.start)..ByteOffset(selection.end),
        &mut ctx,
    );
    match lines_for_spec(&view, LineSpec::parse(entry)?) {
        Ok(r) => Some((r.start.0, r.end.0)),
        Err(LineError::Invalid) => None,
        Err(other) => panic!("{entry:?}: {other:?}"),
    }
}
fn at_start(entry: &str, text: &str) -> Option<(usize, usize)> {
    range(entry, text, 0..0)
}

#[test]
fn line_endings_as_in_the_mac_table() {
    assert_eq!(at_start("1", "a\nb\nc"), Some((0, 2)));
    assert_eq!(at_start("2", "a\nb\nc"), Some((2, 4)));
    assert_eq!(at_start("3", "a\nb\nc"), Some((4, 5)));
    assert_eq!(at_start("2-3", "a\nb\nc"), Some((2, 5)));
    assert_eq!(at_start("2", "a\r\nb\r\nc"), Some((3, 6)));
    assert_eq!(at_start("3", "a\r\nb\r\nc"), Some((6, 7)));
    assert_eq!(at_start("1-2", "a\r\nb\r\nc"), Some((0, 6)));
    assert_eq!(at_start("2", "a\rb\rc"), Some((2, 4)));
    // All six endings count, and a form feed does not.
    let mixed = "a\nb\r\nc\rd\u{2028}e\u{2029}f\u{85}g\u{c}h";
    assert_eq!(at_start("3", mixed), Some((5, 7)));
    // Line 7 is `g`, FF and `h`.
    let start = mixed.find('g').unwrap();
    assert_eq!(at_start("7", mixed), Some((start, mixed.len())));
    assert_eq!(at_start("8", mixed), None);
}

#[test]
fn last_lines_and_out_of_range_as_in_the_mac_table() {
    assert_eq!(at_start("2", "a\nb"), Some((2, 3)));
    assert_eq!(at_start("2", "a\nb\n"), Some((2, 4)));
    assert_eq!(
        at_start("3", "a\nb\n"),
        None,
        "nothing follows the last line end"
    );
    assert_eq!(at_start("1", "only"), Some((0, 4)));
    assert_eq!(at_start("0", "a\nb"), None);
    assert_eq!(at_start("3", "a\nb"), None);
    assert_eq!(at_start("2-9", "a\nb"), None);
    assert_eq!(at_start("1", ""), None, "an empty document has no line 1");
    assert_eq!(at_start("9223372036854775807", "a\nb"), None);
    assert_eq!(at_start("-1", "a\nb"), None, "no line before the first");
}

#[test]
fn relative_forms_as_in_the_mac_table() {
    let text = "l1\nl2\nl3\nl4\nl5\n";
    let line3 = 6..6;
    assert_eq!(range("+0", text, line3.clone()), Some((6, 9)));
    assert_eq!(range("+1", text, line3.clone()), Some((9, 12)));
    assert_eq!(range("+1-2", text, line3.clone()), Some((9, 15)));
    assert_eq!(range("-1", text, line3.clone()), Some((3, 6)));
    assert_eq!(range("-1-2", text, line3.clone()), Some((0, 6)));
    assert_eq!(range("-3", text, line3.clone()), None);
    assert_eq!(range("+3", text, line3.clone()), None);
    // `+` counts from the end of a selection that spans lines 2 to 3.
    assert_eq!(range("+1", text, 3..9), Some((9, 12)));
}

#[test]
fn huge_entries_do_not_overflow_as_in_the_mac_table() {
    let text = "l1\nl2\nl3\nl4\nl5\n";
    let line3 = 6..6;
    // These act as one line back, or the current line for `+`.
    assert_eq!(
        range("--9223372036854775808", text, line3.clone()),
        Some((3, 6))
    );
    assert_eq!(
        range("--9223372036854775807", text, line3.clone()),
        Some((3, 6))
    );
    assert_eq!(
        range("+-9223372036854775808", text, line3.clone()),
        Some((6, 9))
    );
    for entry in [
        "-9223372036854775807",
        "-1-9223372036854775807",
        "--9223372036854775808-0",
        "--9223372036854775808-9223372036854775807",
        "-9223372036854775807-9223372036854775807",
        "+9223372036854775807",
        "+0-9223372036854775807",
        "99999999999999999999999999",
        "-99999999999999999999999999",
    ] {
        assert_eq!(range(entry, text, line3.clone()), None, "{entry}");
    }
}

#[test]
fn unicode_lines_use_bytes_and_stay_on_character_edges() {
    let text = "e\u{301}\u{1f600}\r\n\u{4e2d}\u{6587}\r\nz";
    let second = text.find('\u{4e2d}').unwrap();
    assert_eq!(at_start("2", text), Some((second, text.find('z').unwrap())));
    // A selection that ends right after a multibyte letter stays on its line.
    let end = second + 3;
    assert_eq!(
        range("+0", text, second..end),
        Some((second, text.find('z').unwrap()))
    );
}

#[test]
fn error_text_is_the_macs_and_cuts_as_the_mac_cuts() {
    assert_eq!(
        line_error_message("99"),
        "Invalid line specification \u{201c}99\u{201d}."
    );
    let short = "x".repeat(99);
    assert!(line_error_message(&short).contains(&short));
    let long = "x".repeat(100);
    let cut = format!("{}\u{2026}", "x".repeat(90));
    assert_eq!(
        line_error_message(&long),
        format!("Invalid line specification \u{201c}{cut}\u{201d}.")
    );
    // Counted in UTF-16 units: 50 emoji are 100 units, and a pair is never split.
    let emoji = "\u{1f600}".repeat(50);
    let cut = format!("{}\u{2026}", "\u{1f600}".repeat(45));
    assert!(line_error_message(&emoji).contains(&cut));
}

/// TextEdit's `getRange:…` line for line, over the characters of a string.
fn textedit_oracle(
    spec: LineSpec,
    text: &str,
    selection: (usize, usize),
) -> Option<(usize, usize)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let total = chars.len();
    let byte = |i: usize| chars.get(i).map_or(text.len(), |c| c.0);
    let is_end = |i: usize| matches!(chars[i].1, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}');
    // The index just after the line that holds `i`.
    let line_end = |i: usize| {
        let mut at = i;
        // A location on the LF of a CRLF counts as on that same line end.
        while at < total {
            if is_end(at) {
                if chars[at].1 == '\r' && at + 1 < total && chars[at + 1].1 == '\n' {
                    return at + 2;
                }
                return at + 1;
            }
            at += 1;
        }
        total
    };
    let line_start = |i: usize| {
        let mut at = i.min(total);
        // On the LF of a CRLF, the line still starts before the CR.
        if at > 0 && at < total && chars[at].1 == '\n' && chars[at - 1].1 == '\r' {
            at -= 1;
        }
        while at > 0 {
            let before = at - 1;
            if is_end(before) {
                return at;
            }
            at -= 1;
        }
        0
    };
    let (from, to) = (spec.from, spec.to);
    let mut location = 0;
    if spec.relative != 0 {
        let at = if spec.relative > 0 {
            selection.1 - usize::from(selection.1 > selection.0)
        } else {
            selection.0
        };
        location = line_start(at);
    } else if from == 0 {
        return None;
    }
    if spec.relative < 0 {
        let mut count = 1;
        while count < from {
            if location == 0 {
                return None;
            }
            location = line_start(location - 1);
            count += 1;
        }
        let end = location;
        count = from;
        while count <= to {
            if location == 0 {
                return None;
            }
            location = line_start(location - 1);
            count += 1;
        }
        return Some((byte(location), byte(end)));
    }
    let mut count = i64::from(spec.relative == 0);
    while count < from {
        if location == total {
            return None;
        }
        location = line_end(location);
        count += 1;
    }
    let mut end = location;
    count = from;
    while count <= to {
        if end == total {
            return None;
        }
        end = line_end(end);
        count += 1;
    }
    Some((byte(location), byte(end)))
}

#[test]
fn every_entry_matches_textedits_loop_on_every_ending() {
    let texts = [
        "",
        "x",
        "\n",
        "\r\n",
        "\r",
        "a\nb",
        "a\nb\n",
        "a\r\n\r\nb",
        "\n\n\n",
        "a\r\rb\r\n",
        "a\nb\r\nc\rd\u{2028}e\u{2029}f\u{85}g\u{c}h",
        "\r\n\r",
        "\u{e9}\n\u{1f600}\r\nz",
    ];
    let mut compared = 0;
    for text in texts {
        let chars: Vec<usize> = text.char_indices().map(|c| c.0).collect();
        let total = chars.len();
        let byte = |i: usize| chars.get(i).copied().unwrap_or(text.len());
        let mut selections = vec![(0, 0), (total, total)];
        if total > 1 {
            selections.push((1, total));
        }
        for relative in [0_i8, 1, -1] {
            for from in 0..=6_i64 {
                for to in from..=6_i64 {
                    let spec = LineSpec { from, to, relative };
                    for &(a, b) in &selections {
                        // The caret cannot stand inside a CRLF.
                        let (sa, sb) = (byte(a), byte(b));
                        let bytes = text.as_bytes();
                        let crlf_inside =
                            sa > 0 && sa < bytes.len() && bytes[sa] == 10 && bytes[sa - 1] == 13;
                        if crlf_inside {
                            continue;
                        }
                        let mut view = EditorView::from_text(text).unwrap();
                        let mut ctx = ViewContext::default();
                        view.select(ByteOffset(sa)..ByteOffset(sb), &mut ctx);
                        let selection = view.selection();
                        let got = match lines_for_spec(&view, spec) {
                            Ok(r) => Some((r.start.0, r.end.0)),
                            Err(LineError::Invalid) => None,
                            Err(other) => panic!("{other:?}"),
                        };
                        let index =
                            |bytes: usize| chars.iter().position(|c| *c == bytes).unwrap_or(total);
                        let want = textedit_oracle(
                            spec,
                            text,
                            (index(selection.start.0), index(selection.end.0)),
                        );
                        assert_eq!(got, want, "{spec:?} in {text:?} from {selection:?}");
                        compared += 1;
                    }
                }
            }
        }
    }
    assert!(compared > 2500, "only {compared} compared");
}
