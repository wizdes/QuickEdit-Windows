//! Speed of warm frames. Run with `cargo run -p qe-render --release --locked --example bench`.
//! Each number is the median of 15 frames after warm-up frames.
use qe_api::{Color, Dip, DisplayList, DrawCommand, Point, Rect, Renderer};
use qe_render::BitmapRenderer;
use std::time::Instant;
use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::{DirectWrite::*, Gdi::*},
    },
    core::{BOOL, HSTRING, PCWSTR},
};

/// Any failure, as its message. The `windows` crate's error here does not implement
/// `std::error::Error`.
#[derive(Debug)]
struct Failure(String);
impl<E: std::fmt::Display> From<E> for Failure {
    fn from(e: E) -> Self {
        Self(e.to_string())
    }
}
const FONT: u32 = 1;
const LIGHT: (Color, Color) = (Color::rgb(255, 255, 255), Color::rgb(0, 0, 0));
const DARK: (Color, Color) = (Color::rgb(30, 30, 30), Color::rgb(230, 230, 230));
const LINE: &str =
    "QuickEdit speed test 0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ ";

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(w),
        height: Dip(h),
    }
}
fn glyphs(em: f32, text: &str) -> Result<(Vec<u16>, Vec<Dip>), Failure> {
    let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
    let mut collection = None;
    unsafe { factory.GetSystemFontCollection(&mut collection, false)? };
    let collection = collection.ok_or("no font collection")?;
    let (mut index, mut exists) = (0, BOOL(0));
    let name = HSTRING::from("Cascadia Mono");
    unsafe { collection.FindFamilyName(PCWSTR(name.as_ptr()), &mut index, &mut exists)? };
    if !exists.as_bool() {
        return Err("Cascadia Mono is not installed".into());
    }
    let face = unsafe {
        collection
            .GetFontFamily(index)?
            .GetFirstMatchingFont(
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
            )?
            .CreateFontFace()?
    };
    let points: Vec<u32> = text.chars().map(|c| c as u32).collect();
    let mut ids = vec![0u16; points.len()];
    let mut design = DWRITE_FONT_METRICS::default();
    let mut metrics = vec![DWRITE_GLYPH_METRICS::default(); points.len()];
    unsafe {
        face.GetGlyphIndices(points.as_ptr(), points.len() as u32, ids.as_mut_ptr())?;
        face.GetMetrics(&mut design);
        face.GetDesignGlyphMetrics(ids.as_ptr(), ids.len() as u32, metrics.as_mut_ptr(), false)?;
    }
    let advances = metrics
        .iter()
        .map(|m| Dip(m.advanceWidth as f32 * em / design.designUnitsPerEm as f32))
        .collect();
    Ok((ids, advances))
}
/// Rows of text across the window, as shaped text arrives from qe-layout.
fn glyph_frame(width: f32, height: f32, theme: (Color, Color)) -> Result<DisplayList, Failure> {
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, width, height),
        color: theme.0,
    });
    let em = 13.33;
    let (ids, advances) = glyphs(em, &LINE.repeat(width as usize / 650 + 1))?;
    let count = ids.len();
    let mut y = 16.0;
    while y < height {
        list.push(DrawCommand::GlyphRun {
            at: Point {
                x: Dip(2.0),
                y: Dip(y),
            },
            bidi_level: 0,
            font_id: FONT,
            font_em_size: Dip(em),
            glyph_ids: ids.clone(),
            advances: advances.clone(),
            offsets: vec![Point::default(); count],
            color: theme.1,
        });
        y += 17.0;
    }
    Ok(list)
}
fn text_frame(width: f32, height: f32, theme: (Color, Color)) -> DisplayList {
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, width, height),
        color: theme.0,
    });
    let line = LINE.repeat(width as usize / 650 + 1);
    let mut y = 16.0;
    while y < height {
        list.push(DrawCommand::TextRun {
            at: Point {
                x: Dip(2.0),
                y: Dip(y),
            },
            text: line.clone(),
            font: "Cascadia Mono".into(),
            font_size: Dip(13.33),
            color: theme.1,
        });
        y += 17.0;
    }
    list
}
fn median(mut task: impl FnMut() -> Result<(), Failure>) -> Result<f64, Failure> {
    for _ in 0..3 {
        task()?;
    }
    let mut samples = Vec::new();
    for _ in 0..15 {
        let start = Instant::now();
        task()?;
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    Ok(samples[7])
}
fn frame(renderer: &mut BitmapRenderer, list: &DisplayList) -> Result<(), Failure> {
    renderer.begin_frame()?;
    renderer.draw(list)?;
    renderer.end_frame()?;
    Ok(())
}
fn frame_all(renderer: &mut BitmapRenderer, list: &DisplayList) -> Result<(), Failure> {
    renderer.invalidate_all();
    frame(renderer, list)
}
fn window_frames(
    width: u32,
    height: u32,
    dpi: u32,
    theme: (Color, Color),
    name: &str,
) -> Result<(), Failure> {
    let s = dpi as f32 / 96.0;
    let (w, h) = (width as f32 / s, height as f32 / s);
    let mut renderer = BitmapRenderer::new(width, height)?;
    renderer.set_dpi(dpi)?;
    renderer.register_font(FONT, "Cascadia Mono")?;
    let list = glyph_frame(w, h, theme)?;
    frame(&mut renderer, &list)?;
    let full = median(|| frame_all(&mut renderer, &list))?;
    let line = median(|| {
        renderer.invalidate(rect(0.0, 0.0, w, 17.0));
        frame(&mut renderer, &list)
    })?;
    let pixels = (17.0 * s).round() as i32;
    let scroll = median(|| {
        renderer.scroll_pixels(rect(0.0, 0.0, w, h), -pixels)?;
        frame(&mut renderer, &list)
    })?;
    // The copy to a window, measured against a bitmap, so that no window opens. A real
    // window adds the desktop composition that this cannot measure.
    let target = MemoryTarget::new(width as i32, height as i32)?;
    let whole = RECT {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };
    let present_all = median(|| {
        renderer.present_rect_to_dc(target.dc, whole)?;
        Ok(())
    })?;
    let text = text_frame(w, h, theme);
    let text_full = median(|| frame_all(&mut renderer, &text))?;
    println!(
        "measured on this PC: {name} {width}x{height} at {}%: GlyphRun full {full:.3} ms, line {line:.3} ms, scroll {scroll:.3} ms, TextRun full {text_full:.3} ms, present full to a bitmap {present_all:.3} ms; cache {} glyphs, {} KiB; buffer {} bytes",
        dpi * 100 / 96,
        renderer.glyph_cache_entries(),
        renderer.glyph_cache_bytes() / 1024,
        renderer.back_buffer_bytes()
    );
    Ok(())
}
struct MemoryTarget {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
}
impl MemoryTarget {
    fn new(width: i32, height: i32) -> Result<Self, Failure> {
        let dc = unsafe { CreateCompatibleDC(None) };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap =
            unsafe { CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)? };
        let old = unsafe { SelectObject(dc, bitmap.into()) };
        Ok(Self { dc, bitmap, old })
    }
}
impl Drop for MemoryTarget {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            if !DeleteObject(self.bitmap.into()).as_bool() || !DeleteDC(self.dc).as_bool() {
                eprintln!("the bench target was not freed");
            }
        }
    }
}
fn shapes() -> Result<(), Failure> {
    for (dpi, width, height) in [(96u32, 1920u32, 1080u32), (192, 3840, 2160)] {
        let mut renderer = BitmapRenderer::new(width, height)?;
        renderer.set_dpi(dpi)?;
        let s = dpi as f32 / 96.0;
        let color = Color::rgb(40, 40, 40);
        for (name, command) in [
            (
                "160x32 tab",
                DrawCommand::RoundedRect {
                    rect: rect(8.0, 8.0, 160.0, 32.0),
                    radius: Dip(8.0),
                    color,
                },
            ),
            (
                "800x600 rounded rectangle",
                DrawCommand::RoundedRect {
                    rect: rect(10.0, 10.0, 800.0, 600.0),
                    radius: Dip(8.0),
                    color,
                },
            ),
            (
                "window-sized stroke",
                DrawCommand::StrokeRect {
                    rect: rect(0.0, 0.0, width as f32 / s, height as f32 / s),
                    radius: Dip(8.0),
                    width: Dip(1.0),
                    color,
                },
            ),
        ] {
            let list = DisplayList {
                commands: vec![command],
            };
            let ms = median(|| frame_all(&mut renderer, &list))?;
            println!(
                "measured on this PC: {name} at {}%: {ms:.3} ms",
                dpi * 100 / 96
            );
        }
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("bench failed: {}", e.0);
        std::process::exit(1);
    }
}
fn cold_frame() -> Result<(), Failure> {
    let list = glyph_frame(1920.0, 1080.0, DARK)?;
    let start = Instant::now();
    let mut renderer = BitmapRenderer::new(1920, 1080)?;
    renderer.register_font(FONT, "Cascadia Mono")?;
    let made = start.elapsed().as_secs_f64() * 1000.0;
    frame(&mut renderer, &list)?;
    let first = start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "measured on this PC: cold 1920x1080 dark in one process: renderer {made:.3} ms, with the first GlyphRun frame {first:.3} ms"
    );
    Ok(())
}
fn run() -> Result<(), Failure> {
    cold_frame()?;
    for (theme, name) in [(LIGHT, "light"), (DARK, "dark")] {
        window_frames(1920, 1080, 96, theme, name)?;
        window_frames(1920, 1080, 144, theme, name)?;
        window_frames(3840, 2160, 192, theme, name)?;
    }
    shapes()?;
    let mut renderer = BitmapRenderer::new(1920, 1080)?;
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, 1920.0, 1080.0),
        color: Color::rgb(255, 255, 255),
    });
    list.push(DrawCommand::FillRects {
        rects: (0..5000)
            .map(|i| rect((i % 100) as f32, ((i / 100) * 2) as f32, 1.0, 1.0))
            .collect(),
        color: Color::rgb(0, 0, 0),
    });
    let elapsed = median(|| frame_all(&mut renderer, &list))?;
    println!("measured on this PC: document map FillRects 5000: {elapsed:.3} ms");
    Ok(())
}
