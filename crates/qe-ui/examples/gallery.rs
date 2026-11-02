use qe_api::{
    Color, Dip, DisplayList, DrawCommand, Rect, TextMeasurer, TextMetrics, Theme, ThemeKind, View,
    WidgetId,
};
use qe_ui::*;
use std::{env, fs, path::PathBuf};
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::*;
use windows::core::PCWSTR;

struct Measure;
impl TextMeasurer for Measure {
    fn measure(&self, s: &str, _: &str) -> TextMetrics {
        TextMetrics {
            width: Dip(s.chars().count() as f32 * 7.0),
            height: Dip(16.0),
            baseline: Dip(12.0),
        }
    }
}
fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(w),
        height: Dip(h),
    }
}
fn draw_view(v: &mut dyn View, list: &mut DisplayList, theme: &Theme, b: Rect) {
    v.layout(b, &Measure);
    v.draw(list, theme, &Measure)
}
fn gallery(theme: Theme) -> DisplayList {
    let t = UiTokens::from_theme(&theme);
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: r(0.0, 0.0, 900.0, 800.0),
        color: t.background,
    });
    let mut title = Label::new(WidgetId(1), "QuickEdit controls", LabelKind::Group);
    draw_view(&mut title, &mut list, &theme, r(25.0, 20.0, 300.0, 22.0));
    let mut button = Button::new(WidgetId(2), "Save");
    draw_view(&mut button, &mut list, &theme, r(25.0, 60.0, 120.0, 32.0));
    let mut default = Button::new(WidgetId(3), "Select");
    default.is_default = true;
    draw_view(&mut default, &mut list, &theme, r(155.0, 60.0, 120.0, 32.0));
    let mut icon = Button::new(WidgetId(4), "New");
    icon.icon = Some("E710".into());
    draw_view(&mut icon, &mut list, &theme, r(285.0, 60.0, 32.0, 32.0));
    let mut check = Checkbox::new(WidgetId(5), "Check spelling as you type");
    check.checked = true;
    draw_view(&mut check, &mut list, &theme, r(25.0, 110.0, 300.0, 32.0));
    let mut field = TextField::new(WidgetId(6), "Line number");
    field.placeholder = "Line number".into();
    draw_view(&mut field, &mut list, &theme, r(25.0, 155.0, 220.0, 32.0));
    let mut combo = ComboBox::new(
        WidgetId(7),
        "Encoding",
        vec![Some("UTF-8".into()), None, Some("UTF-16".into())],
    );
    draw_view(&mut combo, &mut list, &theme, r(255.0, 155.0, 180.0, 32.0));
    let mut tabs = TabBar::new(
        WidgetId(8),
        vec![
            "New Document".into(),
            "Open and Save".into(),
            "Keyboard".into(),
        ],
    );
    draw_view(&mut tabs, &mut list, &theme, r(25.0, 210.0, 450.0, 36.0));
    let mut table = Table::new(
        WidgetId(9),
        vec!["Command".into(), "Shortcut".into()],
        vec![
            TableRow {
                label: "File".into(),
                detail: String::new(),
                section: true,
                indent: 0,
            },
            TableRow {
                label: "New".into(),
                detail: "Ctrl+N".into(),
                section: false,
                indent: 1,
            },
            TableRow {
                label: "Open".into(),
                detail: "Ctrl+O".into(),
                section: false,
                indent: 1,
            },
            TableRow {
                label: "Save".into(),
                detail: "Ctrl+S".into(),
                section: false,
                indent: 1,
            },
        ],
    );
    draw_view(&mut table, &mut list, &theme, r(25.0, 265.0, 420.0, 160.0));
    let mut scroll = ScrollBar::new(WidgetId(10), 500.0, 100.0);
    draw_view(&mut scroll, &mut list, &theme, r(450.0, 265.0, 10.0, 160.0));
    let mut hint = Label::new(
        WidgetId(11),
        "Click a shortcut, then press the new keys.",
        LabelKind::Hint,
    );
    draw_view(&mut hint, &mut list, &theme, r(25.0, 445.0, 360.0, 20.0));
    let mut warning = Label::new(
        WidgetId(12),
        "That key is already in use.",
        LabelKind::Warning,
    );
    draw_view(&mut warning, &mut list, &theme, r(25.0, 472.0, 360.0, 20.0));
    let mut file = MenuItem::command("File", qe_api::CommandId::Open);
    file.submenu = Some(MenuModel {
        items: vec![
            MenuItem::command("New", qe_api::CommandId::New),
            MenuItem::command("Open…", qe_api::CommandId::Open),
            MenuItem::separator(),
            MenuItem::command("Save", qe_api::CommandId::Save),
        ],
    });
    let mut menu = MenuView::new(
        WidgetId(13),
        MenuModel {
            items: vec![
                file,
                MenuItem::command("Edit", qe_api::CommandId::Undo),
                MenuItem::command("Settings", qe_api::CommandId::Settings),
            ],
        },
    );
    menu.layout(r(500.0, 60.0, 190.0, 115.0), &Measure);
    menu.open_keyboard();
    let _ = menu.handle(
        &qe_ui::key(0x27, qe_api::Modifiers::default()),
        &mut qe_api::ViewContext::default(),
    );
    draw_view(&mut menu, &mut list, &theme, r(500.0, 60.0, 190.0, 115.0));
    list
}

