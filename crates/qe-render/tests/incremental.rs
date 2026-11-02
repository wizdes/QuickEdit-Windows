//! Drawing a little at a time must give the same pixels as drawing everything.
mod support;
use qe_api::{Bitmap, Color, Dip, DisplayList, DrawCommand, Rect, Renderer};
use qe_render::BitmapRenderer;
use support::*;

const FONT: u32 = 3;
const LINE: f32 = 17.0;

/// A small xorshift, so a failure can be replayed from its seed.
struct Random(u64);
impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn content_rect(height: f32, width: f32) -> Rect {
    rect(0.0, 40.0, width - 12.0, height - 40.0)
}

/// The top row and the scrolling text. Everything comes from `width`, `height`,
/// the theme and how far the text has scrolled.
fn scene(width: f32, height: f32, dark: bool, scrolled: f32) -> DisplayList {
    let (background, ink, row) = if dark {
        (
            DARK_BG,
            DARK_INK,
            Color {
                r: 38,
                g: 38,
                b: 38,
                a: 255,
            },
        )
    } else {
        (
            WHITE,
            BLACK,
            Color {
                r: 240,
                g: 240,
                b: 240,
                a: 255,
            },
        )
    };
    let mut list = DisplayList::default();
    fill(&mut list, rect(0.0, 0.0, width, height), background);
    fill(&mut list, rect(0.0, 0.0, width, 40.0), row);
    list.push(DrawCommand::RoundedRect {
        rect: rect(8.0, 8.0, 160.0, 32.0),
        radius: Dip(8.0),
        color: background,
    });
    list.push(text_run(UI, 12.0, "Notes 日本.txt", point(20.0, 29.0), ink));
    list.push(DrawCommand::Icon {
        at: point(190.0, 26.0),
        icon_id: "E710".into(),
        font_size: Dip(12.0),
        color: ink,
    });
    list.push(DrawCommand::Line {
        from: point(220.0, 10.0),
        to: point(250.0, 32.0),
        width: Dip(1.5),
        color: ink,
    });
    let content = content_rect(height, width);
    list.push(DrawCommand::PushClip(content));
    let first = ((scrolled / LINE).floor() as i32 - 2).max(0);
    let last = first + (height / LINE) as i32 + 6;
    for line in first..last {
        let top = 40.0 + line as f32 * LINE - scrolled;
        if line % 4 == 1 {
            list.push(DrawCommand::FillRects {
                rects: vec![
                    rect(30.0, top, 200.0, LINE),
                    rect(30.0, top + LINE, 90.0, LINE),
                ],
                color: Color {
                    r: 0,
                    g: 120,
                    b: 215,
                    a: 90,
                },
            });
        }
        list.push(glyph_run(
            FONT,
            MONO,
            13.0 + (line % 3) as f32,
            &format!("{line:03} The quick brown fox jumps"),
            point(4.0, top + 13.0),
            ink,
        ));
        if line % 5 == 2 {
            list.push(text_run(EMOJI, 13.0, "😀", point(280.0, top + 13.0), BLACK));
            list.push(DrawCommand::WavyUnderline {
                from: point(30.0, top + 15.0),
                to: point(170.0, top + 15.0),
                color: Color {
                    r: 179,
                    g: 38,
                    b: 30,
                    a: 255,
                },
            });
            list.push(DrawCommand::PushOpacity(0.5));
            list.push(DrawCommand::StrokeRect {
                rect: rect(200.0, top + 1.0, 60.0, LINE - 2.0),
                radius: Dip(4.0),
                width: Dip(1.0),
                color: ink,
            });
            list.push(text_run(MONO, 11.0, "faded", point(205.0, top + 12.0), ink));
            list.push(DrawCommand::PopOpacity);
        }
    }
    list.push(DrawCommand::PopClip);
    fill(
        &mut list,
        rect(width - 12.0, 40.0, 12.0, height - 40.0),
        row,
    );
    let thumb = if dark { 120 } else { 150 };
    list.push(DrawCommand::RoundedRect {
        rect: rect(width - 10.0, 60.0, 8.0, 40.0),
        radius: Dip(4.0),
        color: Color {
            r: thumb,
            g: thumb,
            b: thumb,
            a: 255,
        },
    });
    list
}

