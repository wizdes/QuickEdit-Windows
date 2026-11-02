//! Does the same input always give the same pixels? Each round is a new process, with
//! several threads that start together, because a problem may need a cold start.
//!
//! Modes, as `repeat_stress MODE PROCESSES THREADS`:
//! - `raw`: DirectWrite's own glyph coverage, with no qe-render code in the path.
//! - `locked`: the same, with each thread holding one lock around each glyph.
//! - `mixed`: half the threads as in `locked`, and half making text layouts with no lock.
//! - `renderer`: whole frames through `BitmapRenderer`.
//!
//! It prints every key that ever had two answers.
//! Set `QE_STRESS_TEXT` to try only some glyphs. Set `QE_STRESS_SPIN` to a number of busy threads, because the difference needs load.
use qe_api::{Color, Dip, DisplayList, DrawCommand, Point, Rect, Renderer};
use qe_render::{BitmapRenderer, TextSettings};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader},
    mem::ManuallyDrop,
    process::{Command, Stdio},
    sync::{Arc, Barrier, Mutex},
    thread,
};
use windows::{
    Win32::Graphics::DirectWrite::*,
    core::{BOOL, HSTRING, PCWSTR},
};

#[derive(Debug)]
struct Failure(String);
impl<E: std::fmt::Display> From<E> for Failure {
    fn from(e: E) -> Self {
        Self(e.to_string())
    }
}
static LOCK: Mutex<()> = Mutex::new(());
const TEXT: &str =
    "A longer tab name Untitled Example.rs notes.txt Read me first The quick brown fox 0123456789";
const SIZES: [f32; 4] = [13.0, 19.5, 26.0, 16.25];
const MODES: [DWRITE_RENDERING_MODE; 2] = [
    DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
    DWRITE_RENDERING_MODE_NATURAL,
];

