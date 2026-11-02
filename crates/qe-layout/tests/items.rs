//! One test per finding from the M4 reviews, where another test file does not hold it.
use qe_api::{
    Affinity, ByteOffset, Dip, Layout, LineIndex, Point, TextMeasurer, TextSnapshot,
    testing::StringStore,
};
use qe_layout::{
    DirectWriteLayout, FontChoice, HeightError, HeightMap, LONG_LINE_BYTES, LayoutError,
    loaded_font_count,
};
use std::sync::Arc;
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL as NORMAL;

fn line(engine: &DirectWriteLayout, text: &str, width: f32) -> qe_api::LineLayout {
    engine
        .layout_line(&StringStore::new(text), LineIndex(0), Dip(width))
        .unwrap()
}
fn stops(layout: &qe_api::LineLayout) -> Vec<usize> {
    layout
        .rows
        .iter()
        .flat_map(|r| r.caret_stops.iter().map(|s| s.byte.0))
        .collect()
}

// Finding 1: caret data lives in the layout.
#[test]
fn caret_data_survives_any_number_of_other_layouts() {
    let engine = DirectWriteLayout::new().unwrap();
    let first = line(&engine, "hello", 1000.0);
    for i in 0..1000 {
        line(&engine, &format!("other {i}"), 1000.0);
    }
    let rect = engine.caret_rect(&first, ByteOffset(2), Affinity::After);
    assert_eq!(rect.unwrap().x, Dip(2.0 * engine.cell_width().0));
    let far = Point {
        x: Dip(1000.0),
        y: Dip(1.0),
    };
    assert_eq!(engine.hit_test_point(&first, far).byte, ByteOffset(5));
}

#[test]
fn layouts_with_the_same_glyphs_do_not_share_caret_data() {
    // Right-to-left overrides: same glyphs and advances, opposite caret order.
    let engine = DirectWriteLayout::new().unwrap();
    let a = line(&engine, "\u{202E}abc\u{202C}", 1000.0);
    let b = line(&engine, "\u{202D}abc\u{202C}", 1000.0);
    let x = |l: &qe_api::LineLayout, byte| {
        engine
            .caret_rect(l, ByteOffset(byte), Affinity::After)
            .unwrap()
            .x
            .0
    };
    assert!(x(&a, 3) > x(&a, 4), "the override runs right to left");
    assert!(x(&b, 3) < x(&b, 4));
}

// Change B: one byte, two places.
#[test]
fn a_wrap_point_has_two_caret_places() {
    for shaped in [false, true] {
        let engine = DirectWriteLayout::new().unwrap().with_shaping(shaped);
        let cell = engine.cell_width().0;
        let l = line(&engine, "abcd efgh", cell * 5.0);
        assert_eq!(l.rows.len(), 2);
        let wrap = l.rows[1].range.start;
        assert_eq!(wrap, ByteOffset(5));
        let end_of_first = engine.caret_rect(&l, wrap, Affinity::Before).unwrap();
        let start_of_second = engine.caret_rect(&l, wrap, Affinity::After).unwrap();
        assert_eq!(end_of_first.y, l.rows[0].y);
        assert!((end_of_first.x.0 - cell * 5.0).abs() < 0.02);
        assert_eq!(start_of_second.y, l.rows[1].y);
        assert_eq!(start_of_second.x, Dip(0.0));

        // Clicking the start of row 2 gives the caret on row 2.
        let click = Point {
            x: Dip(0.5),
            y: Dip(l.rows[1].y.0 + 1.0),
        };
        let hit = engine.hit_test_point(&l, click);
        assert_eq!((hit.byte, hit.affinity), (wrap, Affinity::After));
        let caret = engine.caret_rect(&l, hit.byte, hit.affinity).unwrap();
        assert_eq!(caret.y, l.rows[1].y);

        // Clicking past the end of row 1 keeps the caret on row 1.
        let click = Point {
            x: Dip(900.0),
            y: Dip(1.0),
        };
        let hit = engine.hit_test_point(&l, click);
        assert_eq!((hit.byte, hit.affinity), (wrap, Affinity::Before));
        let caret = engine.caret_rect(&l, hit.byte, hit.affinity).unwrap();
        assert_eq!(caret.y, l.rows[0].y);
    }
}

#[test]
fn a_direction_change_has_two_caret_places() {
    let engine = DirectWriteLayout::new().unwrap();
    let l = line(&engine, "\u{5E9}\u{5DC}\u{5D5}\u{5DD} abc", 1000.0);
    let before = engine
        .caret_rect(&l, ByteOffset(8), Affinity::Before)
        .unwrap();
    let after = engine
        .caret_rect(&l, ByteOffset(8), Affinity::After)
        .unwrap();
    assert!(before.x.0 < 0.01, "the Hebrew word ends at its left edge");
    assert!(after.x.0 > 20.0, "the space starts after the Hebrew word");
}