struct Run {
    renderer: BitmapRenderer,
    width_px: u32,
    height_px: u32,
    dpi: u32,
    dark: bool,
    scrolled: f32,
}
impl Run {
    fn dips(&self) -> (f32, f32) {
        let s = self.dpi as f32 / 96.0;
        (self.width_px as f32 / s, self.height_px as f32 / s)
    }
    fn list(&self) -> DisplayList {
        let (w, h) = self.dips();
        scene(w, h, self.dark, self.scrolled)
    }
    fn expected(&self) -> Bitmap {
        fresh(
            self.width_px,
            self.height_px,
            self.dpi,
            &self.list(),
            &[(FONT, MONO)],
        )
    }
    fn check(&mut self, what: &str) {
        let list = self.list();
        frame(&mut self.renderer, &list);
        let actual = self.renderer.bitmap().unwrap();
        let wrong = difference(&actual, &self.expected());
        assert_eq!(
            wrong, 0,
            "{wrong} pixels differ after {what} at dpi {} dark {}",
            self.dpi, self.dark
        );
    }
    /// Scrolling up past the start is not possible, so the amount is cut there.
    fn scroll_pixels(&mut self, pixels: f32) {
        let s = self.dpi as f32 / 96.0;
        let (w, h) = self.dips();
        let dips = pixels / s;
        if self.scrolled + dips < 0.0 {
            return;
        }
        self.renderer
            .scroll(content_rect(h, w), Dip(-dips))
            .unwrap();
        self.scrolled += dips;
    }
}

fn run_sequence(dpi: u32, dark: bool, seed: u64) {
    let mut renderer = BitmapRenderer::new(420, 300).unwrap();
    renderer.set_dpi(dpi).unwrap();
    renderer.register_font(FONT, MONO).unwrap();
    let mut run = Run {
        renderer,
        width_px: 420,
        height_px: 300,
        dpi,
        dark,
        scrolled: 0.0,
    };
    run.check("the first frame");
    let mut random = Random(seed);
    for step in 0..30 {
        let (w, h) = run.dips();
        let what = match random.below(6) {
            0 => {
                // A line of the text: nothing changed, so nothing may move.
                let line = random.below(10) as f32;
                run.renderer
                    .invalidate(rect(0.0, 40.0 + line * LINE, w, LINE));
                format!("step {step}: invalidate line {line}")
            }
            1 | 2 => {
                let pixels = random.below(61) as f32 - 30.0;
                run.scroll_pixels(pixels);
                format!("step {step}: scroll {pixels} pixels")
            }
            3 => {
                // A shift that is not a whole number of pixels at this scale.
                let dips = 3.0 + random.below(30) as f32 + 0.3;
                run.renderer.scroll(content_rect(h, w), Dip(-dips)).unwrap();
                run.scrolled += dips;
                format!("step {step}: scroll {dips} dips")
            }
            4 => {
                run.width_px = 300 + random.below(240) as u32;
                run.height_px = 200 + random.below(200) as u32;
                run.renderer.resize(run.width_px, run.height_px).unwrap();
                format!("step {step}: resize {}x{}", run.width_px, run.height_px)
            }
            _ => {
                let dpi = [96, 120, 144, 168, 192][random.below(5) as usize];
                run.dpi = dpi;
                run.renderer.set_dpi(dpi).unwrap();
                format!("step {step}: DPI {dpi}")
            }
        };
        // Sometimes a second change comes before the draw.
        if random.below(3) == 0 {
            let (w, _) = run.dips();
            run.renderer.invalidate(rect(0.0, 44.0, w, 6.0));
            run.scroll_pixels(random.below(21) as f32);
        }
        run.check(&what);
    }
}

#[test]
fn incremental_drawing_matches_full_redraw_at_every_scale_and_theme() {
    for dark in [false, true] {
        for (index, dpi) in [96u32, 120, 144, 168, 192].into_iter().enumerate() {
            run_sequence(
                dpi,
                dark,
                0x9E37_79B9_7F4A_7C15 + index as u64 * 77 + u64::from(dark),
            );
        }
    }
}

#[test]
fn line_and_strip_redraws_match_full_redraw_at_every_scale() {
    for dark in [false, true] {
        for dpi in [96u32, 120, 144, 168, 192] {
            let mut renderer = BitmapRenderer::new(420, 300).unwrap();
            renderer.set_dpi(dpi).unwrap();
            renderer.register_font(FONT, MONO).unwrap();
            let s = dpi as f32 / 96.0;
            let list = scene(420.0 / s, 300.0 / s, dark, 0.0);
            frame(&mut renderer, &list);
            let full = renderer.bitmap().unwrap();
            for y in (0..60).step_by(7) {
                renderer.invalidate(rect(0.0, 40.0 + y as f32, 420.0 / s, 9.3));
                frame(&mut renderer, &list);
                assert_eq!(
                    difference(&renderer.bitmap().unwrap(), &full),
                    0,
                    "dpi {dpi} y {y}"
                );
            }
        }
    }
}
