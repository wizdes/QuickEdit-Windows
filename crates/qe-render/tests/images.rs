use qe_api::{Bitmap, Color, Dip, DisplayList, DrawCommand, Point, Rect, Renderer};
use qe_render::BitmapRenderer;
use std::{fs, path::PathBuf};

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(w),
        height: Dip(h),
    }
}
fn point(x: f32, y: f32) -> Point {
    Point {
        x: Dip(x),
        y: Dip(y),
    }
}
fn scene(dark: bool) -> DisplayList {
    let mut list = DisplayList::default();
    let background = if dark {
        Color::rgb(30, 30, 30)
    } else {
        Color::rgb(255, 255, 255)
    };
    let ink = if dark {
        Color::rgb(255, 255, 255)
    } else {
        Color::rgb(0, 0, 0)
    };
    list.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, 400.0, 180.0),
        color: background,
    });
    list.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, 400.0, 40.0),
        color: if dark {
            Color::rgb(38, 38, 38)
        } else {
            Color::rgb(240, 240, 240)
        },
    });
    list.push(DrawCommand::RoundedRect {
        rect: rect(8.0, 8.0, 160.0, 32.0),
        radius: Dip(8.0),
        color: background,
    });
    list.push(DrawCommand::TextRun {
        at: point(22.0, 29.0),
        text: "QuickEdit".into(),
        font: "Cascadia Mono".into(),
        color: ink,
    });
    for (x, id) in [
        (181.0, "E710"),
        (212.0, "E96E"),
        (296.0, "E921"),
        (327.0, "E922"),
        (358.0, "E8BB"),
    ] {
        list.push(DrawCommand::Icon {
            at: rect(x, 8.0, 24.0, 24.0),
            icon_id: id.into(),
            color: ink,
        });
    }
    list.push(DrawCommand::FillRects {
        rects: (0..200)
            .map(|i| {
                rect(
                    350.0 + (i % 25) as f32,
                    51.0 + (i / 25) as f32 * 3.0,
                    1.0,
                    1.5,
                )
            })
            .collect(),
        color: Color::rgb(95, 95, 95),
    });
    list.push(DrawCommand::RoundedRect {
        rect: rect(12.0, 53.0, 90.0, 36.0),
        radius: Dip(12.0),
        color: Color::rgb(55, 130, 200),
    });
    list.push(DrawCommand::StrokeRect {
        rect: rect(113.0, 53.0, 90.0, 36.0),
        radius: Dip(8.0),
        width: Dip(2.0),
        color: Color::rgb(180, 50, 70),
    });
    list.push(DrawCommand::Line {
        from: point(12.0, 100.0),
        to: point(203.0, 100.0),
        width: Dip(1.0),
        color: ink,
    });
    list.push(DrawCommand::Line {
        from: point(215.0, 55.0),
        to: point(245.0, 85.0),
        width: Dip(1.5),
        color: ink,
    });
    list.push(DrawCommand::WavyUnderline {
        from: point(12.0, 126.0),
        to: point(195.0, 126.0),
        color: Color::rgb(179, 38, 30),
    });
    list.push(DrawCommand::PushClip(rect(210.0, 100.0, 42.0, 30.0)));
    list.push(DrawCommand::FillRect {
        rect: rect(200.0, 95.0, 60.0, 40.0),
        color: Color::rgb(255, 100, 0),
    });
    list.push(DrawCommand::PopClip);
    list.push(DrawCommand::PushOpacity(0.5));
    list.push(DrawCommand::FillRect {
        rect: rect(260.0, 100.0, 45.0, 30.0),
        color: Color::rgb(0, 60, 255),
    });
    list.push(DrawCommand::PopOpacity);
    list.push(DrawCommand::TextRun {
        at: point(12.0, 159.0),
        text: "Cascadia Mono 0123456789".into(),
        font: "Cascadia Mono".into(),
        color: ink,
    });
    list.push(DrawCommand::TextRun {
        at: point(310.0, 159.0),
        text: "😀".into(),
        font: "Segoe UI Emoji".into(),
        color: Color::rgb(0, 0, 0),
    });
    list
}

