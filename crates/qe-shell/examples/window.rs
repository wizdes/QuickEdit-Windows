//! Run with no arguments for a window, or --png PATH for a headless image.
use qe_api::{
    Bitmap, Color, Dip, DisplayList, DrawCommand, RenderError, Renderer, TextMeasurer, TextMetrics,
    Theme, ThemeKind, View,
};
use qe_shell::{ShellEvent, ShellTokens, ShellWindow, Tab, TabStrip};
use std::ffi::c_void;
use std::fs;
use std::ptr::null_mut;

type Handle = *mut c_void;
#[repr(C)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
#[repr(C)]
struct Header {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bits: u16,
    compression: u32,
    image_size: u32,
    xppm: i32,
    yppm: i32,
    used: u32,
    important: u32,
}
#[repr(C)]
struct Info {
    header: Header,
    colors: [u32; 3],
}
#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(dc: Handle) -> Handle;
    fn DeleteDC(dc: Handle) -> i32;
    fn CreateDIBSection(
        dc: Handle,
        info: *const Info,
        usage: u32,
        bits: *mut Handle,
        section: Handle,
        offset: u32,
    ) -> Handle;
    fn SelectObject(dc: Handle, obj: Handle) -> Handle;
    fn DeleteObject(obj: Handle) -> i32;
    fn CreateSolidBrush(color: u32) -> Handle;
    fn CreatePen(style: i32, width: i32, color: u32) -> Handle;
    fn CreateFontW(
        height: i32,
        width: i32,
        esc: i32,
        orient: i32,
        weight: i32,
        italic: u32,
        underline: u32,
        strike: u32,
        charset: u32,
        out: u32,
        clip: u32,
        quality: u32,
        pitch: u32,
        face: *const u16,
    ) -> Handle;
    fn RoundRect(
        dc: Handle,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        ellipse_w: i32,
        ellipse_h: i32,
    ) -> i32;
    fn Rectangle(dc: Handle, left: i32, top: i32, right: i32, bottom: i32) -> i32;
    fn MoveToEx(dc: Handle, x: i32, y: i32, old: *mut c_void) -> i32;
    fn LineTo(dc: Handle, x: i32, y: i32) -> i32;
    fn SetTextColor(dc: Handle, color: u32) -> u32;
    fn SetBkMode(dc: Handle, mode: i32) -> i32;
    fn TextOutW(dc: Handle, x: i32, y: i32, text: *const u16, len: i32) -> i32;
    fn SaveDC(dc: Handle) -> i32;
    fn RestoreDC(dc: Handle, n: i32) -> i32;
    fn IntersectClipRect(dc: Handle, left: i32, top: i32, right: i32, bottom: i32) -> i32;
}
#[link(name = "user32")]
unsafe extern "system" {
    fn FillRect(dc: Handle, rect: *const WinRect, brush: Handle) -> i32;
}
fn rgb(c: Color) -> u32 {
    u32::from(c.r) | (u32::from(c.g) << 8) | (u32::from(c.b) << 16)
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
fn scaled(v: f32, dpi: u32) -> i32 {
    (v * dpi as f32 / 96.0).round() as i32
}
fn rect(b: qe_api::Rect, dpi: u32) -> WinRect {
    WinRect {
        left: scaled(b.x.0, dpi),
        top: scaled(b.y.0, dpi),
        right: scaled(b.x.0 + b.width.0, dpi),
        bottom: scaled(b.y.0 + b.height.0, dpi),
    }
}
struct DemoRenderer {
    dpi: u32,
    dark: bool,
}
impl DemoRenderer {
    fn new(dark: bool) -> Self {
        Self { dpi: 96, dark }
    }
}
impl Renderer for DemoRenderer {
    fn begin_frame(&mut self) -> Result<(), RenderError> {
        Ok(())
    }
    fn draw(&mut self, _list: &DisplayList) -> Result<(), RenderError> {
        Ok(())
    }
    fn end_frame(&mut self) -> Result<(), RenderError> {
        Ok(())
    }
    fn resize(&mut self, _width: u32, _height: u32) -> Result<(), RenderError> {
        Ok(())
    }
    fn set_dpi(&mut self, dpi: u32) -> Result<(), RenderError> {
        self.dpi = dpi;
        Ok(())
    }
    fn render_to_bitmap(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Bitmap, RenderError> {
        let info = Info {
            header: Header {
                size: 40,
                width: width as i32,
                height: -(height as i32),
                planes: 1,
                bits: 32,
                compression: 0,
                image_size: 0,
                xppm: 0,
                yppm: 0,
                used: 0,
                important: 0,
            },
            colors: [0; 3],
        };
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        if dc.is_null() {
            return Err(RenderError::new("CreateCompatibleDC failed"));
        }
        let mut bits = null_mut();
        let bitmap = unsafe { CreateDIBSection(dc, &info, 0, &mut bits, null_mut(), 0) };
        if bitmap.is_null() {
            unsafe {
                DeleteDC(dc);
            }
            return Err(RenderError::new("CreateDIBSection failed"));
        }
        let old = unsafe { SelectObject(dc, bitmap) };
        let light = Theme::new(ThemeKind::Light);
        let dark = Theme::new(ThemeKind::Dark);
        let dark_row = ShellTokens::from_theme(&Theme::new(ThemeKind::Dark)).row;
        let row = list.commands.first().and_then(|command| match command {
            DrawCommand::FillRect { color, .. } => Some(*color),
            _ => None,
        });
        let background = match row {
            Some(color) if color == dark_row || self.dark => dark.text_background,
            Some(color) if color == light.tab_background => light.text_background,
            Some(color) => color,
            None => light.text_background,
        };
        unsafe {
            let brush = CreateSolidBrush(rgb(background));
            FillRect(
                dc,
                &WinRect {
                    left: 0,
                    top: 0,
                    right: width as i32,
                    bottom: height as i32,
                },
                brush,
            );
            DeleteObject(brush);
            SetBkMode(dc, 1);
        }
        let mut clip_depth = 0;
        for command in &list.commands {
            unsafe {
                match command {
                    DrawCommand::FillRect { rect: b, color } => {
                        let b = rect(*b, self.dpi);
                        let brush = CreateSolidBrush(rgb(*color));
                        FillRect(dc, &b, brush);
                        DeleteObject(brush);
                    }
                    DrawCommand::RoundedRect {
                        rect: b,
                        radius,
                        color,
                    } => {
                        let b = rect(*b, self.dpi);
                        let brush = CreateSolidBrush(rgb(*color));
                        let pen = CreatePen(0, 1, rgb(*color));
                        let old_brush = SelectObject(dc, brush);
                        let old_pen = SelectObject(dc, pen);
                        RoundRect(
                            dc,
                            b.left,
                            b.top,
                            b.right,
                            b.bottom,
                            scaled(radius.0 * 2.0, self.dpi),
                            scaled(radius.0 * 2.0, self.dpi),
                        );
                        SelectObject(dc, old_brush);
                        SelectObject(dc, old_pen);
                        DeleteObject(brush);
                        DeleteObject(pen);
                    }
                    DrawCommand::StrokeRect {
                        rect: b,
                        width,
                        color,
                        ..
                    } => {
                        let b = rect(*b, self.dpi);
                        let pen = CreatePen(0, scaled(width.0, self.dpi).max(1), rgb(*color));
                        let old_pen = SelectObject(dc, pen);
                        let old_brush = SelectObject(dc, GetStockObject(5));
                        Rectangle(dc, b.left, b.top, b.right, b.bottom);
                        SelectObject(dc, old_pen);
                        SelectObject(dc, old_brush);
                        DeleteObject(pen);
                    }
                    DrawCommand::Line {
                        from,
                        to,
                        width,
                        color,
                    } => {
                        let pen = CreatePen(0, scaled(width.0, self.dpi).max(1), rgb(*color));
                        let old_pen = SelectObject(dc, pen);
                        MoveToEx(
                            dc,
                            scaled(from.x.0, self.dpi),
                            scaled(from.y.0, self.dpi),
                            null_mut(),
                        );
                        LineTo(dc, scaled(to.x.0, self.dpi), scaled(to.y.0, self.dpi));
                        SelectObject(dc, old_pen);
                        DeleteObject(pen);
                    }
                    DrawCommand::TextRun {
                        at,
                        text,
                        font,
                        color,
                    } => {
                        let tokens = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
                        let family = wide(if font == tokens.caption_font {
                            tokens.icon_font
                        } else {
                            font
                        });
                        let size = if font == tokens.caption_font {
                            tokens.caption_icon
                        } else if font == tokens.icon_font {
                            tokens.icon_font_size
                        } else {
                            tokens.text_font_size
                        };
                        let font_handle = CreateFontW(
                            -scaled(size, self.dpi),
                            0,
                            0,
                            0,
                            400,
                            0,
                            0,
                            0,
                            1,
                            0,
                            0,
                            5,
                            0,
                            family.as_ptr(),
                        );
                        let old_font = SelectObject(dc, font_handle);
                        SetTextColor(dc, rgb(*color));
                        let utf: Vec<u16> = text.encode_utf16().collect();
                        TextOutW(
                            dc,
                            scaled(at.x.0, self.dpi),
                            scaled(at.y.0, self.dpi),
                            utf.as_ptr(),
                            utf.len() as i32,
                        );
                        SelectObject(dc, old_font);
                        DeleteObject(font_handle);
                    }
                    DrawCommand::PushClip(b) => {
                        SaveDC(dc);
                        clip_depth += 1;
                        let b = rect(*b, self.dpi);
                        IntersectClipRect(dc, b.left, b.top, b.right, b.bottom);
                    }
                    DrawCommand::PopClip if clip_depth > 0 => {
                        RestoreDC(dc, -1);
                        clip_depth -= 1;
                    }
                    _ => {}
                }
            }
        }
        while clip_depth > 0 {
            unsafe {
                RestoreDC(dc, -1);
            }
            clip_depth -= 1;
        }
        let length = width as usize * height as usize * 4;
        let bgra = unsafe { std::slice::from_raw_parts(bits as *const u8, length).to_vec() };
        unsafe {
            SelectObject(dc, old);
            DeleteObject(bitmap);
            DeleteDC(dc);
        }
        Ok(Bitmap {
            width,
            height,
            bgra,
        })
    }
}
#[link(name = "gdi32")]
unsafe extern "system" {
    fn GetStockObject(index: i32) -> Handle;
}
struct DemoMeasure;
impl TextMeasurer for DemoMeasure {
    fn measure(&self, text: &str, _font: &str) -> TextMetrics {
        TextMetrics {
            width: Dip(text.chars().count() as f32 * 7.0),
            height: Dip(14.0),
            baseline: Dip(11.0),
        }
    }
}
fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb88320 } else { 0 };
        }
    }
    !crc
}
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let start = out.len() - data.len() - 4;
    out.extend_from_slice(&checksum(&out[start..]).to_be_bytes());
}
fn save_png(path: &str, bitmap: &Bitmap) -> Result<(), String> {
    let mut raw = Vec::new();
    for row in bitmap.bgra.chunks_exact(bitmap.width as usize * 4) {
        raw.push(0);
        for px in row.as_chunks::<4>().0 {
            raw.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut remaining = raw.as_slice();
    while !remaining.is_empty() {
        let n = remaining.len().min(65535);
        let final_block = n == remaining.len();
        z.push(u8::from(final_block));
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        z.extend_from_slice(&remaining[..n]);
        remaining = &remaining[n..];
    }
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in &raw {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&bitmap.width.to_be_bytes());
    ihdr.extend_from_slice(&bitmap.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    fs::write(path, png).map_err(|e| e.to_string())
}
fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|x| x == "--png") {
        let path = args.get(2).ok_or("usage: window --png PATH [dark]")?;
        let dark = args.get(3).is_some_and(|x| x == "dark");
        let mut strip = TabStrip::new(vec![
            Tab::new("Untitled"),
            Tab::new("Example.rs"),
            Tab::new("A longer tab name"),
        ]);
        strip.tabs_mut()[1].dirty = true;
        strip.layout(
            qe_api::Rect {
                x: Dip(0.0),
                y: Dip(0.0),
                width: Dip(900.0),
                height: Dip(40.0),
            },
            &DemoMeasure,
        );
        let mut list = DisplayList::default();
        strip.draw(
            &mut list,
            &Theme::new(if dark {
                ThemeKind::Dark
            } else {
                ThemeKind::Light
            }),
            &DemoMeasure,
        );
        let mut renderer = DemoRenderer::new(dark);
        let image = renderer
            .render_to_bitmap(&list, 900, 80)
            .map_err(|e| e.to_string())?;
        save_png(path, &image)?;
        println!("wrote {path}");
        return Ok(());
    }
    let mut window = ShellWindow::create(
        Box::new(DemoRenderer::new(false)),
        Box::new(DemoMeasure),
        8.0,
        17.0,
    )?;
    window.strip_mut().add_tab(Tab::new("Example.rs"));
    window.strip_mut().add_tab(Tab::new("A longer tab name"));
    window.set_event_handler(|strip, event| {
        if let ShellEvent::NewTab = event {
            strip.add_tab(Tab::new(format!("Untitled {}", strip.tabs().len() + 1)));
        }
    });
    window.show_and_run()
}
