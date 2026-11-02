//! Helpers shared by the qe-render tests. Each test binary uses only some of them.
#![allow(dead_code)]
use qe_api::{Bitmap, Color, Dip, DisplayList, DrawCommand, Point, Rect, Renderer};
use qe_render::{BitmapRenderer, TextSettings};
use std::mem::ManuallyDrop;
use windows::{
    Win32::{
        Foundation::COLORREF,
        Graphics::{DirectWrite::*, Gdi::*},
    },
    core::{BOOL, HSTRING, PCWSTR},
};

pub const MONO: &str = "Cascadia Mono";
pub const UI: &str = "Segoe UI";
pub const EMOJI: &str = "Segoe UI Emoji";
pub const WHITE: Color = Color {
    r: 255,
    g: 255,
    b: 255,
    a: 255,
};
pub const BLACK: Color = Color {
    r: 0,
    g: 0,
    b: 0,
    a: 255,
};
pub const DARK_BG: Color = Color {
    r: 30,
    g: 30,
    b: 30,
    a: 255,
};
pub const DARK_INK: Color = Color {
    r: 230,
    g: 230,
    b: 230,
    a: 255,
};

pub fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(width),
        height: Dip(height),
    }
}
pub fn point(x: f32, y: f32) -> Point {
    Point {
        x: Dip(x),
        y: Dip(y),
    }
}
pub fn fill(list: &mut DisplayList, r: Rect, color: Color) {
    list.push(DrawCommand::FillRect { rect: r, color });
}

fn factory() -> IDWriteFactory {
    unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() }
}
pub fn face(family: &str) -> IDWriteFontFace {
    let factory = factory();
    let mut collection = None;
    unsafe {
        factory
            .GetSystemFontCollection(&mut collection, false)
            .unwrap()
    };
    let collection = collection.unwrap();
    let (mut index, mut exists) = (0, BOOL(0));
    let name = HSTRING::from(family);
    unsafe {
        collection
            .FindFamilyName(PCWSTR(name.as_ptr()), &mut index, &mut exists)
            .unwrap();
    }
    assert!(exists.as_bool(), "{family} is not installed");
    unsafe {
        collection
            .GetFontFamily(index)
            .unwrap()
            .GetFirstMatchingFont(
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
            )
            .unwrap()
            .CreateFontFace()
            .unwrap()
    }
}
/// Glyph ids and advances in dips for `text`, one glyph for each character.
pub fn glyphs(family: &str, em: f32, text: &str) -> (Vec<u16>, Vec<f32>) {
    let face = face(family);
    let points: Vec<u32> = text.chars().map(|c| c as u32).collect();
    let mut ids = vec![0u16; points.len()];
    let mut design = DWRITE_FONT_METRICS::default();
    let mut metrics = vec![DWRITE_GLYPH_METRICS::default(); points.len()];
    unsafe {
        face.GetGlyphIndices(points.as_ptr(), points.len() as u32, ids.as_mut_ptr())
            .unwrap();
        face.GetMetrics(&mut design);
        face.GetDesignGlyphMetrics(ids.as_ptr(), ids.len() as u32, metrics.as_mut_ptr(), false)
            .unwrap();
    }
    let advances = metrics
        .iter()
        .map(|m| m.advanceWidth as f32 * em / design.designUnitsPerEm as f32)
        .collect();
    (ids, advances)
}
pub fn glyph_run(
    id: u32,
    family: &str,
    em: f32,
    text: &str,
    at: Point,
    color: Color,
) -> DrawCommand {
    let (ids, advances) = glyphs(family, em, text);
    let count = ids.len();
    DrawCommand::GlyphRun {
        at,
        bidi_level: 0,
        font_id: id,
        font_em_size: Dip(em),
        glyph_ids: ids,
        advances: advances.into_iter().map(Dip).collect(),
        offsets: vec![Point::default(); count],
        color,
    }
}
pub fn text_run(family: &str, size: f32, text: &str, at: Point, color: Color) -> DrawCommand {
    DrawCommand::TextRun {
        at,
        text: text.into(),
        font: family.into(),
        font_size: Dip(size),
        color,
    }
}

