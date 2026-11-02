//! What every arm draws, and the one font path that every arm shares.
//!
//! All four arms use the same DirectWrite lookup, the same glyphs and the same
//! pixel layout, so a timing gap between arms comes from the renderer.
use crate::ffi::{self, Com, FontMetrics, GlyphMetrics, H, Hr, check, v, wide};
use crate::vcall;
use std::mem;

pub const COLUMNS: i32 = 90;
pub const ROWS: usize = 30;
pub const MARGIN: i32 = 2;
/// Typed characters start right after the first line's text and its space.
pub const TYPED_COLUMNS: usize = 44;

pub const SAMPLE: [&str; ROWS] = [
    "QuickEdit renderer race - Cascadia Mono 10 pt",
    "0123456789 abcdefghijklmnopqrstuvwxyz",
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ !@#$%^&*()",
    "The quick brown fox jumps over the lazy dog.",
    "A: DirectWrite bitmap and BitBlt",
    "B: Direct2D and hardware swap chain",
    "C: Direct3D glyph atlas and waitable swap chain",
    "D: software first, then hardware",
    "",
    "fn main() { println!(\"Hello, world!\"); }",
    "let latency = after_present - key_received;",
    "if latency < budget { show_frame(); }",
    "Window size: 90 columns by 30 rows",
    "DPI aware per monitor, version 2",
    "White background. Black text.",
    "This same text appears in each renderer.",
    "Key presses append to the first line.",
    "The new character appears after this marker:",
    "A screen capture can watch that spot.",
    "The launcher records the first frame.",
    "The typing tool records screen changes.",
    "One window remains open for the whole run.",
    "Measure cold and warm starts separately.",
    "Percentiles use nearest rank.",
    "The GPU paths require a desktop session.",
    "No animation runs while idle.",
    "Test line 27: 11111111111111111111",
    "Test line 28: 22222222222222222222",
    "Test line 29: 33333333333333333333",
    "Test line 30: END OF SAMPLE TEXT",
];

/// The part of the typed text that shows on line 1. When the row fills, it
/// clears and starts again. Every new non-space key therefore lands on a blank
/// cell and changes pixels, which the typing tool depends on.
pub fn visible_typed(typed: &[char]) -> String {
    if typed.is_empty() {
        return String::new();
    }
    let start = (typed.len() - 1) / TYPED_COLUMNS * TYPED_COLUMNS;
    typed[start..].iter().collect()
}

pub fn line_text(row: usize, typed: &[char]) -> String {
    if row == 0 {
        format!("{} {}", SAMPLE[0], visible_typed(typed))
    } else {
        SAMPLE[row].to_owned()
    }
}

/// Pixel layout, shared by all arms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub cell: i32,
    pub line: i32,
    pub ascent: i32,
    pub width: i32,
    pub height: i32,
    pub em_px: f32,
}

impl Layout {
    /// `dpi` is the window's DPI. The font size is 10 pt. `metrics` holds the
    /// font's ascent, descent and line gap in design units.
    pub fn from_design(dpi: u32, em: u16, advance: u32, metrics: (u16, u16, i16)) -> Self {
        let em_px = 10.0 * dpi as f32 / 72.0;
        let scale = em_px / em as f32;
        let (ascent, descent, gap) = metrics;
        let cell = (advance as f32 * scale).round() as i32;
        let line = ((ascent as f32 + descent as f32 + gap as f32) * scale).round() as i32;
        Layout {
            cell,
            line,
            ascent: (ascent as f32 * scale).round() as i32,
            width: cell * COLUMNS + 2 * MARGIN,
            height: line * ROWS as i32,
            em_px,
        }
    }
    pub fn baseline(&self, row: usize) -> i32 {
        row as i32 * self.line + self.ascent
    }
}

pub struct Font {
    pub factory: Com,
    pub face: Com,
}

