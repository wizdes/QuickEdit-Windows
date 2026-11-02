use qe_tools::{FindOptions, LineSpec, find_text};

#[test]
fn mac_line_forms_and_errors() {
    let cases = [
        ("1", Some((1, 1, 0))),
        ("10-20", Some((10, 20, 0))),
        ("+2", Some((2, 2, 1))),
        ("-5", Some((5, 5, -1))),
        ("+2-4", Some((2, 4, 1))),
        ("-2-4", Some((2, 4, -1))),
        (" 3 - 5 ", Some((3, 5, 0))),
        ("0", Some((0, 0, 0))),
        ("4-2", None),
        ("", None),
        ("9999999999999999999999999999999", None),
    ];
    for (input, want) in cases {
        assert_eq!(
            LineSpec::parse(input).map(|s| (s.from, s.to, s.relative)),
            want,
            "{input}"
        );
    }
}

#[test]
fn find_respects_unicode_clusters_and_case() {
    let text = "a\u{301} A\u{301} ??a\u{301} Stra\u{df}e";
    assert_eq!(find_text(text, "a", FindOptions::default()), vec![17..18]);
    assert_eq!(
        find_text(
            text,
            "A\u{301}",
            FindOptions {
                case_sensitive: false,
                whole_word: false
            }
        ),
        vec![0..3, 4..7, 10..13]
    );
}

#[test]
fn seeded_find_agrees_with_slow_model() {
    let mut seed = 17_u64;
    let parts = [
        "a",
        "A",
        "b",
        "e\u{301}",
        "\u{1f600}",
        "\r\n",
        " ",
        "\u{df}",
    ];
    for _ in 0..80 {
        let mut text = String::new();
        for _ in 0..70 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            text.push_str(parts[(seed as usize) % parts.len()]);
        }
        for query in ["a", "b", "e\u{301}", "\u{1f600}", "\r\n", "\u{df}"] {
            let found = find_text(
                &text,
                query,
                FindOptions {
                    case_sensitive: true,
                    whole_word: false,
                },
            );
            let mut model = Vec::new();
            for (at, _) in text.char_indices() {
                if text.get(at..).is_some_and(|s| s.starts_with(query)) {
                    let end = at + query.len();
                    let previous = text.get(..at).and_then(|s| s.chars().next_back());
                    let next = text.get(end..).and_then(|s| s.chars().next());
                    let boundary = !next.is_some_and(|c| matches!(c as u32, 0x300..=0x36f))
                        && !(previous == Some('\r') && next == Some('\n'));
                    if boundary
                        && model
                            .last()
                            .is_none_or(|r: &std::ops::Range<usize>| r.end <= at)
                    {
                        model.push(at..end);
                    }
                }
            }
            assert_eq!(found, model, "{query:?} in {text:?}");
        }
    }
}