#[test]
fn a_hard_break_has_no_caret_place_on_its_own_row() {
    let engine = DirectWriteLayout::new().unwrap();
    let l = line(&engine, "a\u{b}b", 1000.0);
    assert_eq!(l.rows.len(), 2);
    assert_eq!(stops(&l), [0, 1, 2, 3]);
    let past_end = engine.hit_test_point(
        &l,
        Point {
            x: Dip(900.0),
            y: Dip(1.0),
        },
    );
    assert_eq!(past_end.byte, ByteOffset(1));
    for affinity in [Affinity::Before, Affinity::After] {
        let caret = engine.caret_rect(&l, ByteOffset(2), affinity).unwrap();
        assert_eq!(caret.y, l.rows[1].y, "{affinity:?}");
    }
}

// Finding 9.
#[test]
fn no_caret_stop_inside_a_cluster() {
    for family in ["Cascadia Mono", "Segoe UI"] {
        let engine = DirectWriteLayout::with_font(family, 10.0, NORMAL).unwrap();
        for text in [
            "\u{1F1FA}\u{1F1F8}",
            "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
            "1\u{FE0F}\u{20E3}",
            "a\u{200D}b",
            "\u{1100}\u{1161}\u{11A8}",
            "\u{644}\u{627}",
            "e\u{301}",
            "\u{1F469}\u{200D}\u{1F4BB}",
        ] {
            assert_eq!(
                stops(&line(&engine, text, 1000.0)),
                [0, text.len()],
                "{family} {text:?}"
            );
        }
    }
}

#[test]
fn caret_stops_are_clusters_between_other_text() {
    let engine = DirectWriteLayout::new().unwrap();
    let got = stops(&line(
        &engine,
        "x\u{1F1FA}\u{1F1F8}y1\u{FE0F}\u{20E3}z",
        1000.0,
    ));
    assert_eq!(got, [0, 1, 9, 10, 17, 18]);
}

#[test]
fn slices_do_not_split_clusters() {
    let engine = DirectWriteLayout::new().unwrap();
    for (text, inside) in [
        ("abc \u{1F1FA}\u{1F1F8} xyz", 8),
        ("abc 1\u{FE0F}\u{20E3} xyz", 5),
        ("abc a\u{200D}b xyz", 5),
        ("abc \u{1100}\u{1161}\u{11A8} xyz", 7),
        ("abc \u{644}\u{627} xyz", 6),
    ] {
        let store = StringStore::new(text);
        for max in [4, 8, 16, 32] {
            let Some(range) = engine
                .slice_around(&store, ByteOffset(inside), max)
                .unwrap()
            else {
                continue;
            };
            assert!(
                range.start.0 <= 4 && range.end.0 > inside,
                "{text:?} max {max}: {range:?}"
            );
        }
    }
}

#[test]
fn a_slice_of_a_cut_cluster_is_refused() {
    let engine = DirectWriteLayout::new().unwrap();
    let store = StringStore::new("e\u{301}x");
    let only_mark = ByteOffset(1)..ByteOffset(3);
    assert!(
        engine
            .layout_slice(&store, only_mark.clone(), Dip(100.0))
            .is_none()
    );
    assert_eq!(
        engine.last_error(),
        Some(LayoutError::CutCluster(ByteOffset(1)))
    );
    let keeps_base_only =
        engine.layout_slice_from_column(&store, ByteOffset(0)..ByteOffset(1), Dip(100.0), 0);
    assert_eq!(keeps_base_only, Err(LayoutError::CutCluster(ByteOffset(1))));
    let lam_alef = StringStore::new("\u{644}\u{627}");
    assert!(
        engine
            .layout_slice(&lam_alef, ByteOffset(0)..ByteOffset(2), Dip(100.0))
            .is_none()
    );
    assert!(
        engine
            .layout_slice(&store, ByteOffset(0)..ByteOffset(3), Dip(100.0))
            .is_some()
    );
}

