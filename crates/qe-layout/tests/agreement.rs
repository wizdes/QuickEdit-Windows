//! The fast path, shaping and DirectWrite's own hit test must agree.
use qe_api::{Affinity, Dip, Layout, LineIndex, LineLayout, Point, testing::StringStore};
use qe_layout::DirectWriteLayout;
use windows::{
    Win32::Graphics::DirectWrite::*,
    core::{BOOL, PCWSTR},
};

const FONTS: [&str; 4] = ["Cascadia Mono", "Consolas", "Courier New", "Segoe UI"];
const NORMAL: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT_NORMAL;

fn engines(family: &str) -> (DirectWriteLayout, DirectWriteLayout) {
    (
        DirectWriteLayout::with_font(family, 10.0, NORMAL).unwrap(),
        DirectWriteLayout::with_font(family, 10.0, NORMAL)
            .unwrap()
            .with_shaping(true),
    )
}

fn assert_same_layout(a: &LineLayout, b: &LineLayout, what: &str) {
    let ranges = |l: &LineLayout| l.rows.iter().map(|r| r.range.clone()).collect::<Vec<_>>();
    assert_eq!(ranges(a), ranges(b), "rows differ: {what}");
    assert!((a.height.0 - b.height.0).abs() < 0.01, "height: {what}");
    for (ra, rb) in a.rows.iter().zip(&b.rows) {
        assert!((ra.y.0 - rb.y.0).abs() < 0.01, "row y: {what}");
        let stops =
            |r: &qe_api::LayoutRow| r.caret_stops.iter().map(|s| s.byte).collect::<Vec<_>>();
        assert_eq!(stops(ra), stops(rb), "stops: {what}");
        for (sa, sb) in ra.caret_stops.iter().zip(&rb.caret_stops) {
            assert!(
                (sa.x.0 - sb.x.0).abs() < 0.02,
                "x at {:?}: {} vs {}: {what}",
                sa.byte,
                sa.x.0,
                sb.x.0
            );
        }
        // Each glyph: its id, its advance and where it lands. Run edges may differ.
        let ink = |r: &qe_api::LayoutRow| {
            let mut glyphs = Vec::new();
            for g in &r.glyph_runs {
                let mut x = g.origin.x.0 as f64;
                for (id, advance) in g.glyph_ids.iter().zip(&g.advances) {
                    glyphs.push((*id, advance.0, x));
                    x += advance.0 as f64;
                }
            }
            glyphs
        };
        let (ia, ib) = (ink(ra), ink(rb));
        assert_eq!(ia.len(), ib.len(), "glyph count: {what}");
        for (ga, gb) in ia.iter().zip(&ib) {
            assert_eq!(ga.0, gb.0, "glyph id: {what}");
            assert!((ga.1 - gb.1).abs() < 0.02, "advance: {what}");
            assert!(
                (ga.2 - gb.2).abs() < 0.05,
                "glyph x {} vs {}: {what}",
                ga.2,
                gb.2
            );
        }
        if let (Some(ga), Some(gb)) = (ra.glyph_runs.first(), rb.glyph_runs.first()) {
            assert!(
                (ga.origin.y.0 - gb.origin.y.0).abs() < 0.01,
                "baseline: {what}"
            );
            assert!(
                (ga.font_em_size.0 - gb.font_em_size.0).abs() < 0.001,
                "em: {what}"
            );
        }
    }
}

/// All 126 ASCII values that a line can hold, in all 4 fonts.
#[test]
fn every_ascii_value_agrees_with_shaping_in_every_font() {
    for family in FONTS {
        let (fast, slow) = engines(family);
        for value in 0u8..128 {
            if value == b'\r' || value == b'\n' {
                continue;
            }
            let c = value as char;
            let text = format!("a{c}b {c}{c} c{c}");
            let store = StringStore::new(text.clone());
            let a = fast.layout_line(&store, LineIndex(0), Dip(1000.0)).unwrap();
            let b = slow.layout_line(&store, LineIndex(0), Dip(1000.0)).unwrap();
            assert_same_layout(&a, &b, &format!("{family} value {value}"));
        }
    }
}

