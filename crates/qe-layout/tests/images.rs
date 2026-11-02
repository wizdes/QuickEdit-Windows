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
fn pixels(sample: &str) -> Vec<u8> {
    let engine = DirectWriteLayout::new().unwrap().with_shaping(true);
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
    for (name, sample) in [("arabic", "مرحبا بالعالم"), ("emoji", "👩‍💻 😀 👨‍👩‍👧‍👦")]
    {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/images")
            .join(format!("{name}.png"));
        let actual = pixels(sample);
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