#[test]
fn text_draws_at_125_and_175_percent() {
    for (dpi, width, height) in [(120, 500, 225), (168, 700, 315)] {
        let mut renderer = BitmapRenderer::new(width, height).unwrap();
        renderer.set_dpi(dpi).unwrap();
        let image = renderer
            .render_to_bitmap(&scene(false), width, height)
            .unwrap();
        assert!(image.bgra.as_chunks::<4>().0.iter().any(|p| p[0] < 20));
    }
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
fn raw_pixels(b: &Bitmap) -> Vec<u8> {
    let mut raw = Vec::with_capacity((b.width * b.height * 4 + b.height) as usize);
    for row in b.bgra.chunks_exact(b.width as usize * 4) {
        raw.push(0);
        for p in row.as_chunks::<4>().0 {
            raw.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    raw
}
fn encode_png(b: &Bitmap) -> Vec<u8> {
    let raw = raw_pixels(b);
    let mut z = vec![0x78, 0x01];
    let mut a = 1u32;
    let mut c = 0u32;
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65521;
        c = (c + a) % 65521;
    }
    let blocks = raw.chunks(65535).collect::<Vec<_>>();
    for (i, block) in blocks.iter().enumerate() {
        let len = block.len() as u16;
        z.push(if i + 1 == blocks.len() { 1 } else { 0 });
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
fn decode_own_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("bad PNG signature".into());
    }
    let mut at = 8;
    let mut z = Vec::new();
    while at + 12 <= bytes.len() {
        let len =
            u32::from_be_bytes(bytes[at..at + 4].try_into().map_err(|_| "bad length")?) as usize;
        at += 4;
        let kind = &bytes[at..at + 4];
        at += 4;
        if at + len + 4 > bytes.len() {
            return Err("truncated PNG".into());
        }
        if kind == b"IDAT" {
            z.extend_from_slice(&bytes[at..at + len]);
        }
        at += len + 4;
        if kind == b"IEND" {
            break;
        }
    }
    if z.len() < 6 {
        return Err("missing zlib stream".into());
    }
    let mut at = 2;
    let mut raw = Vec::new();
    loop {
        let header = z[at];
        at += 1;
        let len = u16::from_le_bytes([z[at], z[at + 1]]) as usize;
        at += 4;
        if at + len > z.len() {
            return Err("truncated deflate block".into());
        }
        raw.extend_from_slice(&z[at..at + len]);
        at += len;
        if header & 1 != 0 {
            break;
        }
    }
    Ok(raw)
}
#[test]
fn saved_images_at_three_dpis() {
    for (dpi, scale) in [(96, 1), (144, 3), (192, 2)] {
        for dark in [false, true] {
            let (w, h) = if scale == 3 {
                (600, 270)
            } else {
                (400 * scale, 180 * scale)
            };
            let mut renderer = BitmapRenderer::new(w, h).unwrap();
            renderer.set_dpi(dpi).unwrap();
            let image = renderer.render_to_bitmap(&scene(dark), w, h).unwrap();
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/images")
                .join(if dark {
                    format!("scene-dark-{dpi}.png")
                } else {
                    format!("scene-{dpi}.png")
                });
            if std::env::var_os("QE_RENDER_UPDATE_IMAGES").is_some() {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, encode_png(&image)).unwrap();
            }
            let saved = decode_own_png(&fs::read(&path).unwrap()).unwrap();
            let current = raw_pixels(&image);
            assert_eq!(saved.len(), current.len());
            let different = saved
                .iter()
                .zip(&current)
                .filter(|(a, b)| a.abs_diff(**b) > 3)
                .count();
            assert_eq!(
                different,
                0,
                "{} differs in {different} channels",
                path.display()
            );
        }
    }
}
