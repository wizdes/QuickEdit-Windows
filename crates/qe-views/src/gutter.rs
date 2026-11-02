//! The gutter's numbers, as `LineNumberRulerView.swift` has them.
use qe_editor::VisibleRow;

/// The Mac uses the system's digit-aligned font. Segoe UI's digits all have one width.
pub const GUTTER_FONT: &str = "Segoe UI";
pub const GUTTER_LEFT_PADDING: f32 = 6.0;
pub const GUTTER_RIGHT_PADDING: f32 = 8.0;
const GUTTER_MINIMUM_DIGITS: usize = 3;
/// Typing inside a line redraws the numbers this long after the first key.
pub const GUTTER_REDRAW_SECONDS: f64 = 0.1;

/// The numbers' size in points: 85% of the text's, rounded, and at least 9.
pub fn gutter_font_points(text_points: f32) -> f32 {
    (text_points * 0.85).round().max(9.0)
}
/// The digits the gutter is sized for: at least 3.
pub fn gutter_digits(lines: usize) -> usize {
    let mut digits = 1;
    let mut rest = lines / 10;
    while rest > 0 {
        digits += 1;
        rest /= 10;
    }
    digits.max(GUTTER_MINIMUM_DIGITS)
}
/// The gutter's width in dips, rounded up as on the Mac.
pub fn gutter_width(digits: usize, digit_width: f32) -> f32 {
    (digits as f32 * digit_width + GUTTER_LEFT_PADDING + GUTTER_RIGHT_PADDING).ceil()
}

/// One number on screen. `y` is the top of the number's box, in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GutterLabel {
    pub number: usize,
    pub y: f32,
}

/// One number for each line whose first row is on screen, centered on that row. Rows that
/// are shorter than a number's ink, far zoomed out, skip the numbers that would overlap.
///
/// `number_height` is the font's line box, which centers the number. `ink_height` decides
/// the overlap. The Mac has only one height, because its system font's box fits its rows.
/// Segoe UI's box is 16 dips at 9 pt, and Cascadia Mono's 10 pt row is 15.6, so the box
/// would hide every second number at 100%.
pub fn gutter_labels(rows: &[VisibleRow], number_height: f32, ink_height: f32) -> Vec<GutterLabel> {
    let mut labels = Vec::new();
    let mut last_bottom = f32::NEG_INFINITY;
    for row in rows.iter().filter(|row| !row.continuation) {
        let middle = row.y.0 + row.height.0 / 2.0;
        if middle - ink_height / 2.0 < last_bottom {
            continue;
        }
        labels.push(GutterLabel {
            number: row.line.0.saturating_add(1),
            y: middle - number_height / 2.0,
        });
        last_bottom = middle + ink_height / 2.0;
    }
    labels
}
