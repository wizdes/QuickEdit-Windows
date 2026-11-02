//! Real measurements on real files. These are slow and write big temporary files, so they
//! run only by name, in release:
//! `cargo test -p qe-a11y --release --locked --test measure -- --ignored --nocapture --test-threads=1`
//! A frame is 16 ms. Each test fails if a provider call's median goes over that.
mod support;
use qe_api::{ByteOffset, ViewContext};
use qe_editor::{EditorView, FileLoad, LoadOptions, LoadResult, LoadedFile};
use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use support::*;
use windows::{
    Win32::{
        System::{Com::SAFEARRAY, Ole::SafeArrayGetElement},
        UI::Accessibility::*,
    },
    core::{BSTR, IUnknown, Interface},
};

const FRAME_MS: f64 = 16.0;
const NEEDLE: &str = "zq-needle-at-the-end";

struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0) {
            eprintln!("could not delete {}: {error}", self.0.display());
        }
    }
}

/// Lines of plain text with an emoji and a mark now and then, and one needle near the end.
fn write_file(name: &str, bytes: u64) -> TempFile {
    let path = std::env::temp_dir().join(format!("qe-a11y-{}-{name}.txt", std::process::id()));
    let mut out =
        std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&path).expect("create"));
    let line =
        "The quick brown fox jumps over the lazy dog, a\u{1f600}e\u{301} and 0123456789 words\r\n";
    let tail = format!("{NEEDLE}\r\n");
    let mut written = 0u64;
    while written + (line.len() + tail.len()) as u64 <= bytes {
        out.write_all(line.as_bytes()).expect("write");
        written += line.len() as u64;
    }
    out.write_all(tail.as_bytes()).expect("write");
    written += tail.len() as u64;
    while written < bytes {
        out.write_all(b"\n").expect("write");
        written += 1;
    }
    out.flush().expect("flush");
    TempFile(path)
}