#[test]
fn slice_around_uses_the_engines_font() {
    // Calibri joins "ffi" into one cluster. Cascadia Mono has ligatures off. A
    // slice helper with a fixed font would give the same edges for both.
    let text = "office";
    let store = StringStore::new(text);
    let mono = DirectWriteLayout::with_font("Cascadia Mono", 10.0, NORMAL).unwrap();
    let joined = DirectWriteLayout::with_font("Calibri", 10.0, NORMAL).unwrap();
    assert_eq!(stops(&line(&mono, text, 1000.0)).len(), 7);
    let joined_stops = stops(&line(&joined, text, 1000.0));
    assert!(joined_stops.len() < 7, "{joined_stops:?}");
    for byte in 0..=text.len() {
        let range = joined
            .slice_around(&store, ByteOffset(byte), 16)
            .unwrap()
            .unwrap();
        assert!(joined_stops.contains(&range.start.0), "{range:?}");
        assert!(joined_stops.contains(&range.end.0), "{range:?}");
    }
    let inside = (1..text.len()).find(|b| !joined_stops.contains(b)).unwrap();
    let cut = ByteOffset(inside);
    assert!(
        mono.layout_slice(&store, cut..ByteOffset(6), Dip(100.0))
            .is_some()
    );
    assert!(
        joined
            .layout_slice(&store, cut..ByteOffset(6), Dip(100.0))
            .is_none()
    );
}

// Finding 10.
#[test]
fn slice_around_works_at_the_end_of_a_line_with_a_line_ending() {
    let engine = DirectWriteLayout::new().unwrap();
    for (text, at, want) in [
        ("abc\nxyz", 3, 0..3),
        ("abc\r\nxyz", 3, 0..3),
        ("abc\u{2028}xyz", 3, 0..3),
        ("abc\u{85}xyz", 3, 0..3),
        ("abc", 3, 0..3),
    ] {
        let store = StringStore::new(text);
        let range = engine
            .slice_around(&store, ByteOffset(at), 16)
            .unwrap()
            .unwrap_or_else(|| panic!("{text:?}"));
        assert_eq!(
            range,
            ByteOffset(want.start)..ByteOffset(want.end),
            "{text:?}"
        );
    }
    let long = "x".repeat(1000) + "\n";
    let store = StringStore::new(long);
    let range = engine
        .slice_around(&store, ByteOffset(990), 64)
        .unwrap()
        .unwrap();
    assert_eq!(range.end, ByteOffset(1000), "the last column stays");
}

// Finding 11.
#[test]
fn fast_x_does_not_drift() {
    let engine = DirectWriteLayout::with_font("Consolas", 10.0, NORMAL)
        .unwrap()
        .with_wrap(false);
    let cell = engine.cell_width().0 as f64;
    let store = StringStore::new("x".repeat(LONG_LINE_BYTES));
    let layout = engine
        .layout_line(&store, LineIndex(0), Dip(1000.0))
        .unwrap();
    for stop in &layout.rows[0].caret_stops {
        let want = stop.byte.0 as f64 * cell;
        assert!((stop.x.0 as f64 - want).abs() < 0.02, "{stop:?} vs {want}");
    }
}

// Finding 12.
#[test]
fn height_map_round_trips_real_heights_in_a_huge_file() {
    let mut map = HeightMap::new(20_000_000, Dip(15.494791)).unwrap();
    assert_eq!(map.run_count(), 1);
    assert!(map.memory_bytes() < 1024, "{}", map.memory_bytes());
    for line in (0..20_000_000).step_by(99_991) {
        let got = map.scroll_to_line(map.line_to_scroll(LineIndex(line)));
        assert_eq!(got, LineIndex(line));
    }
    for line in (5..20_000_000).step_by(1_000_003) {
        map.set(LineIndex(line), Dip(31.0)).unwrap();
    }
    assert!(map.memory_bytes() < 16 * 1024, "{}", map.memory_bytes());
    for line in (0..20_000_000).step_by(99_991) {
        let got = map.scroll_to_line(map.line_to_scroll(LineIndex(line)));
        assert_eq!(got, LineIndex(line));
    }
    assert_eq!(map.scroll_to_line(-5.0), LineIndex(0));
    assert_eq!(map.scroll_to_line(f64::NAN), LineIndex(0));
    assert_eq!(map.scroll_to_line(1e15), LineIndex(19_999_999));
}

#[test]
fn height_map_refuses_bad_heights() {
    let mut map = HeightMap::new(10, Dip(12.0)).unwrap();
    let total = map.total_height();
    for bad in [f32::NAN, f32::INFINITY, -1.0] {
        assert!(matches!(
            map.set(LineIndex(2), Dip(bad)),
            Err(HeightError::InvalidHeight(_))
        ));
        assert!(HeightMap::new(5, Dip(bad)).is_err());
    }
    assert!(matches!(
        map.set(LineIndex(10), Dip(1.0)),
        Err(HeightError::OutOfRange { .. })
    ));
    assert!(map.insert_lines(LineIndex(11), 1).is_err());
    assert!(map.remove_lines(5..11).is_err());
    assert_eq!(map.total_height(), total);
    assert_eq!(map.len(), 10);
}

