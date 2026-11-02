//! What the picture tests and the gallery share: a real text measurer, the real
//! renderer, the scenes, and a small PNG reader and writer.
#![allow(dead_code)]
pub mod png;
pub mod scenes;

use qe_api::{Bitmap, Dip, DisplayList, Renderer, TextMeasurer, TextMetrics};
use qe_render::{BitmapRenderer, TextSettings};
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{BOOL, HSTRING, PCWSTR, w};

/// Measures with DirectWrite as `qe_layout::DirectWriteLayout` does. A token ends in
/// a size in points, such as "Segoe UI 9.75". qe-ui cannot use qe-layout here without
/// another line in Cargo.lock, so this copy names the same calls.
pub struct RealMeasure;
impl RealMeasure {
    fn try_measure(&self, text: &str, token: &str) -> windows::core::Result<TextMetrics> {
        let (family, points) = token
            .rsplit_once(' ')
            .and_then(|(family, size)| size.parse::<f32>().ok().map(|p| (family, p)))
            .expect("a measuring token ends in a size");
        let utf16: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: each call gets valid strings and out pointers.
        unsafe {
            let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut fonts = None;
            factory.GetSystemFontCollection(&mut fonts, false)?;
            let fonts = fonts.expect("the system has a font collection");
            let (mut index, mut exists) = (0, BOOL(0));
            fonts.FindFamilyName(
                PCWSTR(HSTRING::from(family).as_ptr()),
                &mut index,
                &mut exists,
            )?;
            assert!(exists.as_bool(), "{family} is not installed");
            let format = factory.CreateTextFormat(
                &HSTRING::from(family),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                points * 96.0 / 72.0,
                w!("en-us"),
            )?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let layout = factory.CreateTextLayout(&utf16, &format, 100_000.0, 1_000_000.0)?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics)?;
            let mut lines = vec![DWRITE_LINE_METRICS::default(); metrics.lineCount.max(1) as usize];
            let mut count = 0;
            layout.GetLineMetrics(Some(&mut lines), &mut count)?;
            Ok(TextMetrics {
                width: Dip(metrics.widthIncludingTrailingWhitespace),
                height: Dip(metrics.height),
                baseline: Dip(lines[0].baseline),
            })
        }
    }
}
impl TextMeasurer for RealMeasure {
    fn measure(&self, text: &str, font: &str) -> TextMetrics {
        self.try_measure(text, font)
            .unwrap_or_else(|e| panic!("measuring {text:?} in {font}: {e}"))
    }
}

/// Paints a list with the real renderer and fixed text settings. A draw error fails.
pub fn render(list: &DisplayList, width: u32, height: u32) -> Bitmap {
    let mut renderer = BitmapRenderer::new(width, height).expect("renderer");
    renderer
        .set_text_settings(TextSettings::reference())
        .expect("text settings");
    renderer
        .render_to_bitmap(list, width, height)
        .unwrap_or_else(|e| panic!("the renderer refused a command: {e}"))
}