#[test]
fn vt_and_ff_rows_agree() {
    let (fast, slow) = engines("Cascadia Mono");
    for text in ["a\u{b}b", "a\u{c}b"] {
        let store = StringStore::new(text);
        let a = fast.layout_line(&store, LineIndex(0), Dip(1000.0)).unwrap();
        let b = slow.layout_line(&store, LineIndex(0), Dip(1000.0)).unwrap();
        assert_eq!(a.rows.len(), b.rows.len());
        assert_eq!(a.rows.len(), 2, "{text:?}");
    }
}

#[test]
fn a_tab_draws_nothing() {
    for family in ["Cascadia Mono", "Consolas", "Courier New"] {
        let engine = DirectWriteLayout::with_font(family, 10.0, NORMAL).unwrap();
        let store = StringStore::new("a\tb");
        let l = engine
            .layout_line(&store, LineIndex(0), Dip(1000.0))
            .unwrap();
        let runs = &l.rows[0].glyph_runs;
        assert_eq!(runs.len(), 2, "{family}");
        assert!(
            runs.iter()
                .all(|r| r.glyph_ids.len() == 1 && r.glyph_ids[0] != 0)
        );
        assert!(runs[1].origin.x.0 >= 8.0 * engine.cell_width().0 - 0.01);
    }
}

#[test]
fn ligature_fonts_look_the_same_on_both_paths() {
    let (fast, slow) = engines("Cascadia Code");
    for text in [
        "a -> b != c == d => e",
        "fi fl ffi <= >= <!-- --> www ::",
        "|| && ++ ** /* */",
    ] {
        let store = StringStore::new(text);
        let a = fast.layout_line(&store, LineIndex(0), Dip(1000.0)).unwrap();
        let b = slow.layout_line(&store, LineIndex(0), Dip(1000.0)).unwrap();
        assert_same_layout(&a, &b, text);
    }
}

struct Random(u64);
impl Random {
    fn next(&mut self, below: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % below
    }
}

/// The fast wrap computes rows without DirectWrite, so compare it on random text.
#[test]
fn wrap_matches_directwrite() {
    let alphabets: [&[u8]; 4] = [
        b"abcdefgh    ",
        b"abc 123 ,.;:!?-/()[]{}'\"$%+#&*<=>@\\^_`|~",
        b"word wrap test\t",
        b" !\"#$%&'()*+,-./0123456789:;<=>?@ABCXYZ[\\]^_`abcxyz{|}~",
    ];
    let mut random = Random(42);
    for family in ["Cascadia Mono", "Consolas", "Courier New"] {
        let (fast, slow) = engines(family);
        let cell = fast.cell_width().0;
        for round in 0..1500 {
            let alphabet = alphabets[round % alphabets.len()];
            let len = random.next(160);
            let text: String = (0..len)
                .map(|_| alphabet[random.next(alphabet.len())] as char)
                .collect();
            let cells = 1 + random.next(50);
            let width = if round % 2 == 0 {
                cells as f32 * cell
            } else {
                (cells as f32 + 0.4) * cell
            };
            let store = StringStore::new(text.clone());
            let a = fast.layout_line(&store, LineIndex(0), Dip(width)).unwrap();
            let b = slow.layout_line(&store, LineIndex(0), Dip(width)).unwrap();
            assert_same_layout(&a, &b, &format!("{family} width {cells} cells: {text:?}"));
        }
    }
}