fn load(file: &TempFile, read_only: bool) -> (EditorView, f64) {
    let began = Instant::now();
    let mut load = FileLoad::start(
        &file.0,
        LoadOptions {
            force_read_only: read_only,
            ..Default::default()
        },
        Arc::new(|| {}),
    )
    .expect("start the load");
    let deadline = began + Duration::from_secs(300);
    let loaded = loop {
        match load.poll() {
            Some(LoadResult::Loaded(file)) => break file,
            Some(LoadResult::Failed(reason)) => panic!("the load failed: {reason}"),
            Some(LoadResult::Cancelled) => panic!("the load was cancelled"),
            None => {
                assert!(Instant::now() < deadline, "the load timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    let view = match loaded {
        LoadedFile::ReadOnly(mapped) => EditorView::from_mapped(mapped).expect("a mapped view"),
        LoadedFile::Editable { store, .. } => EditorView::from_store(store).expect("a view"),
    };
    (view, began.elapsed().as_secs_f64() * 1000.0)
}

fn wait_for_index(view: &EditorView) -> f64 {
    let began = Instant::now();
    let deadline = began + Duration::from_secs(300);
    while !view.store_status().complete {
        assert!(Instant::now() < deadline, "the index timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    began.elapsed().as_secs_f64() * 1000.0
}

struct Row {
    name: String,
    median: f64,
    worst: f64,
}

fn time(rows: &mut Vec<Row>, name: &str, runs: usize, mut call: impl FnMut()) {
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        let began = Instant::now();
        call();
        samples.push(began.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    rows.push(Row {
        name: name.into(),
        median: samples[samples.len() / 2],
        worst: samples[samples.len() - 1],
    });
}

/// "Not yet" is the right answer from a page out of memory, so a timing loop keeps going.
fn not_yet_is_fine(error: windows::core::Error) -> windows::core::Result<()> {
    if error.code().0 == NOT_YET {
        NOT_YET_ANSWERS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    } else {
        Err(error)
    }
}
static NOT_YET_ANSWERS: AtomicUsize = AtomicUsize::new(0);

fn first_range(array: *mut SAFEARRAY) -> ITextRangeProvider {
    let mut raw: *mut std::ffi::c_void = std::ptr::null_mut();
    let index = 0i32;
    unsafe { SafeArrayGetElement(array, &index, (&raw mut raw).cast()) }.expect("element");
    unsafe { IUnknown::from_raw(raw) }.cast().expect("a range")
}

/// A caret range at this byte, placed the way a client finds one: through the selection.
fn caret_at(
    window: &TestWindow,
    document: &IRawElementProviderSimple,
    byte: usize,
) -> ITextRangeProvider {
    window.ui.view.borrow_mut().select(
        ByteOffset(byte)..ByteOffset(byte),
        &mut ViewContext::default(),
    );
    let text = text_of(document)
        .cast::<ITextProvider>()
        .expect("the text provider");
    first_range(unsafe { text.GetSelection() }.expect("GetSelection"))
}

/// One FindText, as UIA calls it. Gives whether it found the text, how long the answer took,
/// and the longest the window's message loop was held up meanwhile.
/// What one search answered. `None` means it hit its limit, which is an honest answer on a big
/// file or a busy PC, and the row shows it.
fn find(range: &ITextRangeProvider, query: &str, ignore_case: bool) -> (Option<bool>, f64, f64) {
    let moved = SendIt(range.clone());
    let query = query.to_owned();
    let (say, heard) = std::sync::mpsc::channel();
    let began = Instant::now();
    std::thread::spawn(move || {
        let moved = moved;
        let answer = unsafe {
            moved
                .0
                .FindText(&BSTR::from(query.as_str()), false, ignore_case)
        };
        let summary = match answer {
            Ok(_) => Ok(Some(true)),
            Err(error) if error.code().0 == 0 => Ok(Some(false)),
            Err(error) if error.code().0 == NOT_YET => Ok(None),
            Err(error) => Err(error.to_string()),
        };
        say.send(summary).expect("report");
    });
    let mut stall = 0f64;
    loop {
        let turn = Instant::now();
        pump();
        stall = stall.max(turn.elapsed().as_secs_f64() * 1000.0);
        if let Ok(answer) = heard.try_recv() {
            let found = answer.expect("FindText failed");
            return (found, began.elapsed().as_secs_f64() * 1000.0, stall);
        }
        std::thread::yield_now();
    }
}

fn get_text_settled(range: &ITextRangeProvider, limit: i32) -> (usize, usize) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut calls = 0;
    loop {
        calls += 1;
        match unsafe { range.GetText(limit) } {
            Ok(text) => return (text.len(), calls),
            Err(error) if error.code().0 == NOT_YET => {
                assert!(Instant::now() < deadline, "the text never came into memory");
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("GetText failed: {error}"),
        }
    }
}

fn measure(label: &str, window: &TestWindow, bytes: usize) {
    keep_the_real_move_limit();
    let ui = window.accessibility();
    let document = ui.document().expect("the document");
    let doc = document_range(&document);
    let text = text_of(&document);
    let basic: ITextProvider = text.cast().expect("the text provider");
    // A page out of memory says "not yet" and starts its read, so this asks until it comes.
    let deadline = Instant::now() + Duration::from_secs(30);
    let middle_byte = loop {
        let view = window.ui.view.borrow();
        let found = view
            .line_of_byte(ByteOffset(bytes / 2))
            .and_then(|line| view.line_range(line));
        match found {
            Ok(range) => break range.start.0,
            Err(_) if Instant::now() < deadline => {
                drop(view);
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("the middle line never came: {error:?}"),
        }
    };
    let middle = caret_at(window, &document, middle_byte);
    let end = caret_at(window, &document, bytes);
    window
        .ui
        .view
        .borrow_mut()
        .select(ByteOffset(0)..ByteOffset(0), &mut ViewContext::default());
    // Warm the middle page, so these rows time the provider and not the first disk read.
    let word = unsafe { middle.Clone() }.expect("Clone");
    let deadline = Instant::now() + Duration::from_secs(30);
    while let Err(error) = unsafe { word.ExpandToEnclosingUnit(TextUnit_Line) } {
        assert_eq!(error.code().0, NOT_YET, "{error}");
        assert!(Instant::now() < deadline, "the middle page never came");
        std::thread::sleep(Duration::from_millis(1));
    }
    let (first_read_units, first_read_calls) = get_text_settled(&word, 200);
    // A whole-cap read from the middle, as UIA calls it: one call, on another thread.
    let to_the_end = unsafe { middle.Clone() }.expect("Clone");
    unsafe {
        to_the_end.MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            &doc,
            TextPatternRangeEndpoint_End,
        )
    }
    .expect("to the end");
    let (long_units, long_ms, long_stall) = read_off_thread(&to_the_end);
    assert_eq!(long_units, qe_a11y::GET_TEXT_CAP, "the whole cap");

    let mut rows = Vec::new();
    let runs = 200;
    time(&mut rows, "DocumentRange", runs, || {
        unsafe { text.DocumentRange() }
            .map(drop)
            .or_else(not_yet_is_fine)
            .expect("a provider call");
    });
    time(&mut rows, "GetSelection", runs, || {
        unsafe { basic.GetSelection() }
            .map(drop)
            .or_else(not_yet_is_fine)
            .expect("a provider call");
    });
    time(&mut rows, "GetVisibleRanges", runs, || {
        unsafe { basic.GetVisibleRanges() }
            .map(drop)
            .or_else(not_yet_is_fine)
            .expect("a provider call");
    });
    time(&mut rows, "GetText(200) at the start", runs, || {
        unsafe { doc.GetText(200) }
            .map(drop)
            .or_else(not_yet_is_fine)
            .expect("a provider call");
    });
    time(&mut rows, "GetText(200) in the middle", runs, || {
        unsafe { word.GetText(200) }
            .map(drop)
            .or_else(not_yet_is_fine)
            .expect("a provider call");
    });
    for (name, unit) in [
        ("character", TextUnit_Character),
        ("word", TextUnit_Word),
        ("line", TextUnit_Line),
        ("paragraph", TextUnit_Paragraph),
    ] {
        let moving = unsafe { middle.Clone() }.expect("Clone");
        let mut way = 1;
        time(
            &mut rows,
            &format!("Move({name}) in the middle"),
            runs,
            || {
                unsafe { moving.Move(unit, way) }
                    .map(drop)
                    .or_else(not_yet_is_fine)
                    .expect("a provider call");
                way = -way;
            },
        );
        let expanding = unsafe { middle.Clone() }.expect("Clone");
        time(
            &mut rows,
            &format!("Expand({name}) in the middle"),
            runs,
            || {
                unsafe { expanding.ExpandToEnclosingUnit(unit) }
                    .map(drop)
                    .or_else(not_yet_is_fine)
                    .expect("a provider call");
            },
        );
    }
    let moving = unsafe { middle.Clone() }.expect("Clone");
    time(&mut rows, "Move(character, huge count)", 5, || {
        unsafe { moving.Move(TextUnit_Character, 1_000_000_000) }
            .map(drop)
            .or_else(not_yet_is_fine)
            .expect("a provider call");
    });
    let moving_words = unsafe { middle.Clone() }.expect("Clone");
    time(&mut rows, "Move(word, huge count)", 5, || {
        unsafe { moving_words.Move(TextUnit_Word, 1_000_000_000) }
            .map(drop)
            .or_else(not_yet_is_fine)
            .expect("a provider call");
    });
    time(&mut rows, "CompareEndpoints", runs, || {
        unsafe {
            end.CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                &doc,
                TextPatternRangeEndpoint_Start,
            )
        }
        .map(drop)
        .or_else(not_yet_is_fine)
        .expect("a provider call");
    });
    time(
        &mut rows,
        "GetBoundingRectangles on the whole document",
        runs,
        || {
            unsafe { doc.GetBoundingRectangles() }
                .map(drop)
                .or_else(not_yet_is_fine)
                .expect("a provider call");
        },
    );
    time(&mut rows, "GetAttributeValue(FontName)", runs, || {
        unsafe { doc.GetAttributeValue(UIA_FontNameAttributeId) }
            .map(drop)
            .or_else(not_yet_is_fine)
            .expect("a provider call");
    });

    println!("\n{label}: {bytes} bytes");
    println!(
        "  first GetText(200) in the middle: {first_read_units} units after {first_read_calls} calls"
    );
    println!(
        "  GetText(-1) from the middle, one call: {long_units} units, answer after {long_ms:.1} ms, window held up {long_stall:.3} ms at most"
    );
    println!(
        "  'not yet' answers so far in this process: {}",
        NOT_YET_ANSWERS.load(Ordering::Relaxed)
    );
    for row in &rows {
        println!(
            "  {:48} median {:9.4} ms   worst {:9.4} ms",
            row.name, row.median, row.worst
        );
    }
    for row in &rows {
        assert!(
            row.median < FRAME_MS,
            "{label}: {} took {} ms, over a frame",
            row.name,
            row.median
        );
    }
    for (name, query, ignore_case, should_find) in [
        ("near the start", "quick", false, true),
        ("far, exact case", NEEDLE, false, true),
        ("far, ignoring case", "ZQ-NEEDLE-AT-THE-END", true, true),
        ("absent, exact case", "this text is not there", false, false),
        (
            "absent, ignoring case",
            "THIS TEXT IS NOT THERE",
            true,
            false,
        ),
    ] {
        let (found, total, stall) = find(&doc, query, ignore_case);
        println!(
            "  FindText {name:24} found {found:?} (None is the limit), one call, answer after {total:.1} ms, window held up {stall:.3} ms at most"
        );
        if let Some(found) = found {
            assert_eq!(found, should_find, "{label}: {name}");
        }
    }
    drop(ui);
}

#[test]
#[ignore = "writes a 512 MiB file. Run by name, in release."]
fn measure_512_mib_file_in_memory() {
    let bytes = 512 * 1024 * 1024;
    let file = write_file("512", bytes as u64);
    let (view, load_ms) = load(&file, false);
    println!("512 MiB editable load: {load_ms:.0} ms");
    let window = TestWindow::new(view);
    window.lay_out(800.0, 600.0);
    measure("512 MiB, in memory, editable", &window, bytes);
    window.close();
}

#[test]
#[ignore = "writes a 512 MiB file. Run by name, in release."]
fn measure_512_mib_file_mapped() {
    let bytes = 512 * 1024 * 1024;
    let file = write_file("512m", bytes as u64);
    let (view, load_ms) = load(&file, true);
    let index_ms = wait_for_index(&view);
    println!("512 MiB mapped: open {load_ms:.0} ms, index {index_ms:.0} ms");
    let window = TestWindow::new(view);
    window.lay_out(800.0, 600.0);
    measure("512 MiB, mapped, read-only", &window, bytes);
    window.close();
}

#[test]
#[ignore = "writes a 1 GiB file. Run by name, in release."]
fn measure_1_gib_mapped_file() {
    let bytes = 1024 * 1024 * 1024;
    let file = write_file("1g", bytes as u64);
    let (view, load_ms) = load(&file, true);
    let window = TestWindow::new(view);
    // While the index grows: calls answer from what is known, or say to ask again.
    let ui = window.accessibility();
    let document = ui.document().expect("the document");
    let mut during = 0;
    let first_revision = window.ui.view.borrow().revision();
    let mut worst = 0f64;

    let mut partial = 0;
    let began = Instant::now();
    while !window.ui.view.borrow().store_status().complete && began.elapsed().as_secs() < 300 {
        let call = Instant::now();
        let doc = unsafe { text_of(&document).DocumentRange() }.expect("DocumentRange");
        match unsafe { doc.GetText(100) } {
            Ok(text) if !text.is_empty() => partial += 1,
            Ok(_) => {}
            Err(error) => assert_eq!(error.code().0, NOT_YET, "{error}"),
        }
        worst = worst.max(call.elapsed().as_secs_f64() * 1000.0);
        during += 1;
        std::thread::sleep(Duration::from_millis(1));
    }
    println!(
        "1 GiB mapped: open {load_ms:.0} ms, {during} calls during indexing, {partial} with text, worst {worst:.3} ms, revision {first_revision} then {}, index done after {:.0} ms",
        window.ui.view.borrow().revision(),
        began.elapsed().as_secs_f64() * 1000.0
    );
    assert!(worst < FRAME_MS, "a call during indexing took {worst} ms");
    drop(ui);
    window.lay_out(800.0, 600.0);
    measure("1 GiB, mapped, read-only", &window, bytes);
    window.close();
}

/// One GetText(-1), as UIA calls it. Gives its length, how long the answer took, and the longest
/// the window's message loop was held up meanwhile.
fn read_off_thread(range: &ITextRangeProvider) -> (usize, f64, f64) {
    let moved = SendIt(range.clone());
    let (say, heard) = std::sync::mpsc::channel();
    let began = Instant::now();
    std::thread::spawn(move || {
        let moved = moved;
        let answer = unsafe { moved.0.GetText(-1) }.map(|text| text.len());
        say.send(answer.map_err(|error| error.to_string()))
            .expect("report");
    });
    let mut stall = 0f64;
    loop {
        let turn = Instant::now();
        pump();
        stall = stall.max(turn.elapsed().as_secs_f64() * 1000.0);
        if let Ok(answer) = heard.try_recv() {
            let units = answer.expect("GetText failed");
            return (units, began.elapsed().as_secs_f64() * 1000.0, stall);
        }
        std::thread::yield_now();
    }
}