#[test]
fn enter_keeps_measured_heights_and_is_fast() {
    let mut map = HeightMap::new(20_000_000, Dip(15.0)).unwrap();
    map.set(LineIndex(10_000_000), Dip(45.0)).unwrap();
    let start = std::time::Instant::now();
    for i in 0..1000 {
        map.insert_lines(LineIndex(5_000_000 + i), 1).unwrap();
        map.remove_lines(5_000_000 + i..5_000_001 + i).unwrap();
    }
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    assert!(ms < 200.0, "2000 edits took {ms} ms");
    map.insert_lines(LineIndex(5_000_000), 3).unwrap();
    assert_eq!(map.len(), 20_000_003);
    assert_eq!(map.line_height(LineIndex(10_000_003)), Some(Dip(45.0)));
    assert_eq!(map.line_height(LineIndex(10_000_000)), Some(Dip(15.0)));
    map.remove_lines(0..3).unwrap();
    assert_eq!(map.line_height(LineIndex(10_000_000)), Some(Dip(45.0)));
    assert!(map.run_count() <= 3, "{}", map.run_count());
}

#[test]
fn height_map_matches_a_plain_list_under_random_edits() {
    let mut map = HeightMap::new(300, Dip(12.5)).unwrap();
    let mut plain = vec![12.5f32; 300];
    let mut seed = 3u64;
    let mut next = move |below: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % below
    };
    for _ in 0..4000 {
        match next(4) {
            0 => {
                let at = next(plain.len() + 1);
                let n = next(4);
                map.insert_lines(LineIndex(at), n).unwrap();
                plain.splice(at..at, std::iter::repeat_n(12.5, n));
            }
            1 if !plain.is_empty() => {
                let at = next(plain.len());
                let end = (at + next(4)).min(plain.len());
                map.remove_lines(at..end).unwrap();
                plain.drain(at..end);
            }
            _ if !plain.is_empty() => {
                let at = next(plain.len());
                let h = [12.5, 25.0, 15.494791, 0.5][next(4)];
                map.set(LineIndex(at), Dip(h)).unwrap();
                plain[at] = h;
            }
            _ => {}
        }
        assert_eq!(map.len(), plain.len());
        let at = next(plain.len() + 1);
        let want: f64 = plain[..at].iter().map(|h| *h as f64).sum();
        assert!((map.line_to_scroll(LineIndex(at)) - want).abs() < 1e-6);
        if at < plain.len() {
            assert_eq!(map.line_height(LineIndex(at)), Some(Dip(plain[at])));
            assert_eq!(
                map.scroll_to_line(map.line_to_scroll(LineIndex(at))),
                LineIndex(at)
            );
        }
    }
}

// Finding 13.
#[test]
fn slices_keep_tabs_on_the_same_stops() {
    for shaped in [false, true] {
        let engine = DirectWriteLayout::new().unwrap().with_shaping(shaped);
        let store = StringStore::new("abc\tdef");
        let whole = engine
            .layout_line(&store, LineIndex(0), Dip(1000.0))
            .unwrap();
        let part = engine
            .layout_slice_from_column(&store, ByteOffset(1)..ByteOffset(7), Dip(1000.0), 1)
            .unwrap();
        for byte in 1..=7 {
            let a = engine
                .caret_rect(&whole, ByteOffset(byte), Affinity::After)
                .unwrap()
                .x
                .0;
            let origin = engine
                .caret_rect(&whole, ByteOffset(1), Affinity::After)
                .unwrap()
                .x
                .0;
            let b = engine
                .caret_rect(&part, ByteOffset(byte), Affinity::After)
                .unwrap()
                .x
                .0;
            assert!(
                (a - origin - b).abs() < 0.05,
                "shaped={shaped} byte {byte}: {a} {b}"
            );
        }
        let glyphs: usize = part.rows[0]
            .glyph_runs
            .iter()
            .map(|g| g.glyph_ids.len())
            .sum();
        assert_eq!(glyphs, 5, "shaped={shaped}: stand-in spaces must not draw");
        assert!(part.partial);
        assert!(!whole.partial);
        let last = part.rows[0].glyph_runs.last().unwrap();
        let tab_end = engine
            .caret_rect(&part, ByteOffset(4), Affinity::After)
            .unwrap()
            .x
            .0;
        assert!((last.origin.x.0 - tab_end).abs() < 0.05, "shaped={shaped}");
    }
}