fn face(family: &str) -> Result<IDWriteFontFace, Failure> {
    let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
    let mut collection = None;
    unsafe { factory.GetSystemFontCollection(&mut collection, false)? };
    let collection = collection.ok_or("no font collection")?;
    let (mut index, mut exists) = (0, BOOL(0));
    let name = HSTRING::from(family);
    unsafe { collection.FindFamilyName(PCWSTR(name.as_ptr()), &mut index, &mut exists)? };
    let face = unsafe {
        collection
            .GetFontFamily(index)?
            .GetFirstMatchingFont(
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
            )?
            .CreateFontFace()?
    };
    Ok(face)
}
/// DirectWrite only: one line for each glyph, size and subpixel position.
fn raw_thread(order: usize, lock: bool, out: &mut Vec<String>) -> Result<(), Failure> {
    let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
    let font = face("Segoe UI")?;
    // QE_STRESS_TEXT narrows the run to a few glyphs, so that a run can have many more
    // processes.
    let text = std::env::var("QE_STRESS_TEXT").unwrap_or_else(|_| TEXT.to_owned());
    let points: Vec<u32> = text.chars().map(|c| c as u32).collect();
    let mut ids = vec![0u16; points.len()];
    unsafe { font.GetGlyphIndices(points.as_ptr(), points.len() as u32, ids.as_mut_ptr())? };
    ids.sort_unstable();
    ids.dedup();
    // Each thread takes the glyphs in its own order.
    let turn = order * 7 % ids.len();
    ids.rotate_left(turn);
    // Each thread also starts at its own size, so that two threads can meet in one glyph
    // at two sizes.
    let mut sizes = SIZES;
    sizes.rotate_left(order % SIZES.len());
    for (em, mode) in sizes
        .into_iter()
        .flat_map(|em| MODES.into_iter().map(move |mode| (em, mode)))
    {
        for &glyph in &ids {
            for position in 0..4 {
                let _held = lock.then(|| LOCK.lock());
                let (advance, offset) = (0f32, DWRITE_GLYPH_OFFSET::default());
                let mut run = DWRITE_GLYPH_RUN {
                    fontFace: ManuallyDrop::new(Some(font.clone())),
                    fontEmSize: em,
                    glyphCount: 1,
                    glyphIndices: &glyph,
                    glyphAdvances: &advance,
                    glyphOffsets: &offset,
                    isSideways: BOOL(0),
                    bidiLevel: 0,
                };
                let analysis = unsafe {
                    factory.CreateGlyphRunAnalysis(
                        &run,
                        1.0,
                        None,
                        mode,
                        DWRITE_MEASURING_MODE_NATURAL,
                        position as f32 / 4.0,
                        0.0,
                    )
                };
                unsafe { ManuallyDrop::drop(&mut run.fontFace) };
                let analysis = analysis?;
                let bounds =
                    unsafe { analysis.GetAlphaTextureBounds(DWRITE_TEXTURE_CLEARTYPE_3x1)? };
                let (w, h) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
                if w <= 0 || h <= 0 {
                    continue;
                }
                let mut texture = vec![0u8; w as usize * h as usize * 3];
                unsafe {
                    analysis.CreateAlphaTexture(
                        DWRITE_TEXTURE_CLEARTYPE_3x1,
                        &bounds,
                        &mut texture,
                    )?;
                }
                out.push(format!(
                    "em{em} mode{} glyph{glyph} pos{position} {w}x{h} {}",
                    mode.0,
                    hex(&texture)
                ));
            }
        }
    }
    Ok(())
}
/// Text layouts, as qe-layout makes on other threads, with no lock.
fn layout_thread() -> Result<(), Failure> {
    let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
    let units: Vec<u16> = TEXT.encode_utf16().collect();
    let (family, locale) = (HSTRING::from("Segoe UI"), HSTRING::from("en-US"));
    for em in SIZES {
        let format = unsafe {
            factory.CreateTextFormat(
                PCWSTR(family.as_ptr()),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                em,
                PCWSTR(locale.as_ptr()),
            )?
        };
        let layout = unsafe { factory.CreateTextLayout(&units, &format, 10000.0, 1000.0)? };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics)? };
    }
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(w),
        height: Dip(h),
    }
}
fn scene(dark: bool) -> DisplayList {
    let (bg, ink) = if dark {
        (Color::rgb(38, 38, 38), Color::rgb(255, 255, 255))
    } else {
        (Color::rgb(240, 240, 240), Color::rgb(0, 0, 0))
    };
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: rect(0.0, 0.0, 800.0, 200.0),
        color: bg,
    });
    for (row, title) in [
        "Untitled",
        "Example.rs",
        "A longer tab name",
        "日本語.txt",
        TEXT,
    ]
    .into_iter()
    .enumerate()
    {
        for column in 0..4 {
            list.push(DrawCommand::TextRun {
                at: Point {
                    x: Dip(8.0 + column as f32 * 180.33),
                    y: Dip(24.0 + row as f32 * 30.0),
                },
                text: title.into(),
                font: "Segoe UI".into(),
                font_size: Dip(13.0),
                color: ink,
            });
        }
    }
    // The same places as the top row in crates/qe-shell/tests/lists/light.txt.
    for (x, title, alpha) in [
        (16.0, "Tab 0", 255),
        (215.667, "Tab 1", 0x9e),
        (415.333, "Tab 2", 0x9e),
    ] {
        list.push(DrawCommand::TextRun {
            at: Point {
                x: Dip(x),
                y: Dip(180.0),
            },
            text: title.into(),
            font: "Segoe UI".into(),
            font_size: Dip(13.0),
            color: Color { a: alpha, ..ink },
        });
    }
    list.push(DrawCommand::Icon {
        at: Point {
            x: Dip(700.0),
            y: Dip(24.0),
        },
        icon_id: "E8BB".into(),
        font_size: Dip(10.0),
        color: ink,
    });
    list
}
/// Whole frames through the renderer, each with a new renderer.
fn renderer_thread(order: usize, out: &mut Vec<String>) -> Result<(), Failure> {
    for round in 0..3 {
        for (dpi, reference) in [(96u32, true), (144, true), (192, true), (144, false)] {
            let s = dpi as f32 / 96.0;
            let (w, h) = ((800.0 * s) as u32, (200.0 * s) as u32);
            for dark in if (order + round).is_multiple_of(2) {
                [false, true]
            } else {
                [true, false]
            } {
                let mut renderer = BitmapRenderer::new(w, h)?;
                renderer.set_dpi(dpi)?;
                if reference {
                    renderer.set_text_settings(TextSettings::reference())?;
                }
                let list = scene(dark);
                renderer.begin_frame()?;
                renderer.draw(&list)?;
                renderer.end_frame()?;
                let image = renderer.bitmap()?;
                // Runs of pixels, so that a difference can be placed afterward.
                let rows: Vec<String> = image
                    .bgra
                    .chunks(w as usize * 4)
                    .map(|row| format!("{:016x}", checksum(row)))
                    .collect();
                out.push(format!(
                    "dpi{dpi} ref{reference} dark{dark} {w}x{h} {}",
                    rows.join("")
                ));
            }
        }
    }
    Ok(())
}
fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)
    })
}
fn child(mode: &str, threads: usize) -> Result<(), Failure> {
    let barrier = Arc::new(Barrier::new(threads));
    let handles: Vec<_> = (0..threads)
        .map(|order| {
            let barrier = barrier.clone();
            let mode = mode.to_owned();
            thread::spawn(move || -> Result<Vec<String>, String> {
                barrier.wait();
                let mut out = Vec::new();
                let done = match mode.as_str() {
                    "raw" => raw_thread(order, false, &mut out),
                    "locked" => raw_thread(order, true, &mut out),
                    "mixed" if order % 2 == 1 => layout_thread(),
                    "mixed" => raw_thread(order, true, &mut out),
                    _ => renderer_thread(order, &mut out),
                };
                done.map_err(|e| e.0)?;
                Ok(out)
            })
        })
        .collect();
    for handle in handles {
        let lines = handle.join().map_err(|_| "a thread panicked")??;
        for line in lines {
            println!("{line}");
        }
    }
    Ok(())
}
fn parent(mode: &str, processes: usize, threads: usize) -> Result<(), Failure> {
    // The difference shows up only on a busy PC, so a run can make the PC busy itself.
    let spinners: usize = std::env::var("QE_STRESS_SPIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    for _ in 0..spinners {
        thread::spawn(|| {
            let mut n = 1u64;
            loop {
                n = std::hint::black_box(n.wrapping_mul(3).wrapping_add(1));
            }
        });
    }
    let exe = std::env::current_exe()?;
    // key -> answer -> how often it came.
    let mut seen: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    let parallel = 6;
    let mut done = 0;
    while done < processes {
        let batch: Vec<_> = (0..parallel.min(processes - done))
            .map(|_| {
                Command::new(&exe)
                    .args(["--child", mode, &threads.to_string()])
                    .stdout(Stdio::piped())
                    .spawn()
            })
            .collect::<Result<_, _>>()?;
        for mut one in batch {
            let stdout = one.stdout.take().ok_or("no output")?;
            for line in BufReader::new(stdout).lines() {
                let line = line?;
                let (key, answer) = line.rsplit_once(' ').ok_or("bad line")?;
                *seen
                    .entry(key.to_owned())
                    .or_default()
                    .entry(answer.to_owned())
                    .or_default() += 1;
            }
            if !one.wait()?.success() {
                return Err("a child failed".into());
            }
            done += 1;
        }
    }
    let unstable: Vec<_> = seen.iter().filter(|(_, a)| a.len() > 1).collect();
    println!(
        "{mode}: {processes} processes, {threads} threads each, {} keys, {} with more than one answer",
        seen.len(),
        unstable.len()
    );
    for (key, answers) in unstable.iter().take(12) {
        let mut sorted: Vec<_> = answers.iter().collect();
        sorted.sort_by_key(|a| std::cmp::Reverse(*a.1));
        let (common, rare) = (sorted[0].0.as_bytes(), sorted[1].0.as_bytes());
        let differing: Vec<usize> = common
            .chunks(2)
            .zip(rare.chunks(2))
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| i)
            .collect();
        println!(
            "  {key}: answers seen {:?} times, {} bytes differ, first at {:?}",
            sorted.iter().map(|a| *a.1).collect::<Vec<_>>(),
            differing.len(),
            &differing[..differing.len().min(9)]
        );
    }
    if unstable.is_empty() {
        Ok(())
    } else {
        Err("the output is not repeatable".into())
    }
}
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match words[..] {
        ["--child", mode, threads] => match threads.parse() {
            Ok(threads) => child(mode, threads),
            Err(e) => Err(Failure::from(e)),
        },
        [mode, processes, threads] => match (processes.parse(), threads.parse()) {
            (Ok(p), Ok(t)) => parent(mode, p, t),
            _ => Err("PROCESSES and THREADS must be numbers".into()),
        },
        _ => Err("use: repeat_stress raw|renderer PROCESSES THREADS".into()),
    };
    if let Err(e) = result {
        eprintln!("repeat_stress: {}", e.0);
        std::process::exit(1);
    }
}