pub fn frame(renderer: &mut BitmapRenderer, list: &DisplayList) {
    renderer.begin_frame().unwrap();
    renderer.draw(list).unwrap();
    renderer.end_frame().unwrap();
}
/// One new renderer that draws `list` once, in full.
pub fn fresh(
    width: u32,
    height: u32,
    dpi: u32,
    list: &DisplayList,
    fonts: &[(u32, &str)],
) -> Bitmap {
    let mut renderer = BitmapRenderer::new(width, height).unwrap();
    renderer.set_dpi(dpi).unwrap();
    for (id, family) in fonts {
        renderer.register_font(*id, family).unwrap();
    }
    frame(&mut renderer, list);
    renderer.bitmap().unwrap()
}
pub fn pixel(image: &Bitmap, x: u32, y: u32) -> [u8; 3] {
    let i = (y * image.width + x) as usize * 4;
    [image.bgra[i], image.bgra[i + 1], image.bgra[i + 2]]
}
pub fn difference(a: &Bitmap, b: &Bitmap) -> usize {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.bgra
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.bgra.as_chunks::<4>().0)
        .filter(|(x, y)| x[..3] != y[..3])
        .count()
}
/// How many pixels have a channel that differs by more than `limit`.
pub fn difference_over(a: &Bitmap, b: &Bitmap, limit: u8) -> usize {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.bgra
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.bgra.as_chunks::<4>().0)
        .filter(|(x, y)| (0..3).any(|c| x[c].abs_diff(y[c]) > limit))
        .count()
}
/// How many pixels differ from `background`.
pub fn inked(image: &Bitmap, background: Color) -> usize {
    let bg = [background.b, background.g, background.r];
    image
        .bgra
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[..3] != bg)
        .count()
}
/// The sum over all channels of how far each one is from `background`.
pub fn ink_mass(image: &Bitmap, background: Color) -> u64 {
    let bg = [background.b, background.g, background.r];
    image
        .bgra
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| (0..3).map(|c| u64::from(p[c].abs_diff(bg[c]))).sum::<u64>())
        .sum()
}
/// Left, top, right and bottom of the pixels that differ from `background`.
pub fn ink_box(image: &Bitmap, background: Color) -> Option<(u32, u32, u32, u32)> {
    let mut found = None::<(u32, u32, u32, u32)>;
    for y in 0..image.height {
        for x in 0..image.width {
            if pixel(image, x, y) != [background.b, background.g, background.r] {
                let b = found.get_or_insert((x, y, x + 1, y + 1));
                b.0 = b.0.min(x);
                b.1 = b.1.min(y);
                b.2 = b.2.max(x + 1);
                b.3 = b.3.max(y + 1);
            }
        }
    }
    found
}

/// A DirectWrite bitmap target with one background, used as the reference for how
/// DirectWrite itself draws text.
pub struct Direct {
    target: IDWriteBitmapRenderTarget,
    params: IDWriteRenderingParams,
    width: u32,
    height: u32,
}
impl Direct {
    pub fn new(
        width: u32,
        height: u32,
        dpi: u32,
        settings: TextSettings,
        background: Color,
    ) -> Self {
        let factory = factory();
        let target = unsafe {
            factory
                .GetGdiInterop()
                .unwrap()
                .CreateBitmapRenderTarget(None, width, height)
                .unwrap()
        };
        unsafe { target.SetPixelsPerDip(dpi as f32 / 96.0).unwrap() };
        let params = unsafe {
            factory
                .CreateCustomRenderingParams(
                    settings.gamma,
                    settings.enhanced_contrast,
                    settings.cleartype_level,
                    settings.geometry,
                    settings.mode,
                )
                .unwrap()
        };
        let this = Self {
            target,
            params,
            width,
            height,
        };
        let (bits, stride) = this.bits();
        for y in 0..height as isize {
            for x in 0..width as isize {
                let p = unsafe { bits.offset(y * stride + x * 4) };
                let v = [background.b, background.g, background.r, 255];
                unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), p, 4) };
            }
        }
        this
    }
    fn bits(&self) -> (*mut u8, isize) {
        let dc = unsafe { self.target.GetMemoryDC() };
        let object = unsafe { GetCurrentObject(dc, OBJ_BITMAP) };
        let mut dib = DIBSECTION::default();
        unsafe {
            GetObjectW(
                object,
                std::mem::size_of::<DIBSECTION>() as i32,
                Some((&mut dib as *mut DIBSECTION).cast()),
            );
        }
        (dib.dsBm.bmBits.cast(), dib.dsBm.bmWidthBytes as isize)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        face: &IDWriteFontFace,
        em: f32,
        ids: &[u16],
        advances: &[f32],
        offsets: &[DWRITE_GLYPH_OFFSET],
        bidi: u8,
        at: (f32, f32),
        ink: Color,
    ) {
        let mut run = DWRITE_GLYPH_RUN {
            fontFace: ManuallyDrop::new(Some(face.clone())),
            fontEmSize: em,
            glyphCount: ids.len() as u32,
            glyphIndices: ids.as_ptr(),
            glyphAdvances: advances.as_ptr(),
            glyphOffsets: offsets.as_ptr(),
            isSideways: BOOL(0),
            bidiLevel: u32::from(bidi),
        };
        let color = COLORREF(u32::from(ink.r) | (u32::from(ink.g) << 8) | (u32::from(ink.b) << 16));
        unsafe {
            self.target
                .DrawGlyphRun(
                    at.0,
                    at.1,
                    DWRITE_MEASURING_MODE_NATURAL,
                    &run,
                    &self.params,
                    color,
                    None,
                )
                .unwrap();
            ManuallyDrop::drop(&mut run.fontFace);
        }
    }
    pub fn bitmap(&self) -> Bitmap {
        assert!(unsafe { GdiFlush() }.as_bool());
        let (bits, stride) = self.bits();
        let mut bgra = vec![0; self.width as usize * self.height as usize * 4];
        for y in 0..self.height as usize {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bits.offset(y as isize * stride),
                    bgra.as_mut_ptr().add(y * self.width as usize * 4),
                    self.width as usize * 4,
                );
            }
        }
        Bitmap {
            width: self.width,
            height: self.height,
            bgra,
        }
    }
}

/// Time limits differ because a test build has no optimization.
pub fn limit_ms(release: f64, debug: f64) -> f64 {
    if cfg!(debug_assertions) {
        debug
    } else {
        release
    }
}
pub fn median_ms(mut task: impl FnMut()) -> f64 {
    let mut samples = Vec::new();
    for _ in 0..5 {
        let start = std::time::Instant::now();
        task();
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    samples[2]
}
