//! Text weight against what Windows itself draws for the same font, size and system settings.
//!
//! What the measuring found, on Windows build 26300, Cascadia Mono at 10 points and 100%:
//! our text is DirectWrite's bitmap target to within 0.1%, and it has 2.5% to 3.2% less ink
//! than Direct2D. So the blend adds no weight. Run with `--nocapture` to read the numbers.
mod support;
use qe_api::{Bitmap, Color, DisplayList};
use qe_render::{BitmapRenderer, TextSettings};
use std::mem::ManuallyDrop;
use support::*;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::core::BOOL;

const TEXT: &str = "The quick brown fox jumps over the lazy dog.";
const WIDTH: u32 = 420;
const HEIGHT: u32 = 30;
const POINTS_10: f32 = 10.0 * 96.0 / 72.0;
const AT: (f32, f32) = (10.0, 20.0);
const DARK: (Color, Color) = (Color::rgb(39, 39, 39), Color::rgb(255, 255, 255));
const LIGHT: (Color, Color) = (WHITE, BLACK);

/// The system's own settings, read here and not through the renderer.
fn system() -> IDWriteRenderingParams {
    let factory: IDWriteFactory =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
    unsafe { factory.CreateRenderingParams().unwrap() }
}
fn settings_of(params: &IDWriteRenderingParams) -> TextSettings {
    unsafe {
        TextSettings {
            gamma: params.GetGamma(),
            enhanced_contrast: params.GetEnhancedContrast(),
            cleartype_level: params.GetClearTypeLevel(),
            geometry: params.GetPixelGeometry(),
            mode: params.GetRenderingMode(),
        }
    }
}
fn ours(settings: TextSettings, (bg, ink): (Color, Color)) -> Bitmap {
    let mut r = BitmapRenderer::new(WIDTH, HEIGHT).unwrap();
    r.set_text_settings(settings).unwrap();
    r.register_font(1, MONO).unwrap();
    let mut list = DisplayList::default();
    fill(&mut list, rect(0.0, 0.0, WIDTH as f32, HEIGHT as f32), bg);
    list.push(glyph_run(1, MONO, POINTS_10, TEXT, point(AT.0, AT.1), ink));
    frame(&mut r, &list);
    r.bitmap().unwrap()
}
fn bitmap_target(settings: TextSettings, (bg, ink): (Color, Color)) -> Bitmap {
    let (ids, advances) = glyphs(MONO, POINTS_10, TEXT);
    let offsets = vec![DWRITE_GLYPH_OFFSET::default(); ids.len()];
    let direct = Direct::new(WIDTH, HEIGHT, 96, settings, bg);
    direct.draw(
        &face(MONO),
        POINTS_10,
        &ids,
        &advances,
        &offsets,
        0,
        AT,
        ink,
    );
    direct.bitmap()
}
/// Direct2D's own text, on a memory bitmap with no window, with ClearType as on the screen.
fn direct2d(params: &IDWriteRenderingParams, (bg, ink): (Color, Color)) -> Bitmap {
    let color = |c: Color| D2D1_COLOR_F {
        r: f32::from(c.r) / 255.0,
        g: f32::from(c.g) / 255.0,
        b: f32::from(c.b) / 255.0,
        a: 1.0,
    };
    let (ids, advances) = glyphs(MONO, POINTS_10, TEXT);
    let offsets = vec![DWRITE_GLYPH_OFFSET::default(); ids.len()];
    unsafe {
        let factory: ID2D1Factory =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
        let target = factory
            .CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            })
            .unwrap();
        let dc = CreateCompatibleDC(None);
        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = WIDTH as i32;
        info.bmiHeader.biHeight = -(HEIGHT as i32);
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
        let old = SelectObject(dc, dib.into());
        target
            .BindDC(
                dc,
                &RECT {
                    left: 0,
                    top: 0,
                    right: WIDTH as i32,
                    bottom: HEIGHT as i32,
                },
            )
            .unwrap();
        target.BeginDraw();
        target.Clear(Some(&color(bg)));
        target.SetTextRenderingParams(params);
        target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
        let brush = target.CreateSolidColorBrush(&color(ink), None).unwrap();
        let mut run = DWRITE_GLYPH_RUN {
            fontFace: ManuallyDrop::new(Some(face(MONO))),
            fontEmSize: POINTS_10,
            glyphCount: ids.len() as u32,
            glyphIndices: ids.as_ptr(),
            glyphAdvances: advances.as_ptr(),
            glyphOffsets: offsets.as_ptr(),
            isSideways: BOOL(0),
            bidiLevel: 0,
        };
        // SAFETY: two f32 values are the layout of `windows_numerics::Vector2`, a crate this
        // package does not name.
        #[allow(clippy::missing_transmute_annotations)]
        let origin = std::mem::transmute::<[f32; 2], _>([AT.0, AT.1]);
        target.DrawGlyphRun(origin, &run, &brush, DWRITE_MEASURING_MODE_NATURAL);
        target.EndDraw(None, None).unwrap();
        ManuallyDrop::drop(&mut run.fontFace);
        let bgra =
            std::slice::from_raw_parts(bits as *const u8, (WIDTH * HEIGHT * 4) as usize).to_vec();
        SelectObject(dc, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(dc);
        Bitmap {
            width: WIDTH,
            height: HEIGHT,
            bgra,
        }
    }
}
/// Mean ink for each glyph, in full pixels of the ink's color.
fn ink_per_glyph(image: &Bitmap, (bg, ink): (Color, Color)) -> f64 {
    let span = f64::from(ink.g.abs_diff(bg.g)) * 3.0;
    ink_mass(image, bg) as f64 / span / TEXT.len() as f64
}

