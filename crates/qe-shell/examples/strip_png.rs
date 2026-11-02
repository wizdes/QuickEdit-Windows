//! Draw the tab strip without creating a window: Direct2D on a WIC bitmap.
use qe_api::{Dip, DisplayList, DrawCommand, Theme, ThemeKind, View};
use qe_shell::{ShellTokens, Tab, TabStrip};
use std::path::Path;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::core::PCWSTR;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn color(c: qe_api::Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: c.a as f32 / 255.0,
    }
}
fn rect(b: qe_api::Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: b.x.0,
        top: b.y.0,
        right: b.x.0 + b.width.0,
        bottom: b.y.0 + b.height.0,
    }
}
unsafe fn draw(rt: &ID2D1RenderTarget, list: &DisplayList, dark: bool) -> Result<(), String> {
    let theme = Theme::new(if dark {
        ThemeKind::Dark
    } else {
        ThemeKind::Light
    });
    let tokens = ShellTokens::from_theme(&theme);
    let dw: IDWriteFactory =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.map_err(err)?;
    let mut clips = 0usize;
    unsafe {
        rt.BeginDraw();
        rt.Clear(Some(&color(theme.text_background)));
    }
    for command in &list.commands {
        unsafe {
            match command {
                DrawCommand::FillRect { rect: b, color: c } => {
                    let brush = rt.CreateSolidColorBrush(&color(*c), None).map_err(err)?;
                    rt.FillRectangle(&rect(*b), &brush);
                }
                DrawCommand::RoundedRect {
                    rect: b,
                    radius,
                    color: c,
                } => {
                    let brush = rt.CreateSolidColorBrush(&color(*c), None).map_err(err)?;
                    rt.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: rect(*b),
                            radiusX: radius.0,
                            radiusY: radius.0,
                        },
                        &brush,
                    );
                }
                DrawCommand::StrokeRect {
                    rect: b,
                    radius,
                    width,
                    color: c,
                } => {
                    let brush = rt.CreateSolidColorBrush(&color(*c), None).map_err(err)?;
                    rt.DrawRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: rect(*b),
                            radiusX: radius.0,
                            radiusY: radius.0,
                        },
                        &brush,
                        width.0,
                        None,
                    );
                }
                DrawCommand::Line {
                    from,
                    to,
                    width,
                    color: c,
                } => {
                    let brush = rt.CreateSolidColorBrush(&color(*c), None).map_err(err)?;
                    rt.FillRectangle(
                        &D2D_RECT_F {
                            left: from.x.0.min(to.x.0),
                            top: from.y.0.min(to.y.0),
                            right: from.x.0.max(to.x.0) + width.0,
                            bottom: from.y.0.max(to.y.0) + width.0,
                        },
                        &brush,
                    );
                }
                DrawCommand::TextRun {
                    at,
                    text,
                    font,
                    color: c,
                } => {
                    let family = wide(if font == tokens.caption_font {
                        tokens.icon_font
                    } else {
                        font
                    });
                    let locale = wide("en-US");
                    let size = if font == tokens.caption_font {
                        tokens.caption_icon
                    } else if font == tokens.icon_font {
                        tokens.icon_font_size
                    } else {
                        tokens.text_font_size
                    };
                    let format = dw
                        .CreateTextFormat(
                            PCWSTR(family.as_ptr()),
                            None,
                            DWRITE_FONT_WEIGHT_NORMAL,
                            DWRITE_FONT_STYLE_NORMAL,
                            DWRITE_FONT_STRETCH_NORMAL,
                            size,
                            PCWSTR(locale.as_ptr()),
                        )
                        .map_err(err)?;
                    let brush = rt.CreateSolidColorBrush(&color(*c), None).map_err(err)?;
                    let utf: Vec<u16> = text.encode_utf16().collect();
                    rt.DrawText(
                        &utf,
                        &format,
                        &D2D_RECT_F {
                            left: at.x.0,
                            top: at.y.0,
                            right: rt.GetSize().width,
                            bottom: rt.GetSize().height,
                        },
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                DrawCommand::PushClip(b) => {
                    rt.PushAxisAlignedClip(&rect(*b), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    clips += 1;
                }
                DrawCommand::PopClip if clips > 0 => {
                    rt.PopAxisAlignedClip();
                    clips -= 1;
                }
                _ => {}
            }
        }
    }
    while clips > 0 {
        unsafe {
            rt.PopAxisAlignedClip();
        }
        clips -= 1;
    }
    unsafe { rt.EndDraw(None, None) }.map_err(err)
}
fn render(path: &Path, dark: bool) -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(err)?;
        let result = (|| {
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(err)?;
            let bitmap = wic
                .CreateBitmap(
                    900,
                    80,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapCacheOnLoad,
                )
                .map_err(err)?;
            let factory: ID2D1Factory =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).map_err(err)?;
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
            let rt = factory
                .CreateWicBitmapRenderTarget(&bitmap, &props)
                .map_err(err)?;
            let mut strip = TabStrip::new(vec![
                Tab::new("Untitled"),
                Tab::new("Example.rs"),
                Tab::new("A longer tab name"),
            ]);
            strip.tabs_mut()[1].dirty = true;
            let measure = qe_api::testing::FixedTextMeasurer {
                char_width: Dip(7.0),
                height: Dip(14.0),
                baseline: Dip(11.0),
            };
            strip.layout(
                qe_api::Rect {
                    x: Dip(0.0),
                    y: Dip(0.0),
                    width: Dip(900.0),
                    height: Dip(40.0),
                },
                &measure,
            );
            let mut list = DisplayList::default();
            strip.draw(
                &mut list,
                &Theme::new(if dark {
                    ThemeKind::Dark
                } else {
                    ThemeKind::Light
                }),
                &measure,
            );
            draw(&rt, &list, dark)?;
            let mut pixels = vec![0u8; 900 * 80 * 4];
            bitmap
                .CopyPixels(std::ptr::null(), 900 * 4, &mut pixels)
                .map_err(err)?;
            let stream = wic.CreateStream().map_err(err)?;
            stream
                .InitializeFromFilename(PCWSTR(wide(&path.to_string_lossy()).as_ptr()), 0x40000000)
                .map_err(err)?;
            let encoder = wic
                .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())
                .map_err(err)?;
            encoder
                .Initialize(&stream, WICBitmapEncoderNoCache)
                .map_err(err)?;
            let (mut frame, mut options) = (None, None);
            encoder
                .CreateNewFrame(&mut frame, &mut options)
                .map_err(err)?;
            let frame = frame.ok_or("WIC returned no PNG frame")?;
            frame.Initialize(options.as_ref()).map_err(err)?;
            frame.SetSize(900, 80).map_err(err)?;
            let mut format = GUID_WICPixelFormat32bppBGRA;
            frame.SetPixelFormat(&mut format).map_err(err)?;
            if format != GUID_WICPixelFormat32bppBGRA {
                return Err("WIC changed PNG pixel format".into());
            }
            frame.WritePixels(80, 900 * 4, &pixels).map_err(err)?;
            frame.Commit().map_err(err)?;
            encoder.Commit().map_err(err)?;
            Ok(())
        })();
        CoUninitialize();
        result
    }
}
fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).ok_or("usage: strip_png PATH [dark]")?;
    render(Path::new(path), args.get(2).is_some_and(|s| s == "dark"))?;
    println!("wrote {path} with Direct2D on WIC");
    Ok(())
}