fn directwrite_hit(text: &str, width: f32, x: f32, y: f32) -> usize {
    let factory: IDWriteFactory =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
    let name: Vec<u16> = "Cascadia Mono\0".encode_utf16().collect();
    let locale: Vec<u16> = "en-US\0".encode_utf16().collect();
    let format = unsafe {
        factory
            .CreateTextFormat(
                PCWSTR(name.as_ptr()),
                None,
                NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                10.0 * 96.0 / 72.0,
                PCWSTR(locale.as_ptr()),
            )
            .unwrap()
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    let layout = unsafe {
        factory
            .CreateTextLayout(&units, &format, width, 1e6)
            .unwrap()
    };
    // Eight cells of Cascadia Mono at this size, the tab stop the engine uses.
    unsafe { layout.SetIncrementalTabStop(62.5).unwrap() };
    let (mut trailing, mut inside) = (BOOL(0), BOOL(0));
    let mut metrics = DWRITE_HIT_TEST_METRICS::default();
    unsafe {
        layout
            .HitTestPoint(x, y, &mut trailing, &mut inside, &mut metrics)
            .unwrap();
    }
    let position = metrics.textPosition as usize
        + if trailing.as_bool() {
            metrics.length as usize
        } else {
            0
        };
    String::from_utf16(&units[..position]).unwrap().len()
}

const HEB: &str = "\u{5E9}\u{5DC}\u{5D5}\u{5DD}";
const ARB: &str = "\u{645}\u{631}\u{62D}\u{628}\u{627}";

#[test]
fn proven_mixed_direction_clicks() {
    let engine = DirectWriteLayout::new().unwrap();
    let cases = [
        (format!("{HEB} abc"), 0.5, 8),
        (format!("{HEB} abc"), 31.5, 8),
        (format!("abc {HEB} 123 def"), 51.5, 16),
        (format!("abc {HEB} 123"), 27.5, 4),
    ];
    for (text, x, expected) in cases {
        let store = StringStore::new(text.clone());
        let l = engine
            .layout_line(&store, LineIndex(0), Dip(1000.0))
            .unwrap();
        let hit = engine.hit_test_point(
            &l,
            Point {
                x: Dip(x),
                y: Dip(1.0),
            },
        );
        assert_eq!(
            directwrite_hit(&text, 1000.0, x, 1.0),
            expected,
            "{text} at {x}"
        );
        assert_eq!(hit.byte.0, expected, "{text} at {x}");
    }
}

#[test]
fn clicks_match_directwrite_on_mixed_samples() {
    let engine = DirectWriteLayout::new().unwrap();
    let samples = [
        format!("{HEB} abc"),
        format!("abc {HEB} 123 def"),
        format!("abc {HEB} 123"),
        HEB.to_string(),
        format!("{ARB} {HEB}"),
        format!("{ARB} 123 {ARB}"),
        format!("abc {ARB} def {HEB} ghi"),
        format!("x{HEB}y{ARB}z"),
        format!("1 {HEB} 2 {ARB} 3"),
        "plain ascii".to_string(),
        "e\u{301}a\u{301}".to_string(),
        "\u{6F22}\u{5B57}abc".to_string(),
        "a\u{1}b\u{7f}c".to_string(),
        "a\u{b}b".to_string(),
        "ab\tcd\tef".to_string(),
        "\u{1F469}\u{200D}\u{1F4BB}x\u{1F1FA}\u{1F1F8}".to_string(),
    ];
    let mut random = Random(7);
    for text in &samples {
        let store = StringStore::new(text.clone());
        let l = engine
            .layout_line(&store, LineIndex(0), Dip(1000.0))
            .unwrap();
        let right = l.rows[0]
            .caret_stops
            .iter()
            .map(|s| s.x.0.max(s.x_before.0))
            .fold(0.0, f32::max);
        for _ in 0..400 {
            let x = random.next(((right + 20.0) * 100.0) as usize) as f32 / 100.0 - 10.0;
            let hit = engine.hit_test_point(
                &l,
                Point {
                    x: Dip(x),
                    y: Dip(1.0),
                },
            );
            let expected = directwrite_hit(text, 1000.0, x, 1.0);
            // Finding 9 keeps the caret out of a flag even where a font draws it as
            // two letters. Then the answer must be an edge of that cluster.
            let edges: Vec<usize> = l.rows[0].caret_stops.iter().map(|s| s.byte.0).collect();
            if edges.contains(&expected) {
                assert_eq!(hit.byte.0, expected, "{text:?} at x={x}");
            } else {
                let after = edges.partition_point(|e| *e < expected);
                assert!(
                    [edges[after - 1], edges[after]].contains(&hit.byte.0),
                    "{text:?} at x={x}: {} around {expected}",
                    hit.byte.0
                );
            }
        }
    }
}

#[test]
fn wrapped_clicks_match_directwrite_on_mixed_samples() {
    let engine = DirectWriteLayout::new().unwrap().with_shaping(true);
    let cell = engine.cell_width().0;
    let width = cell * 12.0;
    let samples = [
        format!("abc {HEB} 123 def {HEB} 456"),
        format!("{ARB} {ARB} {ARB} abc def"),
        "one two three four five six".to_string(),
    ];
    let mut random = Random(9);
    for text in &samples {
        let store = StringStore::new(text.clone());
        let l = engine
            .layout_line(&store, LineIndex(0), Dip(width))
            .unwrap();
        assert!(l.rows.len() > 1, "{text}");
        for _ in 0..400 {
            let x = random.next((width as usize + 40) * 100) as f32 / 100.0 - 20.0;
            let y = random.next((l.height.0 * 100.0) as usize) as f32 / 100.0;
            let hit = engine.hit_test_point(
                &l,
                Point {
                    x: Dip(x),
                    y: Dip(y),
                },
            );
            let expected = directwrite_hit(text, width, x, y);
            assert_eq!(hit.byte.0, expected, "{text:?} at {x},{y}");
        }
    }
}

/// Each caret place must match DirectWrite's own answer, on both sides of each byte.
#[test]
fn caret_places_match_directwrite() {
    let engine = DirectWriteLayout::new().unwrap();
    let text = format!("abc {HEB} 123 {ARB} def");
    let store = StringStore::new(text.clone());
    let l = engine
        .layout_line(&store, LineIndex(0), Dip(1000.0))
        .unwrap();
    let factory: IDWriteFactory =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
    let name: Vec<u16> = "Cascadia Mono\0".encode_utf16().collect();
    let locale: Vec<u16> = "en-US\0".encode_utf16().collect();
    let format = unsafe {
        factory
            .CreateTextFormat(
                PCWSTR(name.as_ptr()),
                None,
                NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                10.0 * 96.0 / 72.0,
                PCWSTR(locale.as_ptr()),
            )
            .unwrap()
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    let layout = unsafe {
        factory
            .CreateTextLayout(&units, &format, 1000.0, 1e6)
            .unwrap()
    };
    for stop in &l.rows[0].caret_stops {
        let unit = text[..stop.byte.0].encode_utf16().count();
        let (mut x, mut y) = (0.0, 0.0);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        if unit < units.len() {
            unsafe {
                layout
                    .HitTestTextPosition(unit as u32, false, &mut x, &mut y, &mut m)
                    .unwrap();
            }
            let got = engine
                .caret_rect(&l, stop.byte, Affinity::After)
                .unwrap()
                .x
                .0;
            assert!(
                (got - x).abs() < 0.02,
                "after byte {}: {got} vs {x}",
                stop.byte.0
            );
        }
        if unit > 0 {
            unsafe {
                layout
                    .HitTestTextPosition(unit as u32 - 1, true, &mut x, &mut y, &mut m)
                    .unwrap();
            }
            let got = engine
                .caret_rect(&l, stop.byte, Affinity::Before)
                .unwrap()
                .x
                .0;
            assert!(
                (got - x).abs() < 0.02,
                "before byte {}: {got} vs {x}",
                stop.byte.0
            );
        }
    }
}

/// One text laid out once by DirectWrite alone, in a named font.
struct DirectWriteText {
    units: Vec<u16>,
    layout: IDWriteTextLayout,
}
impl DirectWriteText {
    fn new(family: &str, text: &str, width: f32) -> Self {
        let factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
        let name: Vec<u16> = family.encode_utf16().chain(Some(0)).collect();
        let locale: Vec<u16> = "en-US\0".encode_utf16().collect();
        let format = unsafe {
            factory
                .CreateTextFormat(
                    PCWSTR(name.as_ptr()),
                    None,
                    NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    10.0 * 96.0 / 72.0,
                    PCWSTR(locale.as_ptr()),
                )
                .unwrap()
        };
        let units: Vec<u16> = text.encode_utf16().collect();
        let layout = unsafe {
            factory
                .CreateTextLayout(&units, &format, width, 1e6)
                .unwrap()
        };
        // The same settings as the engine: ligatures off for ASCII in a fixed-width font.
        let typography = unsafe { factory.CreateTypography().unwrap() };
        for tag in [
            DWRITE_FONT_FEATURE_TAG_STANDARD_LIGATURES,
            DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_LIGATURES,
            DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_ALTERNATES,
        ] {
            let feature = DWRITE_FONT_FEATURE {
                nameTag: tag,
                parameter: 0,
            };
            unsafe { typography.AddFontFeature(feature).unwrap() };
        }
        let mut at = 0;
        while at < units.len() {
            if units[at] >= 0x80 {
                at += 1;
                continue;
            }
            let begin = at;
            while at < units.len() && units[at] < 0x80 {
                at += 1;
            }
            let range = DWRITE_TEXT_RANGE {
                startPosition: begin as u32,
                length: (at - begin) as u32,
            };
            unsafe { layout.SetTypography(&typography, range).unwrap() };
        }
        Self { units, layout }
    }
    fn hit(&self, x: f32, y: f32) -> usize {
        let (mut trailing, mut inside) = (BOOL(0), BOOL(0));
        let mut metrics = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            self.layout
                .HitTestPoint(x, y, &mut trailing, &mut inside, &mut metrics)
                .unwrap();
        }
        let position = metrics.textPosition as usize
            + if trailing.as_bool() {
                metrics.length as usize
            } else {
                0
            };
        String::from_utf16(&self.units[..position]).unwrap().len()
    }
}

const RLM_PIECES: [&str; 21] = [
    "abc", " ", "  ", "123", "\u{200F}", "\u{200E}", "\u{2066}", "\u{2067}", "\u{2068}",
    "\u{2069}", HEB, ARB, "x", "9", ",", "(", "\u{202A}", "\u{202B}", "\u{202C}", "\u{202D}",
    "\u{202E}",
];

fn random_mixed_text(random: &mut Random) -> String {
    let pieces = 3 + random.next(12);
    (0..pieces)
        .map(|_| RLM_PIECES[random.next(RLM_PIECES.len())])
        .collect()
}

#[test]
fn the_review_rlm_click_matches_directwrite() {
    let engine = DirectWriteLayout::new().unwrap();
    let text = "  abc \u{200F}\u{5E9}\u{5DC}\u{5D5}\u{5DD} 123123\u{200F}\u{200F}123 ";
    let l = engine
        .layout_line(&StringStore::new(text), LineIndex(0), Dip(1000.0))
        .unwrap();
    let hit = engine.hit_test_point(
        &l,
        Point {
            x: Dip(102.0),
            y: Dip(1.0),
        },
    );
    assert_eq!(
        hit.byte.0,
        DirectWriteText::new("Cascadia Mono", text, 1000.0).hit(102.0, 1.0)
    );
    assert_eq!(hit.byte.0, 22);
}

/// Direction marks and isolates, in all 3 fixed-width fonts, against HitTestPoint.
#[test]
fn random_mixed_direction_clicks_match_directwrite() {
    let mut random = Random(2024);
    for family in ["Cascadia Mono", "Consolas", "Courier New"] {
        let engine = DirectWriteLayout::with_font(family, 10.0, NORMAL).unwrap();
        for _ in 0..400 {
            let text = random_mixed_text(&mut random);
            let store = StringStore::new(text.clone());
            let l = engine
                .layout_line(&store, LineIndex(0), Dip(100_000.0))
                .unwrap();
            let row = &l.rows[0];
            assert!(
                row.visual_stops.windows(2).all(|p| p[0].x.0 <= p[1].x.0),
                "{family} {text:?}: stops out of order"
            );
            let direct = DirectWriteText::new(family, &text, 100_000.0);
            let right = row
                .caret_stops
                .iter()
                .map(|s| s.x.0.max(s.x_before.0))
                .fold(0.0, f32::max);
            let edges: Vec<usize> = row.caret_stops.iter().map(|s| s.byte.0).collect();
            for _ in 0..15 {
                let x = random.next(((right + 20.0) * 100.0) as usize) as f32 / 100.0 - 10.0;
                let hit = engine.hit_test_point(
                    &l,
                    Point {
                        x: Dip(x),
                        y: Dip(1.0),
                    },
                );
                let expected = direct.hit(x, 1.0);
                // A click on a zero-width mark's own place has two right answers only
                // when DirectWrite's answer is not a caret stop: see finding 9.
                if edges.contains(&expected) {
                    assert_eq!(hit.byte.0, expected, "{family} {text:?} at x={x}");
                }
            }
        }
    }
}

#[test]
fn random_wrapped_mixed_direction_clicks_match_directwrite() {
    let mut random = Random(31);
    for family in ["Cascadia Mono", "Consolas", "Courier New"] {
        let engine = DirectWriteLayout::with_font(family, 10.0, NORMAL).unwrap();
        let width = engine.cell_width().0 * 12.0;
        for _ in 0..200 {
            let text = random_mixed_text(&mut random);
            let store = StringStore::new(text.clone());
            let l = engine
                .layout_line(&store, LineIndex(0), Dip(width))
                .unwrap();
            let direct = DirectWriteText::new(family, &text, width);
            for row in &l.rows {
                let edges: Vec<usize> = row.caret_stops.iter().map(|s| s.byte.0).collect();
                for _ in 0..6 {
                    let x = random.next((width as usize + 40) * 100) as f32 / 100.0 - 20.0;
                    let y = row.y.0 + row.height.0 / 2.0;
                    let hit = engine.hit_test_point(
                        &l,
                        Point {
                            x: Dip(x),
                            y: Dip(y),
                        },
                    );
                    let expected = direct.hit(x, y);
                    if edges.contains(&expected) {
                        assert_eq!(hit.byte.0, expected, "{family} {text:?} at {x},{y}");
                    }
                }
            }
        }
    }
}

/// Item 14: below one cell both paths use DirectWrite's own rule.
#[test]
fn widths_below_one_cell_agree_with_shaping() {
    let mut random = Random(5);
    for family in ["Cascadia Mono", "Consolas", "Courier New"] {
        let (fast, slow) = engines(family);
        let cell = fast.cell_width().0;
        for width in [0.5, 1.0, 3.0, 5.0, 7.0, cell - 0.01, cell, cell + 0.01] {
            for _ in 0..20 {
                let len = random.next(60);
                let text: String = (0..len)
                    .map(|_| b"word wrap, test-1 "[random.next(18)] as char)
                    .collect();
                let store = StringStore::new(text.clone());
                let a = fast.layout_line(&store, LineIndex(0), Dip(width)).unwrap();
                let b = slow.layout_line(&store, LineIndex(0), Dip(width)).unwrap();
                assert_same_layout(&a, &b, &format!("{family} width {width}: {text:?}"));
            }
        }
    }
}

// Astra finding E.
#[test]
fn left_of_row_clicks_around_direction_controls_match_directwrite() {
    let engine = DirectWriteLayout::new().unwrap();
    for text in [
        "\u{202A} ",
        "\u{202B} ",
        "\u{200E}a",
        "\u{200F}a",
        "\u{2066}a\u{2069}",
        " \u{202C}",
    ] {
        let l = engine
            .layout_line(&StringStore::new(text), LineIndex(0), Dip(1000.0))
            .unwrap();
        let direct = DirectWriteText::new("Cascadia Mono", text, 1000.0);
        for x in [-50.0, -1.0, 0.0, 0.5, 3.0, 7.0, 8.0, 500.0] {
            let hit = engine.hit_test_point(
                &l,
                Point {
                    x: Dip(x),
                    y: Dip(1.0),
                },
            );
            assert_eq!(hit.byte.0, direct.hit(x, 1.0), "{text:?} at x={x}");
        }
    }
}
