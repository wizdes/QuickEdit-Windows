use std::{fs, os::windows::io::AsRawHandle, path::Path, time::Instant};
use windows::{
    Win32::{
        Foundation::*,
        Globalization::*,
        Graphics::{Direct2D::Common::*, Direct2D::*, Direct3D::*, Direct3D11::*, DirectWrite::*, Dxgi::Common::*, Dxgi::*, Gdi::*, Imaging::*},
        System::{Com::*, Memory::*, RemoteDesktop::*, StationsAndDesktops::*, Threading::*},
        UI::{Accessibility::*, TextServices::*, WindowsAndMessaging::*},
    },
    core::{BOOL, PCWSTR},
};

type P = Result<String, String>;
fn e(v: impl std::fmt::Display) -> String {
    v.to_string()
}
fn take<T>(v: Option<T>) -> Result<T, String> {
    v.ok_or_else(|| "null output".into())
}
fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn run(name: &str, f: impl FnOnce() -> P) {
    let t = Instant::now();
    match f() {
        Ok(s) => println!("{name} PASS {:.2} ms {s}", t.elapsed().as_secs_f64() * 1000.0),
        Err(s) => println!("{name} FAIL {:.2} ms {s}", t.elapsed().as_secs_f64() * 1000.0),
    }
}
unsafe fn device(driver: D3D_DRIVER_TYPE) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let (mut d, mut c) = (None, None);
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut d),
            None,
            Some(&mut c),
        )
    }
    .map_err(e)?;
    Ok((take(d)?, take(c)?))
}
unsafe fn layout() -> Result<(IDWriteFactory, IDWriteTextLayout), String> {
    let f: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.map_err(e)?;
    let fmt = unsafe {
        f.CreateTextFormat(
            PCWSTR(w("Segoe UI").as_ptr()),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            24.0,
            PCWSTR(w("en-US").as_ptr()),
        )
    }
    .map_err(e)?;
    let text: Vec<u16> = "Latin مرحبا 😀".encode_utf16().collect();
    let l = unsafe { f.CreateTextLayout(&text, &fmt, 500.0, 80.0) }.map_err(e)?;
    let mut n = 0;
    let mut clusters = vec![DWRITE_CLUSTER_METRICS::default(); text.len()];
    unsafe { l.GetClusterMetrics(Some(&mut clusters), &mut n) }.map_err(e)?;
    if n == 0 {
        return Err("no shaped clusters".into());
    }
    Ok((f, l))
}
unsafe fn wic() -> Result<IWICImagingFactory, String> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.map_err(e)
}
unsafe fn png(path: &Path, pixels: &[u8], width: u32, height: u32) -> P {
    let f = unsafe { wic()? };
    let stream = unsafe { f.CreateStream() }.map_err(e)?;
    unsafe { stream.InitializeFromFilename(PCWSTR(w(&path.to_string_lossy()).as_ptr()), 0x40000000) }.map_err(e)?;
    let enc = unsafe { f.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()) }.map_err(e)?;
    unsafe { enc.Initialize(&stream, WICBitmapEncoderNoCache) }.map_err(e)?;
    let (mut frame, mut options) = (None, None);
    unsafe { enc.CreateNewFrame(&mut frame, &mut options) }.map_err(e)?;
    let frame = take(frame)?;
    unsafe { frame.Initialize(options.as_ref()) }.map_err(e)?;
    unsafe { frame.SetSize(width, height) }.map_err(e)?;
    let mut format = GUID_WICPixelFormat32bppBGRA;
    unsafe { frame.SetPixelFormat(&mut format) }.map_err(e)?;
    if format != GUID_WICPixelFormat32bppBGRA {
        return Err("WIC changed pixel format".into());
    }
    unsafe { frame.WritePixels(height, width * 4, pixels) }.map_err(e)?;
    unsafe { frame.Commit() }.map_err(e)?;
    unsafe { enc.Commit() }.map_err(e)?;
    Ok(path.display().to_string())
}
fn dwrite_png(out: &Path) -> P {
    unsafe {
        let (f, _) = layout()?;
        let rt = f.GetGdiInterop().map_err(e)?.CreateBitmapRenderTarget(None, 360, 80).map_err(e)?;
        if !PatBlt(rt.GetMemoryDC(), 0, 0, 360, 80, WHITENESS).as_bool() {
            return Err(format!("PatBlt error {:?}", GetLastError()));
        }
        let mut collection = None;
        f.GetSystemFontCollection(&mut collection, false).map_err(e)?;
        let collection = take(collection)?;
        let (mut index, mut exists) = (0, BOOL(0));
        collection.FindFamilyName(PCWSTR(w("Segoe UI").as_ptr()), &mut index, &mut exists).map_err(e)?;
        if !exists.as_bool() {
            return Err("Segoe UI font unavailable".into());
        }
        let font = collection
            .GetFontFamily(index)
            .map_err(e)?
            .GetFirstMatchingFont(DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL)
            .map_err(e)?;
        let face = font.CreateFontFace().map_err(e)?;
        let points: Vec<u32> = "Probe text".chars().map(u32::from).collect();
        let mut glyphs = vec![0u16; points.len()];
        face.GetGlyphIndices(points.as_ptr(), points.len() as u32, glyphs.as_mut_ptr()).map_err(e)?;
        let advances = vec![18.0f32; glyphs.len()];
        let gr = DWRITE_GLYPH_RUN {
            fontFace: std::mem::ManuallyDrop::new(Some(face)),
            fontEmSize: 24.0,
            glyphCount: glyphs.len() as u32,
            glyphIndices: glyphs.as_ptr(),
            glyphAdvances: advances.as_ptr(),
            glyphOffsets: std::ptr::null(),
            isSideways: BOOL(0),
            bidiLevel: 0,
        };
        let params = f.CreateRenderingParams().map_err(e)?;
        rt.DrawGlyphRun(12.0, 42.0, DWRITE_MEASURING_MODE_NATURAL, &gr, &params, COLORREF(0), None)
            .map_err(|x| format!("DrawGlyphRun: {x}"))?;
        let obj = GetCurrentObject(rt.GetMemoryDC(), OBJ_BITMAP);
        if obj.0.is_null() {
            return Err("no bitmap".into());
        }
        let bitmap = wic()?
            .CreateBitmapFromHBITMAP(HBITMAP(obj.0), HPALETTE::default(), WICBitmapIgnoreAlpha)
            .map_err(|x| format!("CreateBitmapFromHBITMAP: {x}"))?;
        let mut pixels = vec![0; 360 * 80 * 4];
        bitmap.CopyPixels(std::ptr::null(), 360 * 4, &mut pixels).map_err(|x| format!("CopyPixels: {x}"))?;
        png(&out.join("directwrite.png"), &pixels, 360, 80)
    }
}
fn d2d_png(out: &Path) -> P {
    unsafe {
        let (dw, _) = layout()?;
        let bitmap = wic()?.CreateBitmap(500, 80, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad).map_err(e)?;
        let f: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).map_err(e)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let rt = f.CreateWicBitmapRenderTarget(&bitmap, &props).map_err(e)?;
        let black = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
        let white = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
        let brush = rt.CreateSolidColorBrush(&black, None).map_err(e)?;
        rt.BeginDraw();
        rt.Clear(Some(&white));
        let fmt = dw
            .CreateTextFormat(
                PCWSTR(w("Segoe UI").as_ptr()),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                24.0,
                PCWSTR(w("en-US").as_ptr()),
            )
            .map_err(e)?;
        let text: Vec<u16> = "Latin مرحبا 😀".encode_utf16().collect();
        rt.DrawText(
            &text,
            &fmt,
            &D2D_RECT_F {
                left: 10.0,
                top: 10.0,
                right: 490.0,
                bottom: 70.0,
            },
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
        rt.EndDraw(None, None).map_err(e)?;
        let mut pixels = vec![0; 500 * 80 * 4];
        bitmap.CopyPixels(std::ptr::null(), 500 * 4, &mut pixels).map_err(e)?;
        png(&out.join("direct2d.png"), &pixels, 500, 80)
    }
}
fn readback() -> P {
    unsafe {
        let (d, c) = device(D3D_DRIVER_TYPE_WARP)?;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: 4,
            Height: 4,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex = None;
        d.CreateTexture2D(&desc, None, Some(&mut tex)).map_err(e)?;
        let tex = take(tex)?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        c.Map(&tex, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(e)?;
        let ok = !mapped.pData.is_null() && mapped.RowPitch >= 16;
        c.Unmap(&tex, 0);
        if !ok {
            return Err("invalid mapped texture".into());
        }
        Ok(format!("row_pitch={}", mapped.RowPitch))
    }
}
fn spelling() -> P {
    unsafe {
        let f: ISpellCheckerFactory = CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER).map_err(e)?;
        let checker = f.CreateSpellChecker(PCWSTR(w("en-US").as_ptr())).map_err(e)?;
        let errors = checker.Check(PCWSTR(w("mispellled").as_ptr())).map_err(e)?;
        let mut first = None;
        errors.Next(&mut first).ok().map_err(e)?;
        if first.is_none() {
            return Err("misspelled word was not flagged".into());
        }
        Ok(String::new())
    }
}
fn window() -> P {
    unsafe {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            windows::core::w!("STATIC"),
            windows::core::w!("probe"),
            WS_OVERLAPPED,
            0,
            0,
            1,
            1,
            None,
            None,
            None,
            None,
        )
        .map_err(e)?;
        let result = (|| {
            PostMessageW(Some(hwnd), WM_APP + 1, WPARAM(7), LPARAM(0)).map_err(e)?;
            let mut msg = MSG::default();
            if !PeekMessageW(&mut msg, Some(hwnd), WM_APP + 1, WM_APP + 1, PM_REMOVE).as_bool() || msg.wParam.0 != 7 {
                return Err("posted message missing".into());
            }
            Ok(String::new())
        })();
        let _ = DestroyWindow(hwnd);
        result
    }
}
fn desktop() -> P {
    unsafe {
        let desk = CreateDesktopW(
            PCWSTR(w("spike-probe-desktop").as_ptr()),
            PCWSTR::null(),
            None,
            DESKTOP_CONTROL_FLAGS(0),
            DESKTOP_CREATEWINDOW.0 | DESKTOP_READOBJECTS.0 | DESKTOP_WRITEOBJECTS.0,
            None,
        )
        .map_err(e)?;
        let desk_value = desk.0 as usize;
        let result = std::thread::spawn(move || {
            SetThreadDesktop(HDESK(desk_value as *mut _)).map_err(e)?;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                windows::core::w!("STATIC"),
                windows::core::w!("desk"),
                WS_OVERLAPPED,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )
            .map_err(e)?;
            DestroyWindow(hwnd).map_err(e)?;
            Ok(String::new())
        })
        .join()
        .map_err(|_| "desktop thread panicked".to_string())?;
        CloseDesktop(desk).map_err(e)?;
        result
    }
}
fn mapping() -> P {
    let path = std::env::temp_dir().join(format!("spike-probe-{}.bin", std::process::id()));
    fs::write(&path, b"probe-mapped-file").map_err(e)?;
    let result = (|| unsafe {
        let file = fs::File::open(&path).map_err(e)?;
        let map = CreateFileMappingW(HANDLE(file.as_raw_handle()), None, PAGE_READONLY, 0, 0, PCWSTR::null()).map_err(e)?;
        let view = MapViewOfFile(map, FILE_MAP_READ, 0, 0, 17);
        if view.Value.is_null() {
            let code = GetLastError();
            let _ = CloseHandle(map);
            return Err(format!("MapViewOfFile error {code:?}"));
        }
        let same = std::slice::from_raw_parts(view.Value as *const u8, 17) == b"probe-mapped-file";
        UnmapViewOfFile(view).map_err(e)?;
        CloseHandle(map).map_err(e)?;
        if !same {
            return Err("mapped bytes differ".into());
        }
        Ok(String::new())
    })();
    let _ = fs::remove_file(path);
    result
}
fn session() -> P {
    unsafe {
        let mut id = 0;
        ProcessIdToSessionId(GetCurrentProcessId(), &mut id).map_err(e)?;
        let station = GetProcessWindowStation().map_err(e)?;
        let mut flags = USEROBJECTFLAGS::default();
        GetUserObjectInformationW(
            HANDLE(station.0),
            UOI_FLAGS,
            Some(&mut flags as *mut _ as *mut _),
            std::mem::size_of::<USEROBJECTFLAGS>() as u32,
            None,
        )
        .map_err(e)?;
        Ok(format!("session={id} interactive={}", flags.dwFlags & WSF_VISIBLE as u32 != 0))
    }
}
fn chain() -> P {
    unsafe {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            windows::core::w!("STATIC"),
            windows::core::w!("swap"),
            WS_OVERLAPPED,
            0,
            0,
            16,
            16,
            None,
            None,
            None,
            None,
        )
        .map_err(e)?;
        let desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: 16,
                Height: 16,
                RefreshRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                ScanlineOrdering: DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED,
                Scaling: DXGI_MODE_SCALING_UNSPECIFIED,
            },
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: hwnd,
            Windowed: BOOL(1),
            SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
            Flags: 0,
        };
        let mut swap = None;
        let result = D3D11CreateDeviceAndSwapChain(
            None,
            D3D_DRIVER_TYPE_WARP,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&desc),
            Some(&mut swap),
            None,
            None,
            None,
        )
        .map_err(e)
        .and_then(|_| take(swap).map(|_| String::new()));
        let _ = DestroyWindow(hwnd);
        result
    }
}
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let out = if args.len() == 3 && args[1] == "--out" {
        args[2].clone().into()
    } else {
        std::path::PathBuf::from("target/probe-out")
    };
    let out_error = fs::create_dir_all(&out).err().map(e);
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().map_err(e);
    run("D3D11_HARDWARE", || {
        unsafe {
            device(D3D_DRIVER_TYPE_HARDWARE)?;
        }
        Ok(String::new())
    });
    run("D3D11_WARP", || {
        unsafe {
            device(D3D_DRIVER_TYPE_WARP)?;
        }
        Ok(String::new())
    });
    run("DIRECTWRITE_LAYOUT_SHAPING", || {
        let (_, l) = unsafe { layout()? };
        let mut n = 0;
        let mut clusters = vec![DWRITE_CLUSTER_METRICS::default(); 64];
        unsafe { l.GetClusterMetrics(Some(&mut clusters), &mut n) }.map_err(e)?;
        Ok(format!("clusters={n}"))
    });
    run("DIRECTWRITE_BITMAP_PNG", || out_error.as_ref().map_or_else(|| dwrite_png(&out), |x| Err(x.clone())));
    run("DIRECT2D_WIC_PNG", || out_error.as_ref().map_or_else(|| d2d_png(&out), |x| Err(x.clone())));
    run("D3D11_WARP_READBACK", readback);
    run("SPELLCHECK_EN_US", || com.as_ref().map_or_else(|x| Err(x.clone()), |_| spelling()));
    run("HIDDEN_HWND_MESSAGE", window);
    run("DESKTOP_THREAD_WINDOW", desktop);
    run("UI_AUTOMATION", || {
        com.as_ref().map_or_else(
            |x| Err(x.clone()),
            |_| {
                let _: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }.map_err(e)?;
                Ok(String::new())
            },
        )
    });
    run("TSF_THREAD_MANAGER", || {
        com.as_ref().map_or_else(
            |x| Err(x.clone()),
            |_| {
                let _: ITfThreadMgr = unsafe { CoCreateInstance(&CLSID_TF_ThreadMgr, None, CLSCTX_INPROC_SERVER) }.map_err(e)?;
                Ok(String::new())
            },
        )
    });
    run("MEMORY_MAPPED_FILE", mapping);
    run("SESSION", session);
    run("HIDDEN_SWAP_CHAIN", chain);
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn mixed_text_layout_has_shaped_clusters() {
        let (_, layout) = unsafe { super::layout() }.expect("mixed text layout");
        let mut count = 0;
        let mut clusters = vec![windows::Win32::Graphics::DirectWrite::DWRITE_CLUSTER_METRICS::default(); 64];
        unsafe { layout.GetClusterMetrics(Some(&mut clusters), &mut count) }.expect("cluster data");
        assert!(count > 0);
    }
    #[test]
    fn directwrite_bitmap_writes_png() {
        let com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED) };
        assert!(com.is_ok());
        let out = std::env::temp_dir().join(format!("spike-probe-test-{}", std::process::id()));
        std::fs::create_dir_all(&out).expect("output dir");
        let result = super::dwrite_png(&out);
        let png = out.join("directwrite.png");
        let bytes = std::fs::read(&png);
        let distinct_pixels = (|| unsafe {
            let decoder = super::wic()?
                .CreateDecoderFromFilename(
                    windows::core::PCWSTR(super::w(&png.to_string_lossy()).as_ptr()),
                    None,
                    windows::Win32::Foundation::GENERIC_READ,
                    windows::Win32::Graphics::Imaging::WICDecodeMetadataCacheOnLoad,
                )
                .map_err(super::e)?;
            let frame = decoder.GetFrame(0).map_err(super::e)?;
            let mut pixels = vec![0; 360 * 80 * 4];
            frame.CopyPixels(std::ptr::null(), 360 * 4, &mut pixels).map_err(super::e)?;
            Ok::<_, String>(pixels.chunks_exact(4).any(|p| p != &pixels[..4]))
        })();
        let _ = std::fs::remove_file(&png);
        let _ = std::fs::remove_dir(&out);
        unsafe { windows::Win32::System::Com::CoUninitialize() };
        result.expect("DirectWrite render");
        assert!(bytes.expect("PNG file").starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(distinct_pixels.expect("decoded pixels"), "picture has no visible text");
    }
}
