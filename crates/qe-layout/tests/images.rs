use qe_api::{Dip, Layout, LineIndex, testing::StringStore};
use qe_layout::{DirectWriteLayout, font_face};
use std::{mem::ManuallyDrop, path::Path};
use windows::{
    Win32::{
        Foundation::COLORREF,
        Graphics::{DirectWrite::*, Gdi::*, Imaging::*},
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
    },
    core::{BOOL, PCWSTR},
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn fixed_rendering_params(factory: &IDWriteFactory) -> IDWriteRenderingParams {
    unsafe {
        factory
            .CreateCustomRenderingParams(
                2.2,
                1.0,
                0.0,
                DWRITE_PIXEL_GEOMETRY_FLAT,
                DWRITE_RENDERING_MODE_NATURAL,
            )
            .unwrap()
    }
}
#[test]
fn saved_image_rendering_params_are_fixed() {
    let factory: IDWriteFactory =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
    let params = fixed_rendering_params(&factory);
    assert_eq!(unsafe { params.GetGamma() }, 2.2);
    assert_eq!(unsafe { params.GetEnhancedContrast() }, 1.0);
    assert_eq!(unsafe { params.GetClearTypeLevel() }, 0.0);
    assert_eq!(
        unsafe { params.GetPixelGeometry() },
        DWRITE_PIXEL_GEOMETRY_FLAT
    );
    assert_eq!(
        unsafe { params.GetRenderingMode() },
        DWRITE_RENDERING_MODE_NATURAL
    );
}
#[test]
fn arabic_shaping_uses_the_named_font_after_another_font_is_loaded() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).unwrap() };
    let sample = "مرحبا بالعالم";
    let shape = || {
        let engine = DirectWriteLayout::with_font("Segoe UI", 10.0, DWRITE_FONT_WEIGHT_NORMAL)
            .unwrap()
            .with_shaping(true);
        let layout = engine
            .layout_line(&StringStore::new(sample), LineIndex(0), Dip(400.0))
            .unwrap();
        layout.rows[0]
            .glyph_runs
            .iter()
            .map(|run| {
                (
                    run.glyph_ids.clone(),
                    run.advances
                        .iter()
                        .map(|advance| advance.0.to_bits())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    let expected = shape();
    assert_eq!(
        expected,
        vec![(
            vec![
                2287, 2828, 2256, 3623, 2247, 3, 2251, 2247, 2286, 2275, 2247, 2286, 3671
            ],
            vec![
                1090098517, 1085415424, 1090084864, 1083080704, 1079361536, 1080672256, 1083080704,
                1079361536, 1083080704, 1090084864, 1079361536, 1083080704, 1090084864,
            ],
        )]
    );
    let _other_font = DirectWriteLayout::new().unwrap();
    assert_eq!(shape(), expected);
    let explicit_pixels = pixels(sample, "Segoe UI");
    assert_eq!(explicit_pixels.len(), 400 * 80 * 4);
    unsafe { CoUninitialize() };
}
fn pixels(sample: &str, family: &str) -> Vec<u8> {
    let engine = DirectWriteLayout::with_font(family, 10.0, DWRITE_FONT_WEIGHT_NORMAL)
        .unwrap()
        .with_shaping(true);
    let store = StringStore::new(sample);
    let layout = engine
        .layout_line(&store, LineIndex(0), Dip(400.0))
        .unwrap();
    let factory: IDWriteFactory =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
    let target = unsafe {
        factory
            .GetGdiInterop()
            .unwrap()
            .CreateBitmapRenderTarget(None, 400, 80)
            .unwrap()
    };
    unsafe {
        target.SetPixelsPerDip(1.0).unwrap(); // fixed 96 DPI
        assert!(PatBlt(target.GetMemoryDC(), 0, 0, 400, 80, WHITENESS).as_bool());
    }
    let params = fixed_rendering_params(&factory);
    for run in layout.rows[0].glyph_runs.iter() {
        let face = font_face(run.font_id).unwrap();
        let advances: Vec<f32> = run.advances.iter().map(|a| a.0).collect();
        let offsets: Vec<DWRITE_GLYPH_OFFSET> = run
            .offsets
            .iter()
            .map(|p| DWRITE_GLYPH_OFFSET {
                advanceOffset: p.x.0,
                ascenderOffset: p.y.0,
            })
            .collect();
        let native = DWRITE_GLYPH_RUN {
            fontFace: ManuallyDrop::new(Some(face)),
            fontEmSize: 10.0 * 96.0 / 72.0,
            glyphCount: run.glyph_ids.len() as u32,
            glyphIndices: run.glyph_ids.as_ptr(),
            glyphAdvances: advances.as_ptr(),
            glyphOffsets: offsets.as_ptr(),
            isSideways: BOOL(0),
            bidiLevel: run.bidi_level as u32,
        };
        unsafe {
            target
                .DrawGlyphRun(
                    run.origin.x.0 + 12.0,
                    run.origin.y.0 + 28.0,
                    DWRITE_MEASURING_MODE_NATURAL,
                    &native,
                    &params,
                    COLORREF(0),
                    None,
                )
                .unwrap();
        }
    }
    let bitmap = unsafe { GetCurrentObject(target.GetMemoryDC(), OBJ_BITMAP) };
    let wic: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap() };
    let source = unsafe {
        wic.CreateBitmapFromHBITMAP(HBITMAP(bitmap.0), HPALETTE::default(), WICBitmapIgnoreAlpha)
            .unwrap()
    };
    let mut pixels = vec![0u8; 400 * 80 * 4];
    unsafe {
        source
            .CopyPixels(std::ptr::null(), 400 * 4, &mut pixels)
            .unwrap();
    }
    pixels
}
fn save_png(path: &Path, pixels: &[u8]) {
    let wic: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap() };
    let stream = unsafe { wic.CreateStream().unwrap() };
    unsafe {
        stream
            .InitializeFromFilename(PCWSTR(wide(&path.to_string_lossy()).as_ptr()), 0x40000000)
            .unwrap();
    }
    let enc = unsafe {
        wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())
            .unwrap()
    };
    unsafe {
        enc.Initialize(&stream, WICBitmapEncoderNoCache).unwrap();
    }
    let (mut frame, mut options) = (None, None);
    unsafe {
        enc.CreateNewFrame(&mut frame, &mut options).unwrap();
    }
    let frame = frame.unwrap();
    unsafe {
        frame.Initialize(options.as_ref()).unwrap();
        frame.SetSize(400, 80).unwrap();
    }
    let mut format = GUID_WICPixelFormat32bppBGRA;
    unsafe {
        frame.SetPixelFormat(&mut format).unwrap();
        frame.WritePixels(80, 400 * 4, pixels).unwrap();
        frame.Commit().unwrap();
        enc.Commit().unwrap();
    }
}
fn load_png(path: &Path) -> Vec<u8> {
    let wic: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap() };
    let decoder = unsafe {
        wic.CreateDecoderFromFilename(
            PCWSTR(wide(&path.to_string_lossy()).as_ptr()),
            None,
            windows::Win32::Foundation::GENERIC_READ,
            WICDecodeMetadataCacheOnLoad,
        )
        .unwrap()
    };
    let frame = unsafe { decoder.GetFrame(0).unwrap() };
    let mut pixels = vec![0u8; 400 * 80 * 4];
    unsafe {
        frame
            .CopyPixels(std::ptr::null(), 400 * 4, &mut pixels)
            .unwrap();
    }
    pixels
}
#[test]
fn arabic_and_emoji_match_saved_pngs() {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).unwrap();
    }
    // System font fallback differs per user, so each saved image names its font.
    for (name, sample, family) in [
        ("arabic", "مرحبا بالعالم", "Segoe UI"),
        ("emoji", "👩‍💻 😀 👨‍👩‍👧‍👦", "Segoe UI Emoji"),
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/images")
            .join(format!("{name}.png"));
        let actual = pixels(sample, family);
        if std::env::var_os("QE_LAYOUT_WRITE_IMAGES").is_some() {
            save_png(&path, &actual);
        }
        let expected = load_png(&path);
        let differing = actual
            .iter()
            .zip(expected.iter())
            .filter(|(a, b)| a.abs_diff(**b) > 3)
            .count();
        assert!(
            differing <= 20,
            "{name} has {differing} different channels: {}",
            path.display()
        );
    }
    unsafe {
        CoUninitialize();
    }
}
