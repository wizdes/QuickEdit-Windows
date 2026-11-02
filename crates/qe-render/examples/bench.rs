use qe_api::{Color, Dip, DisplayList, DrawCommand, Point, Rect, Renderer};
use qe_render::BitmapRenderer;
use std::time::{Duration, Instant};

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(w),
        height: Dip(h),
    }
}
fn text_frame(width: u32, height: u32) -> DisplayList {
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, width as f32, height as f32),
        color: Color::rgb(255, 255, 255),
    });
    let line =
        "QuickEdit speed test 0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ "
            .repeat(width as usize / 650 + 1);
    for y in (16..height).step_by(17) {
        list.push(DrawCommand::TextRun {
            at: Point {
                x: Dip(2.0),
                y: Dip(y as f32),
            },
            text: line.clone(),
            font: "Cascadia Mono".into(),
            color: Color::rgb(0, 0, 0),
        });
    }
    list
}
fn measure(
    mut task: impl FnMut() -> Result<(), Box<dyn std::error::Error>>,
) -> Result<Duration, Box<dyn std::error::Error>> {
    let mut samples = Vec::new();
    for _ in 0..8 {
        let start = Instant::now();
        task()?;
        samples.push(start.elapsed());
    }
    samples.sort();
    Ok(samples[4])
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    for (width, height) in [(1920, 1080), (3840, 2160)] {
        let mut renderer = BitmapRenderer::new(width, height)?;
        let frame = text_frame(width, height);
        renderer.begin_frame()?;
        renderer.draw(&frame)?;
        renderer.end_frame()?;
        let full = measure(|| {
            renderer.begin_frame()?;
            renderer.draw(&frame)?;
            renderer.end_frame()?;
            Ok(())
        })?;
        let line = measure(|| {
            renderer.invalidate(rect(0.0, 0.0, width as f32, 17.0));
            renderer.begin_frame()?;
            renderer.draw(&frame)?;
            renderer.end_frame()?;
            Ok(())
        })?;
        let scroll = measure(|| {
            renderer.scroll(rect(0.0, 0.0, width as f32, height as f32), Dip(-17.0))?;
            renderer.begin_frame()?;
            renderer.draw(&frame)?;
            renderer.end_frame()?;
            Ok(())
        })?;
        println!(
            "measured on this PC: {width}x{height} full text median {:.3} ms; single line {:.3} ms; scroll line {:.3} ms; buffer {} bytes",
            full.as_secs_f64() * 1000.0,
            line.as_secs_f64() * 1000.0,
            scroll.as_secs_f64() * 1000.0,
            renderer.back_buffer_bytes()
        );
    }
    let mut renderer = BitmapRenderer::new(1920, 1080)?;
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, 1920.0, 1080.0),
        color: Color::rgb(255, 255, 255),
    });
    let rects = (0..5000)
        .map(|i| rect((i % 100) as f32, ((i / 100) * 2) as f32, 1.0, 1.0))
        .collect();
    list.push(DrawCommand::FillRects {
        rects,
        color: Color::rgb(0, 0, 0),
    });
    let elapsed = measure(|| {
        renderer.begin_frame()?;
        renderer.draw(&list)?;
        renderer.end_frame()?;
        Ok(())
    })?;
    println!(
        "measured on this PC: document map FillRects 5000 median {:.3} ms",
        elapsed.as_secs_f64() * 1000.0
    );
    Ok(())
}