// Finding 14.
#[test]
fn measure_uses_the_fonts_that_really_draw_the_text() {
    let engine = DirectWriteLayout::new().unwrap();
    let plain = engine.measure("a", "Cascadia Mono 10");
    let emoji = engine.measure("\u{1F600}", "Cascadia Mono 10");
    assert!(emoji.height.0 > 17.0, "{:?}", emoji);
    assert!(emoji.height.0 > plain.height.0);
    assert!(emoji.baseline.0 >= plain.baseline.0);
    let two = engine.measure("a\nb", "Cascadia Mono 10");
    assert!(
        (two.height.0 - plain.height.0 * 2.0).abs() < 0.01,
        "{:?}",
        two
    );
    assert_eq!(two.baseline, plain.baseline);
    let wide = engine.measure("a\nbbbb", "Cascadia Mono 10");
    assert!((wide.width.0 - engine.cell_width().0 * 4.0).abs() < 0.01);
    assert!(engine.measure("", "Cascadia Mono 10").height.0 > 0.0);
}

#[test]
fn missing_family_names_are_not_kept() {
    let engine = DirectWriteLayout::new().unwrap();
    engine.measure("x", "Definitely Missing Family 11");
    let before = loaded_font_count();
    for i in 0..2000 {
        engine.measure("x", &format!("Missing Family Number {i} x 11"));
    }
    // Other test threads load real fonts at the same time, but only a few.
    assert!(loaded_font_count() < before + 100);
}

// Finding 15.
#[test]
fn a_missing_font_is_an_error_not_a_quiet_change() {
    let missing = DirectWriteLayout::with_font("No Such Font", 10.0, NORMAL);
    assert!(matches!(missing, Err(LayoutError::MissingFont(name)) if name == "No Such Font"));
}

#[test]
fn the_default_font_choice_is_stated() {
    let (engine, choice) = DirectWriteLayout::with_default_font().unwrap();
    assert_eq!(engine.font_choice(), &choice);
    assert_eq!(choice.used, engine.font_family());
    let (engine, choice) =
        DirectWriteLayout::with_first_installed(&["No Such Font", "Consolas"], 10.0, NORMAL)
            .unwrap();
    assert_eq!(
        choice,
        FontChoice {
            requested: "No Such Font".into(),
            used: "Consolas".into()
        }
    );
    assert!(choice.is_fallback());
    assert_eq!(engine.font_family(), "Consolas");
    assert!(matches!(
        DirectWriteLayout::with_first_installed(&["No Such Font", "Nor This"], 10.0, NORMAL),
        Err(LayoutError::NoFontFound(_))
    ));
}

#[test]
fn a_measure_with_a_missing_font_names_what_it_used() {
    let engine = DirectWriteLayout::new().unwrap();
    let measured = engine.measure_with_font("text", "No Such Font 10").unwrap();
    let fallback = measured.fallback.unwrap();
    assert_eq!(fallback.requested, "No Such Font");
    assert_eq!(fallback.used, engine.font_family());
    engine.measure("text", "No Such Font 10");
    assert_eq!(
        engine.last_measure_fallback().unwrap().requested,
        "No Such Font"
    );
    let good = engine.measure_with_font("text", "Segoe UI 9").unwrap();
    assert_eq!(good.fallback, None);
}

#[test]
fn a_failed_layout_says_why() {
    let engine = DirectWriteLayout::new().unwrap();
    let store = StringStore::new("abc");
    assert!(
        engine
            .layout_line(&store, LineIndex(5), Dip(100.0))
            .is_none()
    );
    assert_eq!(
        engine.last_error(),
        Some(LayoutError::NoSuchLine(LineIndex(5)))
    );
    let too_long = StringStore::new("x".repeat(LONG_LINE_BYTES + 1));
    assert!(matches!(
        engine.layout_slice_from_column(
            &too_long,
            ByteOffset(0)..ByteOffset(LONG_LINE_BYTES + 1),
            Dip(100.0),
            0
        ),
        Err(LayoutError::SliceTooLong { .. })
    ));
    assert!(matches!(
        engine.layout_slice_from_column(
            &StringStore::new("a\nb"),
            ByteOffset(0)..ByteOffset(3),
            Dip(100.0),
            0
        ),
        Err(LayoutError::BadRange(_))
    ));
}

