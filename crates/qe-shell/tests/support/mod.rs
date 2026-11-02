//! Shared by the tests and examples: DirectWrite measuring and saved images through the
//! real renderer. Root's lock has no `qe-layout` for this crate, so that this measurer
//! copies what `DirectWriteLayout::measure` does, with the same font tokens.
#![allow(dead_code)]
use qe_api::{
    Bitmap, Dip, DisplayList, DrawCommand, Rect, Renderer, TextMeasurer, TextMetrics, Theme, View,
};
use qe_render::{BitmapRenderer, TextSettings};
use qe_shell::TabStrip;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_LINE_METRICS, DWRITE_TEXT_METRICS, DWriteCreateFactory,
    IDWriteFactory,
};
use windows::core::{HSTRING, w};

pub struct RealMeasurer;
impl TextMeasurer for RealMeasurer {
    fn measure(&self, text: &str, token: &str) -> TextMetrics {
        let (family, points) = token
            .rsplit_once(' ')
            .and_then(|(f, v)| v.parse::<f32>().ok().map(|p| (f, p)))
            .unwrap_or_else(|| panic!("a test token needs a size: {token:?}"));
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap();
            let format = factory
                .CreateTextFormat(
                    &HSTRING::from(family),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    points * 96.0 / 72.0,
                    w!("en-us"),
                )
                .unwrap();
            let layout = factory
                .CreateTextLayout(&utf16, &format, 1_000_000.0, 1_000_000.0)
                .unwrap();
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics).unwrap();
            let mut lines = vec![DWRITE_LINE_METRICS::default(); metrics.lineCount.max(1) as usize];
            let mut count = 0;
            layout.GetLineMetrics(Some(&mut lines), &mut count).unwrap();
            TextMetrics {
                width: Dip(metrics.widthIncludingTrailingWhitespace),
                height: Dip(metrics.height),
                baseline: Dip(lines[0].baseline),
            }
        }
    }
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(w),
        height: Dip(h),
    }
}

/// Draws a strip through the real renderer, with the text area under it, at this DPI.
pub fn render_strip(
    strip: &TabStrip,
    theme: &Theme,
    dpi: u32,
    width_dips: f32,
    height_dips: f32,
) -> Bitmap {
    let scale = dpi as f32 / 96.0;
    let (w, h) = (
        (width_dips * scale).round() as u32,
        (height_dips * scale).round() as u32,
    );
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, width_dips, height_dips),
        color: theme.text_background,
    });
    strip.set_scale(scale);
    strip.draw(&mut list, theme, &RealMeasurer);
    let mut renderer = BitmapRenderer::new(w, h).unwrap();
    renderer.set_dpi(dpi).unwrap();
    renderer
        .set_text_settings(TextSettings::reference())
        .unwrap();
    renderer.begin_frame().unwrap();
    renderer.draw(&list).unwrap();
    renderer.end_frame().unwrap();
    renderer.bitmap().unwrap()
}

/// One pixel as `[r, g, b]`.
pub fn pixel(image: &Bitmap, x: u32, y: u32) -> [u8; 3] {
    let at = (y * image.width + x) as usize * 4;
    [image.bgra[at + 2], image.bgra[at + 1], image.bgra[at]]
}
/// The box of pixels inside `area` that differ from `background` by more than `limit`,
/// as left, top, right and bottom, with right and bottom one past the last pixel.
pub fn ink_box(
    image: &Bitmap,
    area: (u32, u32, u32, u32),
    background: [u8; 3],
    limit: u8,
) -> Option<(u32, u32, u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in area.1..area.3.min(image.height) {
        for x in area.0..area.2.min(image.width) {
            let p = pixel(image, x, y);
            if p.iter().zip(background).any(|(a, b)| a.abs_diff(b) > limit) {
                found = Some(match found {
                    None => (x, y, x + 1, y + 1),
                    Some(b) => (b.0.min(x), b.1.min(y), b.2.max(x + 1), b.3.max(y + 1)),
                });
            }
        }
    }
    found
}

fn crc(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for b in data {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb88320 } else { 0 };
        }
    }
    !crc
}
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut v = kind.to_vec();
    v.extend_from_slice(data);
    out.extend_from_slice(&crc(&v).to_be_bytes());
}
pub fn raw_pixels(b: &Bitmap) -> Vec<u8> {
    let mut raw = Vec::with_capacity((b.width * b.height * 4 + b.height) as usize);
    for row in b.bgra.chunks_exact(b.width as usize * 4) {
        raw.push(0);
        for p in row.as_chunks::<4>().0 {
            raw.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    raw
}
/// A PNG with stored blocks, so that a saved image reads back with no decoder.
pub fn encode_png(b: &Bitmap) -> Vec<u8> {
    let raw = raw_pixels(b);
    let mut z = vec![0x78, 0x01];
    let (mut a, mut c) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65521;
        c = (c + a) % 65521;
    }
    let blocks = raw.chunks(65535).collect::<Vec<_>>();
    for (i, block) in blocks.iter().enumerate() {
        let len = block.len() as u16;
        z.push(u8::from(i + 1 == blocks.len()));
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&((c << 16) | a).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&b.width.to_be_bytes());
    ihdr.extend_from_slice(&b.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    png
}
/// Reads back the raw rows of a PNG that `encode_png` wrote.
pub fn decode_own_png(bytes: &[u8]) -> Vec<u8> {
    let mut at = 8;
    let mut z = Vec::new();
    while at + 12 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind = &bytes[at + 4..at + 8];
        if kind == b"IDAT" {
            z.extend_from_slice(&bytes[at + 8..at + 8 + len]);
        }
        at += 12 + len;
    }
    let mut out = Vec::new();
    let mut i = 2;
    loop {
        let last = z[i] == 1;
        let len = u16::from_le_bytes([z[i + 1], z[i + 2]]) as usize;
        out.extend_from_slice(&z[i + 5..i + 5 + len]);
        i += 5 + len;
        if last {
            return out;
        }
    }
}