impl Font {
    /// Finds Cascadia Mono through the system font collection.
    pub fn open() -> Result<Self, String> {
        let create: unsafe extern "system" fn(u32, *const ffi::Guid, *mut H) -> Hr =
            unsafe { mem::transmute(ffi::system_proc("dwrite.dll", b"DWriteCreateFactory\0")?) };
        let mut factory = Com::null();
        check(
            unsafe { create(0, &ffi::IID_DWRITE_FACTORY, factory.slot()) },
            "DWriteCreateFactory",
        )?;
        let mut collection = Com::null();
        check(
            vcall!(
                factory.0,
                v::DWF_GET_SYSTEM_FONT_COLLECTION,
                fn(*mut H, i32) -> Hr,
                collection.slot(),
                0
            ),
            "GetSystemFontCollection",
        )?;
        let name = wide("Cascadia Mono");
        let (mut index, mut exists) = (0u32, 0i32);
        check(
            vcall!(
                collection.0,
                v::DWC_FIND_FAMILY_NAME,
                fn(*const u16, *mut u32, *mut i32) -> Hr,
                name.as_ptr(),
                &mut index,
                &mut exists
            ),
            "FindFamilyName",
        )?;
        if exists == 0 {
            return Err("the Cascadia Mono font is not installed".into());
        }
        let mut family = Com::null();
        check(
            vcall!(
                collection.0,
                v::DWC_GET_FONT_FAMILY,
                fn(u32, *mut H) -> Hr,
                index,
                family.slot()
            ),
            "GetFontFamily",
        )?;
        let mut font = Com::null();
        // Weight 400, normal stretch (5), normal style (0).
        check(
            vcall!(
                family.0,
                v::DWFAM_GET_FIRST_MATCHING_FONT,
                fn(u32, u32, u32, *mut H) -> Hr,
                400,
                5,
                0,
                font.slot()
            ),
            "GetFirstMatchingFont",
        )?;
        let mut face = Com::null();
        check(
            vcall!(
                font.0,
                v::DWFONT_CREATE_FONT_FACE,
                fn(*mut H) -> Hr,
                face.slot()
            ),
            "CreateFontFace",
        )?;
        Ok(Font { factory, face })
    }

    pub fn layout(&self, dpi: u32) -> Result<Layout, String> {
        let mut metrics = FontMetrics::default();
        vcall!(
            self.face.0,
            v::DWFACE_GET_METRICS,
            fn(*mut FontMetrics) -> (),
            &mut metrics
        );
        if metrics.em == 0 {
            return Err("Cascadia Mono reports zero design units per em".into());
        }
        let glyph = self.glyphs("M")?;
        let mut glyph_metrics = GlyphMetrics::default();
        check(
            vcall!(
                self.face.0,
                v::DWFACE_GET_DESIGN_GLYPH_METRICS,
                fn(*const u16, u32, *mut GlyphMetrics, i32) -> Hr,
                glyph.as_ptr(),
                1,
                &mut glyph_metrics,
                0
            ),
            "GetDesignGlyphMetrics",
        )?;
        Ok(Layout::from_design(
            dpi,
            metrics.em,
            glyph_metrics.advance_width,
            (metrics.ascent, metrics.descent, metrics.line_gap),
        ))
    }

    pub fn glyphs(&self, text: &str) -> Result<Vec<u16>, String> {
        let codepoints: Vec<u32> = text.chars().map(|c| c as u32).collect();
        let mut glyphs = vec![0u16; codepoints.len()];
        if !codepoints.is_empty() {
            check(
                vcall!(
                    self.face.0,
                    v::DWFACE_GET_GLYPH_INDICES,
                    fn(*const u32, u32, *mut u16) -> Hr,
                    codepoints.as_ptr(),
                    codepoints.len() as u32,
                    glyphs.as_mut_ptr()
                ),
                "GetGlyphIndices",
            )?;
        }
        Ok(glyphs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_has_30_lines_that_fit_in_90_columns() {
        assert_eq!(SAMPLE.len(), 30);
        for line in SAMPLE {
            assert!(line.chars().count() <= COLUMNS as usize, "{line}");
        }
        assert_eq!(SAMPLE[0].chars().count() + 1 + TYPED_COLUMNS, 90);
    }

    #[test]
    fn typed_text_clears_when_the_row_is_full_so_each_key_lands_on_a_blank_cell() {
        let keys: Vec<char> = "x".repeat(TYPED_COLUMNS + 1).chars().collect();
        assert_eq!(visible_typed(&[]), "");
        assert_eq!(visible_typed(&keys[..1]), "x");
        assert_eq!(visible_typed(&keys[..TYPED_COLUMNS]).len(), TYPED_COLUMNS);
        assert_eq!(visible_typed(&keys), "x");
    }

    #[test]
    fn only_line_one_shows_typed_text() {
        assert!(line_text(0, &['q']).ends_with(" q"));
        assert_eq!(line_text(1, &['q']), SAMPLE[1]);
    }

    #[test]
    fn layout_scales_with_dpi_and_gives_a_90_by_30_window() {
        let at_96 = Layout::from_design(96, 2048, 1200, (1900, 480, 0));
        assert_eq!(at_96.cell, 8);
        assert_eq!(at_96.width, 8 * 90 + 4);
        assert_eq!(at_96.height, at_96.line * 30);
        let at_192 = Layout::from_design(192, 2048, 1200, (1900, 480, 0));
        assert_eq!(at_192.em_px, at_96.em_px * 2.0);
        assert!(at_192.cell > at_96.cell);
    }
}