// Finding 16.
#[test]
fn threads_get_the_same_layouts() {
    let engine = Arc::new(DirectWriteLayout::new().unwrap());
    let store = Arc::new(StringStore::new(
        "plain ascii line\n\u{645}\u{631}\u{62D}\u{628}\u{627} mixed abc\n\u{6F22}\u{5B57} wide\n\u{1F469}\u{200D}\u{1F4BB} emoji",
    ));
    let expected: Vec<_> = (0..4)
        .map(|i| {
            engine
                .layout_line(store.as_ref(), LineIndex(i), Dip(300.0))
                .unwrap()
        })
        .collect();
    let expected = Arc::new(expected);
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let (engine, store, expected) = (engine.clone(), store.clone(), expected.clone());
            std::thread::spawn(move || {
                for round in 0..200 {
                    let i = round % 4;
                    let got = engine
                        .layout_line(store.as_ref(), LineIndex(i), Dip(300.0))
                        .unwrap();
                    assert_eq!(got, expected[i]);
                    let hit = engine.hit_test_point(
                        &expected[i],
                        Point {
                            x: Dip(20.0),
                            y: Dip(1.0),
                        },
                    );
                    assert!(hit.byte.0 <= store.len_bytes().0);
                    let m = engine.measure("abc", "Segoe UI 9");
                    assert!(m.width.0 > 0.0);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}

// Change C.
#[test]
fn runs_carry_their_em_size_and_a_resolvable_face() {
    for shaped in [false, true] {
        let engine = DirectWriteLayout::with_font("Consolas", 12.0, NORMAL)
            .unwrap()
            .with_shaping(shaped);
        let run = line(&engine, "abc", 1000.0).rows[0].glyph_runs[0].clone();
        assert!((run.font_em_size.0 - 16.0).abs() < 0.001);
        assert!(qe_layout::font_face(run.font_id).is_some());
    }
    assert!(qe_layout::font_face(0).is_none());
    assert!(qe_layout::font_face(u32::MAX).is_none());
}

// Round 3, item 1.
const SET_LIMIT_US: f64 = if cfg!(debug_assertions) { 60.0 } else { 10.0 };
const DEPTH_LIMIT: usize = 90;

#[test]
fn height_map_stays_balanced_at_a_million_runs() {
    let lines = 1 << 20;
    let mut map = HeightMap::new(lines, Dip(10.0)).unwrap();
    let start = std::time::Instant::now();
    for line in 0..lines {
        let height = if line % 2 == 0 { 20.0 } else { 30.0 };
        map.set(LineIndex(line), Dip(height)).unwrap();
    }
    let per_set = start.elapsed().as_secs_f64() * 1e6 / lines as f64;
    println!("in order: {per_set:.2} us per set, depth {}", map.depth());
    assert!(map.run_count() >= 1_000_000, "{}", map.run_count());
    assert!(map.depth() <= DEPTH_LIMIT, "depth {}", map.depth());
    assert!(per_set < SET_LIMIT_US, "{per_set} us per set");
    for line in (0..lines).step_by(9973) {
        assert_eq!(
            map.scroll_to_line(map.line_to_scroll(LineIndex(line))),
            LineIndex(line)
        );
    }
    let start = std::time::Instant::now();
    for i in 0..1000 {
        map.insert_lines(LineIndex(500_000 + i), 1).unwrap();
        map.remove_lines(700_000 + i..700_001 + i).unwrap();
    }
    let per_edit = start.elapsed().as_secs_f64() * 1e6 / 2000.0;
    println!(
        "insert and remove: {per_edit:.2} us each, depth {}",
        map.depth()
    );
    assert!(per_edit < SET_LIMIT_US, "{per_edit} us per edit");
    assert!(map.depth() <= DEPTH_LIMIT, "depth {}", map.depth());
}

#[test]
fn height_map_stays_balanced_in_random_order() {
    // A multiplier of 5 mod 2^20 with an odd step visits every line once.
    let lines = 1usize << 20;
    let mut map = HeightMap::new(lines, Dip(10.0)).unwrap();
    let mut line = 0usize;
    let start = std::time::Instant::now();
    for _ in 0..lines {
        line = (line * 5 + 12345) % lines;
        let height = [20.0, 30.0, 40.0][line % 3];
        map.set(LineIndex(line), Dip(height)).unwrap();
    }
    let per_set = start.elapsed().as_secs_f64() * 1e6 / lines as f64;
    println!(
        "random: {per_set:.2} us per set, depth {}, {} runs",
        map.depth(),
        map.run_count()
    );
    assert!(map.run_count() >= 1_000_000, "{}", map.run_count());
    assert!(map.depth() <= DEPTH_LIMIT, "depth {}", map.depth());
    assert!(per_set < SET_LIMIT_US, "{per_set} us per set");
}

// Round 3, item 13.
#[test]
fn last_error_is_per_thread_and_cleared_by_the_next_call() {
    let engine = Arc::new(DirectWriteLayout::new().unwrap());
    let store = StringStore::new("abc");
    assert!(
        engine
            .layout_line(&store, LineIndex(9), Dip(100.0))
            .is_none()
    );
    assert_eq!(
        engine.last_error(),
        Some(LayoutError::NoSuchLine(LineIndex(9)))
    );
    let other = engine.clone();
    let seen = std::thread::spawn(move || {
        let before = other.last_error();
        let store = StringStore::new("abc");
        assert!(
            other
                .layout_line(&store, LineIndex(0), Dip(100.0))
                .is_some()
        );
        (before, other.last_error())
    })
    .join()
    .unwrap();
    assert_eq!(seen, (None, None));
    assert_eq!(
        engine.last_error(),
        Some(LayoutError::NoSuchLine(LineIndex(9)))
    );
    assert!(
        engine
            .layout_line(&store, LineIndex(0), Dip(100.0))
            .is_some()
    );
    assert_eq!(engine.last_error(), None);
    engine.measure("x", "No Such Font 10");
    assert!(engine.last_measure_fallback().is_some());
    engine.measure("x", "Segoe UI 10");
    assert_eq!(engine.last_measure_fallback(), None);
}

// Round 3, items 7 and 14.
#[test]
fn stand_in_spaces_never_make_a_row() {
    for family in ["Cascadia Mono", "Consolas", "Courier New"] {
        let engine = DirectWriteLayout::with_font(family, 10.0, NORMAL).unwrap();
        let store = StringStore::new("ab\tcd");
        for width in [1.0, 5.0, 7.0, 12.0, 20.0, 40.0, 60.0] {
            let l = engine
                .layout_slice_from_column(&store, ByteOffset(0)..ByteOffset(5), Dip(width), 5)
                .unwrap();
            assert_eq!(
                l.rows[0].range.start,
                ByteOffset(0),
                "{family} width {width}"
            );
            assert!(
                l.rows[0].range.end > ByteOffset(0),
                "{family} width {width}: empty row"
            );
            assert_eq!(l.rows[0].y, Dip(0.0));
            let click = Point {
                x: Dip(0.5),
                y: Dip(1.0),
            };
            let hit = engine.hit_test_point(&l, click);
            let caret = engine.caret_rect(&l, hit.byte, hit.affinity).unwrap();
            assert_eq!(
                caret.y,
                Dip(0.0),
                "{family} width {width}: caret on another row"
            );
            let bottom = l.rows.last().unwrap();
            assert!((bottom.y.0 + bottom.height.0 - l.height.0).abs() < 0.01);
            for row in &l.rows {
                for run in &row.glyph_runs {
                    assert!(run.origin.y.0 >= row.y.0 && run.origin.y.0 <= row.y.0 + row.height.0);
                    assert!(run.origin.x.0 >= -0.01, "{family} width {width}");
                }
            }
        }
    }
}

// Astra finding A.
fn assert_no_stop_inside(text: &str, unit: &str, prefix: usize, layout: &qe_api::LineLayout) {
    for row in &layout.rows {
        for edge in [row.range.start.0, row.range.end.0] {
            assert!(
                edge <= prefix || (edge - prefix).is_multiple_of(unit.len()),
                "{text:?}: row edge {edge} is inside a cluster"
            );
        }
        for stop in &row.caret_stops {
            assert!(
                stop.byte.0 <= prefix || (stop.byte.0 - prefix).is_multiple_of(unit.len()),
                "{text:?}: stop {} is inside a cluster",
                stop.byte.0
            );
        }
    }
    let ends: Vec<_> = layout.rows.iter().map(|r| r.range.clone()).collect();
    for pair in ends.windows(2) {
        assert_eq!(pair[0].end, pair[1].start, "{text:?}: rows must touch");
    }
    assert_eq!(ends.last().unwrap().end.0, text.len(), "{text:?}");
}

#[test]
fn wrapping_keeps_clusters_whole_at_any_width() {
    let units = [
        "\u{1F1FA}\u{1F1F8}",
        "1\u{FE0F}\u{20E3}",
        "\u{1F469}\u{200D}\u{1F4BB}",
        "a\u{200D}b",
        "\u{1100}\u{1161}\u{11A8}",
        "\u{644}\u{627}",
        "e\u{301}\u{302}",
    ];
    for family in ["Cascadia Mono", "Consolas", "Segoe UI"] {
        let engine = DirectWriteLayout::with_font(family, 10.0, NORMAL).unwrap();
        let cell = engine.cell_width().0;
        for unit in units {
            for prefix in ["", "x", "abc ", "abcdefg"] {
                for count in [1, 2, 7] {
                    let text = format!("{prefix}{}", unit.repeat(count));
                    for width in [
                        1.0,
                        cell,
                        cell * 1.5,
                        cell * 2.0,
                        cell * 3.0,
                        cell * 5.5,
                        cell * 9.0,
                    ] {
                        let layout = line(&engine, &text, width);
                        assert_no_stop_inside(&text, unit, prefix.len(), &layout);
                        let mut bottom = 0.0;
                        for row in &layout.rows {
                            assert!((row.y.0 - bottom).abs() < 0.01, "{text:?} width {width}");
                            bottom += row.height.0;
                            for run in &row.glyph_runs {
                                assert!(
                                    run.origin.y.0 >= row.y.0
                                        && run.origin.y.0 <= row.y.0 + row.height.0,
                                    "{text:?} width {width}: a run sits outside its row"
                                );
                            }
                        }
                        assert!((layout.height.0 - bottom).abs() < 0.01);
                    }
                }
            }
        }
    }
}

#[test]
fn a_cluster_wider_than_the_row_stays_on_one_row() {
    let engine = DirectWriteLayout::new().unwrap();
    let flag = "\u{1F1FA}\u{1F1F8}";
    let l = line(&engine, flag, 1.0);
    assert_eq!(
        l.rows.len(),
        1,
        "{:?}",
        l.rows.iter().map(|r| r.range.clone()).collect::<Vec<_>>()
    );
    assert_eq!(stops(&l), [0, 8]);
    let two = line(&engine, &flag.repeat(2), 1.0);
    assert_eq!(two.rows.len(), 2);
    assert_eq!(stops(&two), [0, 8, 8, 16]);
}

#[test]
fn a_run_edge_inside_a_cluster_is_no_caret_stop() {
    // The joiner keeps `a` and the Hebrew letter whole, though they run in two directions.
    let engine = DirectWriteLayout::new().unwrap();
    let text = "a\u{200D}\u{5D0}b";
    let got = stops(&line(&engine, text, 1000.0));
    assert!(!got.contains(&1) && !got.contains(&4), "{got:?}");
}

// Astra finding C.
#[test]
fn removing_many_runs_costs_about_one_edit_and_memory_is_reused() {
    let lines = 1usize << 20;
    let mut map = HeightMap::new(lines, Dip(10.0)).unwrap();
    for line in 0..lines {
        map.set(
            LineIndex(line),
            Dip(if line % 2 == 0 { 20.0 } else { 30.0 }),
        )
        .unwrap();
    }
    let runs = map.run_count();
    let bytes = map.memory_bytes();
    let start = std::time::Instant::now();
    map.remove_lines(100..lines - 100).unwrap();
    let micros = start.elapsed().as_secs_f64() * 1e6;
    println!(
        "removing {} runs at once: {micros:.1} us",
        runs - map.run_count()
    );
    assert!(map.run_count() <= 201, "{}", map.run_count());
    assert_eq!(map.len(), 200);
    assert!(micros < 200.0, "{micros} us: it visited the removed runs");
    assert_eq!(map.line_height(LineIndex(150)), Some(Dip(20.0)));
    map.insert_lines(LineIndex(100), lines - 200).unwrap();
    for line in 0..lines {
        map.set(
            LineIndex(line),
            Dip(if line % 2 == 0 { 20.0 } else { 30.0 }),
        )
        .unwrap();
    }
    assert!(
        map.memory_bytes() <= bytes + bytes / 8,
        "the removed runs were not reused"
    );
    assert_eq!(
        map.scroll_to_line(map.line_to_scroll(LineIndex(777))),
        LineIndex(777)
    );
}

// The font bound.
#[test]
fn requested_font_sizes_stay_bounded() {
    let engine = DirectWriteLayout::new().unwrap();
    for size in 0..1000 {
        let token = format!("Segoe UI {}", 8.0 + size as f32 / 10.0);
        assert!(engine.measure("x", &token).width.0 > 0.0);
    }
    assert!(qe_layout::loaded_font_count() <= qe_layout::MAX_LOADED_FONTS);
    assert!(qe_layout::thread_format_count() <= qe_layout::MAX_LOADED_FONTS);
    // A font dropped from the cache keeps working for the engine that holds it.
    assert!(line(&engine, "abc", 1000.0).rows[0].glyph_runs[0].font_id != 0);
}

#[test]
fn the_face_registry_does_not_grow_with_repeat_layouts() {
    let text = "abc \u{6F22}\u{645}\u{1F600}";
    let engine = DirectWriteLayout::new().unwrap();
    line(&engine, text, 1000.0);
    let before = qe_layout::registered_face_count();
    for size in 0..200 {
        let other = DirectWriteLayout::with_font("Cascadia Mono", 9.0 + size as f32 / 10.0, NORMAL);
        line(&other.unwrap(), text, 1000.0);
    }
    // Other test threads may add their own fonts at the same time, but only a few.
    assert!(qe_layout::registered_face_count() < before + 40);
}
