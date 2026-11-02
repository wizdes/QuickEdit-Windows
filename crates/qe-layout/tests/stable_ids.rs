//! Finding D: saved display text must not depend on which font loaded first.
use qe_api::{Color, Dip, DisplayList, DrawCommand, Layout, LineIndex, testing::StringStore};
use qe_layout::{DirectWriteLayout, font_face};
use std::process::Command;
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;

const SAMPLES: [(&str, &str); 4] = [
    ("Cascadia Mono", "abc"),
    ("Consolas", "abc"),
    ("Segoe UI", "abc"),
    // Fallback fonts register as they are drawn.
    ("Cascadia Mono", "\u{6F22}\u{1F600}"),
];

fn stable_text(family: &str, sample: &str) -> String {
    let engine = DirectWriteLayout::with_font(family, 10.0, DWRITE_FONT_WEIGHT_NORMAL).unwrap();
    let layout = engine
        .layout_line(&StringStore::new(sample), LineIndex(0), Dip(1000.0))
        .unwrap();
    let mut list = DisplayList::default();
    for run in layout.rows.iter().flat_map(|r| &r.glyph_runs) {
        assert!(font_face(run.font_id).is_some());
        list.push(DrawCommand::GlyphRun {
            at: run.origin,
            bidi_level: run.bidi_level,
            font_id: run.font_id,
            font_em_size: run.font_em_size,
            glyph_ids: run.glyph_ids.clone(),
            advances: run.advances.clone(),
            offsets: run.offsets.clone(),
            color: Color::rgb(0, 0, 0),
        });
    }
    list.to_stable_text()
}

/// Run by the test below in a fresh process. It prints each sample's text.
#[test]
#[ignore = "run by each_load_order_prints_the_same_text in a fresh process"]
fn print_stable_text() {
    let order = std::env::var("QE_FONT_ORDER").unwrap();
    let on_worker = std::env::var_os("QE_FONT_ON_WORKER").is_some();
    // Load the fonts first, in the asked order, so that only registration order differs.
    for at in order.split(',') {
        let (family, sample) = SAMPLES[at.parse::<usize>().unwrap()];
        if on_worker {
            std::thread::spawn(move || stable_text(family, sample))
                .join()
                .unwrap();
        } else {
            stable_text(family, sample);
        }
    }
    for (family, sample) in SAMPLES {
        println!("TEXT {family} {:?}", stable_text(family, sample));
    }
}

fn fresh_process(order: &str, on_worker: bool) -> String {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", "print_stable_text", "--nocapture"])
        .env("QE_FONT_ORDER", order);
    if on_worker {
        command.env("QE_FONT_ON_WORKER", "1");
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{order}: {output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| line.contains("TEXT "))
        .map(|line| line[line.find("TEXT ").unwrap()..].to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn each_load_order_prints_the_same_text() {
    let first = fresh_process("0,1,2,3", false);
    assert_eq!(first.lines().count(), SAMPLES.len(), "{first}");
    assert_eq!(first, fresh_process("3,2,1,0", false));
    assert_eq!(first, fresh_process("1,3,0,2", false));
    assert_eq!(first, fresh_process("2,0,3,1", true));
}