fn png(path: &PathBuf, width: u32, height: u32, bgra: &[u8]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity((width * height * 4 + height) as usize);
    for y in 0..height as usize {
        raw.push(0);
        for x in 0..width as usize {
            let i = (y * width as usize + x) * 4;
            raw.extend_from_slice(&[bgra[i + 2], bgra[i + 1], bgra[i], 255]);
        }
    }
    let mut z = vec![0x78, 0x01];
    for chunk in raw.chunks(65535) {
        let final_chunk = chunk.as_ptr_range().end == raw.as_ptr_range().end;
        z.push(u8::from(final_chunk));
        let len = chunk.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(chunk);
    }
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    fn crc(bytes: &[u8]) -> u32 {
        let mut c = !0u32;
        for &b in bytes {
            c ^= b as u32;
            for _ in 0..8 {
                c = (c >> 1) ^ if c & 1 != 0 { 0xedb88320 } else { 0 }
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc(&out[start..]).to_be_bytes());
    }
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    fs::write(path, out)
}

fn render(list: &DisplayList, path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height) = (900u32, 800u32);
    let mut info = BITMAPINFO::default();
    info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = width as i32;
    info.bmiHeader.biHeight = -(height as i32);
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB.0;
    let mut bits = std::ptr::null_mut();
    unsafe {
        let dc = CreateCompatibleDC(None);
        let bmp = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| std::io::Error::other(format!("CreateDIBSection: {e}")))?;
        let old = SelectObject(dc, bmp.into());
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, (width * height * 4) as usize);
        pixels.fill(255);
        for cmd in &list.commands {
            match cmd {
                DrawCommand::FillRect { rect, color }
                | DrawCommand::RoundedRect { rect, color, .. } => {
                    fill(pixels, width, height, *rect, *color)
                }
                DrawCommand::StrokeRect {
                    rect,
                    width: line_width,
                    color,
                    ..
                } => {
                    let s = line_width.0.max(1.0);
                    fill(
                        pixels,
                        width,
                        height,
                        r(rect.x.0, rect.y.0, rect.width.0, s),
                        *color,
                    );
                    fill(
                        pixels,
                        width,
                        height,
                        r(rect.x.0, rect.y.0 + rect.height.0 - s, rect.width.0, s),
                        *color,
                    );
                    fill(
                        pixels,
                        width,
                        height,
                        r(rect.x.0, rect.y.0, s, rect.height.0),
                        *color,
                    );
                    fill(
                        pixels,
                        width,
                        height,
                        r(rect.x.0 + rect.width.0 - s, rect.y.0, s, rect.height.0),
                        *color,
                    );
                }
                DrawCommand::Icon { at, icon_id, color } => {
                    let glyph = u32::from_str_radix(icon_id, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .unwrap_or('+');
                    let face: Vec<u16> = "Segoe Fluent Icons"
                        .encode_utf16()
                        .chain(std::iter::once(0))
                        .collect();
                    let f = CreateFontW(
                        -17,
                        0,
                        0,
                        0,
                        400,
                        0,
                        0,
                        0,
                        DEFAULT_CHARSET,
                        OUT_DEFAULT_PRECIS,
                        CLIP_DEFAULT_PRECIS,
                        DEFAULT_QUALITY,
                        0,
                        PCWSTR(face.as_ptr()),
                    );
                    let prev = SelectObject(dc, f.into());
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(
                        dc,
                        COLORREF(
                            color.r as u32 | ((color.g as u32) << 8) | ((color.b as u32) << 16),
                        ),
                    );
                    let _ = TextOutW(
                        dc,
                        at.x.0 as i32,
                        at.y.0 as i32,
                        glyph.encode_utf16(&mut [0; 2]),
                    );
                    let _ = GdiFlush();
                    SelectObject(dc, prev);
                    let _ = DeleteObject(f.into());
                }
                DrawCommand::Line {
                    from, to, color, ..
                } => {
                    let x = from.x.0.min(to.x.0);
                    let y = from.y.0.min(to.y.0);
                    fill(
                        pixels,
                        width,
                        height,
                        r(
                            x,
                            y,
                            (from.x.0 - to.x.0).abs().max(1.0),
                            (from.y.0 - to.y.0).abs().max(1.0),
                        ),
                        *color,
                    )
                }
                DrawCommand::TextRun {
                    at,
                    text,
                    font,
                    color,
                } => {
                    let face: Vec<u16> = font.encode_utf16().chain(std::iter::once(0)).collect();
                    let f = CreateFontW(
                        -17,
                        0,
                        0,
                        0,
                        400,
                        0,
                        0,
                        0,
                        DEFAULT_CHARSET,
                        OUT_DEFAULT_PRECIS,
                        CLIP_DEFAULT_PRECIS,
                        DEFAULT_QUALITY,
                        0,
                        PCWSTR(face.as_ptr()),
                    );
                    let prev = SelectObject(dc, f.into());
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(
                        dc,
                        COLORREF(
                            color.r as u32 | ((color.g as u32) << 8) | ((color.b as u32) << 16),
                        ),
                    );
                    let wide: Vec<u16> = text.encode_utf16().collect();
                    let _ = TextOutW(dc, at.x.0 as i32, at.y.0 as i32, &wide);
                    let _ = GdiFlush();
                    SelectObject(dc, prev);
                    let _ = DeleteObject(f.into());
                }
                _ => {}
            }
        }
        png(path, width, height, pixels)?;
        SelectObject(dc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(dc);
    }
    Ok(())
}
fn fill(pixels: &mut [u8], width: u32, height: u32, rect: Rect, color: Color) {
    let x0 = (rect.x.0 as i32).max(0) as usize;
    let y0 = (rect.y.0 as i32).max(0) as usize;
    let x1 = ((rect.x.0 + rect.width.0) as i32).clamp(0, width as i32) as usize;
    let y1 = ((rect.y.0 + rect.height.0) as i32).clamp(0, height as i32) as usize;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * width as usize + x) * 4;
            let a = color.a as u16;
            for (j, v) in [color.b, color.g, color.r].into_iter().enumerate() {
                pixels[i + j] = ((v as u16 * a + pixels[i + j] as u16 * (255 - a)) / 255) as u8
            }
            pixels[i + 3] = 255
        }
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let out = args
        .windows(2)
        .find(|a| a[0] == "--out")
        .map(|a| PathBuf::from(&a[1]))
        .unwrap_or_else(|| PathBuf::from("target/gallery"));
    fs::create_dir_all(&out)?;
    for (name, kind) in [("light", ThemeKind::Light), ("dark", ThemeKind::Dark)] {
        let list = gallery(Theme::new(kind));
        render(&list, &out.join(format!("widgets-{name}.png")))?;
    }
    println!("gallery: {}", out.display());
    Ok(())
}