/// Ours against the two ways Windows draws: both with the system's real settings.
///
/// Limits. Against the bitmap target, whose blend ours copies, 1%: measured 0.1%. Against
/// Direct2D, which blends another way, light on dark may not pass 1.01 and dark on light may
/// not fall under 0.95. The ends are one-sided on purpose: light text must not grow past what
/// Windows draws, and dark text must not thin from it by more than what is measured here,
/// 3.2%, and a margin of under 2% for other PCs' settings.
#[test]
fn text_weighs_the_same_as_what_windows_draws_in_dark_and_light() {
    let params = system();
    let settings = settings_of(&params);
    for (name, colors, least, most) in [("dark", DARK, 0.94, 1.01), ("light", LIGHT, 0.95, 1.03)] {
        let mine = ink_per_glyph(&ours(settings, colors), colors);
        let bitmap = ink_per_glyph(&bitmap_target(settings, colors), colors);
        let d2d = ink_per_glyph(&direct2d(&params, colors), colors);
        println!(
            "{name}: ink per glyph {mine:.2}, bitmap target {bitmap:.2} (ratio {:.3}), Direct2D {d2d:.2} (ratio {:.3})",
            mine / bitmap,
            mine / d2d,
        );
        assert!(
            (mine / bitmap - 1.0).abs() <= 0.01,
            "{name}: {mine} against the bitmap target's {bitmap}"
        );
        assert!(
            (least..=most).contains(&(mine / d2d)),
            "{name}: {mine} against Direct2D's {d2d}"
        );
    }
}

/// A stem pixel by pixel. Subpixel placement can differ by a quarter of a pixel between the
/// two, so this compares only the straight stem of the `h`, where both agree on the place.
#[test]
fn a_stem_has_the_pixels_of_the_bitmap_target_within_rounding() {
    let params = system();
    let settings = settings_of(&params);
    for colors in [DARK, LIGHT] {
        let (mine, theirs) = (ours(settings, colors), bitmap_target(settings, colors));
        for x in 18..21 {
            let (a, b) = (pixel(&mine, x, 12), pixel(&theirs, x, 12));
            for c in 0..3 {
                assert!(a[c].abs_diff(b[c]) <= 2, "x {x}: ours {a:?}, theirs {b:?}");
            }
        }
    }
}

/// The app's renderer starts with the system's values, not the fixed ones for saved pictures.
#[test]
fn a_new_renderer_uses_the_systems_values() {
    let found = BitmapRenderer::new(1, 1).unwrap().text_settings();
    assert_eq!(found, settings_of(&system()));
}
