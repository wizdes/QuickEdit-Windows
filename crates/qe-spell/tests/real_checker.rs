//! Windows' real `ISpellChecker`. Each test says why it skipped when this account has no
//! dictionary, and root runs it where one is present. Includes the loaded-module proof, and
//! a saved picture of the red underlines through `qe-render`.
mod support;

use qe_api::{ByteOffset, Color, DrawCommand, Renderer, Theme, ThemeKind, View, ViewContext};
use qe_editor::{EditorCommand, EditorView};
use qe_layout::DirectWriteLayout;
use qe_render::{BitmapRenderer, TextSettings};
use qe_spell::{SpellOff, SpellService, WindowsChecker};
use std::{
    process::Command,
    time::{Duration, Instant},
};
use support::{area, finish, laid_out, marks, no_wake, settle};
use windows::Win32::{
    Foundation::HMODULE,
    System::{
        Com::{COINIT_MULTITHREADED, CoGetApartmentType, CoInitializeEx, CoUninitialize},
        ProcessStatus::{EnumProcessModules, GetModuleBaseNameW},
        Threading::GetCurrentProcess,
    },
};

const DLL: &str = "msspellcheckingfacility.dll";
const CHILD: &str = "QE_SPELL_MODULES_CHILD";

fn loaded_modules() -> Vec<String> {
    let mut modules = vec![HMODULE::default(); 4096];
    let mut needed = 0;
    unsafe {
        let process = GetCurrentProcess();
        EnumProcessModules(
            process,
            modules.as_mut_ptr(),
            std::mem::size_of_val(modules.as_slice()) as u32,
            &mut needed,
        )
        .unwrap();
        let count = needed as usize / size_of::<HMODULE>();
        modules[..count]
            .iter()
            .map(|module| {
                let mut name = [0_u16; 260];
                let length = GetModuleBaseNameW(process, Some(*module), &mut name);
                String::from_utf16_lossy(&name[..length as usize]).to_ascii_lowercase()
            })
            .collect()
    }
}

/// `true` when Windows has a dictionary. Otherwise it says why the test skipped.
fn have_english(test: &str) -> bool {
    let answer = std::thread::spawn(|| {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .unwrap();
        let answer = WindowsChecker::is_supported("en-US");
        unsafe { CoUninitialize() };
        answer
    })
    .join()
    .unwrap();
    match answer {
        Ok(true) => true,
        Ok(false) => {
            eprintln!("SKIPPED {test}: this account has no en-US dictionary. Root runs it.");
            false
        }
        Err(e) => {
            eprintln!("SKIPPED {test}: ISpellChecker is not available here: {e}. Root runs it.");
            false
        }
    }
}

fn real_service(view: &EditorView) -> SpellService {
    SpellService::start(view, true, Some("en-US".into()), no_wake()).unwrap()
}

// ---- Nothing loads while spelling is off ----

#[test]
fn child_process_body() {
    let Ok(mode) = std::env::var(CHILD) else {
        return;
    };
    // COM unloads the DLL when the last thread leaves the multithreaded apartment. So a thread
    // stays in it, and a load that came and went still shows in the list at the end.
    let (stay, stay_until) = std::sync::mpsc::channel::<()>();
    let (ready, is_ready) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .unwrap();
        ready.send(()).unwrap();
        let _ = stay_until.recv();
    });
    is_ready.recv().unwrap();
    let mut view = laid_out("speling here");
    let mut ctx = ViewContext::default();
    match mode.as_str() {
        "off" => {
            // Each thing a host does with spelling off. None may load the DLL.
            assert!(matches!(
                SpellService::start(&view, false, None, no_wake()),
                Err(SpellOff::NotOn)
            ));
            assert_eq!(qe_spell::why_off(&view, false), Some(SpellOff::NotOn));
            view.replace_range(ByteOffset(0)..ByteOffset(0), "x", &mut ctx);
            view.run(EditorCommand::Undo, &mut ctx);
            std::thread::sleep(Duration::from_millis(50));
        }
        "on" => {
            let mut service = real_service(&view);
            service.refresh(&mut view, &mut ctx);
            finish(&mut service, &mut view, &mut ctx);
        }
        // The check itself: load the DLL by hand, and see that the list shows it.
        "by-hand" => {
            std::thread::spawn(|| {
                unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                    .ok()
                    .ok();
                WindowsChecker::is_supported("en-US").unwrap();
                println!("MODULES {}", loaded_modules().join(" "));
            })
            .join()
            .unwrap();
        }
        other => panic!("{other}"),
    }
    println!("MODULES {}", loaded_modules().join(" "));
    println!("CHILD_DONE");
    drop(stay);
}

fn child(mode: &str) -> String {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_process_body",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, mode)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success() && text.contains("CHILD_DONE"),
        "the child failed: {text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    text
}

#[test]
fn with_spelling_off_the_spelling_dll_does_not_load_and_the_check_can_see_it_when_it_does() {
    let off = child("off");
    assert!(off.contains("MODULES "), "{off}");
    assert!(
        !off.contains(DLL),
        "the DLL loaded with spelling off: {off}"
    );
    if !have_english("the half of the module test that loads the DLL") {
        return;
    }
    // A loaded DLL shows in the same list, so the line above means something.
    let by_hand = child("by-hand");
    assert!(
        by_hand.contains(DLL),
        "the list did not show the DLL: {by_hand}"
    );
    let on = child("on");
    assert!(on.contains(DLL), "spelling on did not load the DLL: {on}");
}

// ---- The real checker ----

#[test]
fn the_real_checker_marks_a_word_and_suggests_the_right_one() {
    if !have_english("the_real_checker_marks_a_word_and_suggests_the_right_one") {
        return;
    }
    let text = "Teh quick \u{1f600} speling\r\nof cafe\u{301} works";
    let mut view = laid_out(text);
    let mut service = real_service(&view);
    let mut ctx = ViewContext::default();
    service.refresh(&mut view, &mut ctx);
    finish(&mut service, &mut view, &mut ctx);
    let start = text.find("speling").unwrap();
    assert!(
        marks(&view).contains(&(start, start + 7)),
        "{:?}",
        service.marks()
    );
    let menu = service.menu_at(&view, ByteOffset(start + 2)).unwrap();
    assert_eq!(menu.word, "speling");
    assert!(menu.suggestions.len() <= 5);
    assert!(
        menu.suggestions.iter().any(|s| s == "spelling"),
        "{:?}",
        menu.suggestions
    );
    service
        .apply_fix(&mut view, &menu, "spelling", &mut ctx)
        .unwrap();
    assert!(view.text().unwrap().contains("spelling"));
    finish(&mut service, &mut view, &mut ctx);
    let start = view.text().unwrap().find("spelling").unwrap();
    assert!(
        !marks(&view).contains(&(start, start + 8)),
        "the fix cleared its mark"
    );
    view.run(EditorCommand::Undo, &mut ctx);
    assert_eq!(view.text().unwrap(), text, "the fix is one undo step");
}

#[test]
fn the_real_checker_leaves_urls_emails_and_doubled_words_alone() {
    if !have_english("the_real_checker_leaves_urls_emails_and_doubled_words_alone") {
        return;
    }
    let text = "http://speling.example/speling speling@example.com www.speling.com \
                the the okay";
    let mut view = laid_out(text);
    let mut service = real_service(&view);
    let mut ctx = ViewContext::default();
    service.refresh(&mut view, &mut ctx);
    finish(&mut service, &mut view, &mut ctx);
    assert_eq!(
        marks(&view),
        vec![],
        "{:?}",
        service.marks().iter().map(|m| &m.word).collect::<Vec<_>>()
    );
}

#[test]
fn ignore_on_the_real_checker_lasts_and_touches_no_dictionary_file() {
    if !have_english("ignore_on_the_real_checker_lasts_and_touches_no_dictionary_file") {
        return;
    }
    let word = "qzxvlorpt";
    let mut view = laid_out(&format!("{word} and {word}"));
    let mut service = real_service(&view);
    let mut ctx = ViewContext::default();
    service.refresh(&mut view, &mut ctx);
    finish(&mut service, &mut view, &mut ctx);
    assert_eq!(marks(&view).len(), 2);
    let menu = service.menu_at(&view, ByteOffset(0)).unwrap();
    service.ignore(&mut view, &menu, &mut ctx).unwrap();
    assert_eq!(marks(&view), vec![]);
    // A recheck of the word asks Windows again, and this document's ignore list holds.
    view.replace_range(ByteOffset(9)..ByteOffset(9), " ", &mut ctx);
    service.refresh(&mut view, &mut ctx);
    finish(&mut service, &mut view, &mut ctx);
    assert_eq!(marks(&view), vec![]);
}

/// Writes to the user's own dictionary file, so it does not run by default. Root can run it
/// with `--ignored`, and then remove the word from Settings or from `default.dic`.
#[test]
#[ignore = "writes to this account's dictionary at %APPDATA%\\Microsoft\\Spelling"]
fn add_on_the_real_checker_lasts() {
    if !have_english("add_on_the_real_checker_lasts") {
        return;
    }
    let word = "qzxvaddtest";
    let mut view = laid_out(word);
    let mut service = real_service(&view);
    let mut ctx = ViewContext::default();
    service.refresh(&mut view, &mut ctx);
    finish(&mut service, &mut view, &mut ctx);
    let menu = service.menu_at(&view, ByteOffset(0)).unwrap();
    service
        .add_to_dictionary(&mut view, &menu, &mut ctx)
        .unwrap();
    drop(service);
    let mut service = real_service(&view);
    service.refresh(&mut view, &mut ctx);
    finish(&mut service, &mut view, &mut ctx);
    assert_eq!(marks(&view), vec![], "a new checker knows the word");
}

#[test]
fn a_language_with_no_dictionary_says_so_in_plain_words() {
    let mut view = laid_out("speling");
    let mut service = SpellService::start(&view, true, Some("zz-ZZ".into()), no_wake()).unwrap();
    let mut ctx = ViewContext::default();
    service.refresh(&mut view, &mut ctx);
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        match service.poll(&mut view, &mut ctx) {
            Err(message) => {
                assert!(
                    message.contains("zz-ZZ") || message.contains("not available"),
                    "{message}"
                );
                return;
            }
            Ok(_) => {
                assert!(Instant::now() < until, "no failure came");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

#[test]
fn real_marks_from_a_big_document_stay_under_the_key_budget_in_release() {
    if !have_english("real_marks_from_a_big_document_stay_under_the_key_budget_in_release") {
        return;
    }
    let text = "plain words and a speling here\n".repeat(200_000);
    let mut view = laid_out(&text);
    let woken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = woken.clone();
    let mut service = SpellService::start(
        &view,
        true,
        Some("en-US".into()),
        std::sync::Arc::new(move || flag.store(true, std::sync::atomic::Ordering::Release)),
    )
    .unwrap();
    let mut ctx = ViewContext::default();
    let first_refresh = Instant::now();
    service.refresh(&mut view, &mut ctx);
    let refresh_time = first_refresh.elapsed();
    let first_check = Instant::now();
    finish(&mut service, &mut view, &mut ctx);
    let check_time = first_check.elapsed();
    // Scrolling through the document gathers marks, and the work of a key grows with them.
    let mut scroll = 0.0;
    while service.marks().len() < 5_000 {
        scroll += 300.0;
        view.set_scroll_position(scroll, &mut ctx);
        settle(&mut view);
        service.refresh(&mut view, &mut ctx);
        finish(&mut service, &mut view, &mut ctx);
    }
    let marks_held = service.marks().len();
    view.set_scroll_position(0.0, &mut ctx);
    settle(&mut view);
    service.refresh(&mut view, &mut ctx);
    finish(&mut service, &mut view, &mut ctx);
    let asked_before = service.counts().bytes_asked;
    woken.store(false, std::sync::atomic::Ordering::Release);
    let mut keys = Vec::new();
    const KEYS: usize = 200;
    for at in 0..KEYS {
        view.replace_range(ByteOffset(at)..ByteOffset(at), "x", &mut ctx);
        ctx.requests.clear();
        let mut spent = Duration::ZERO;
        let key = Instant::now();
        service.refresh(&mut view, &mut ctx);
        service.poll(&mut view, &mut ctx).unwrap();
        spent += key.elapsed();
        // The answer's own poll is where the marks are set, so it counts as this key's work.
        let until = Instant::now() + Duration::from_secs(30);
        while service.is_working() {
            if woken.swap(false, std::sync::atomic::Ordering::Acquire) {
                let poll = Instant::now();
                service.poll(&mut view, &mut ctx).unwrap();
                spent += poll.elapsed();
            } else {
                std::thread::yield_now();
            }
            assert!(Instant::now() < until, "the worker did not finish");
        }
        keys.push(spent);
        ctx.requests.clear();
    }
    finish(&mut service, &mut view, &mut ctx);
    keys.sort();
    let mean = keys.iter().sum::<Duration>() / KEYS as u32;
    let p99 = keys[KEYS * 99 / 100];
    println!(
        "MEASURE document={} MB first refresh={:?}, first check on the worker={:?}, marks held={marks_held}, \
         spelling calls on a key: mean={mean:?}, p99={p99:?}, worst={:?}, keys={KEYS}, bytes asked in all={}",
        text.len() / 1_000_000,
        refresh_time,
        check_time,
        keys[KEYS - 1],
        service.counts().bytes_asked - asked_before,
    );
    // The plan's budget for a key is p99 under 2 ms. A debug build says nothing about it.
    if !cfg!(debug_assertions) {
        assert!(p99 < Duration::from_millis(2), "p99 {p99:?}");
    }
    // Each key may ask only for its own word. A whole-document check would be 6 MB.
    assert!(
        service.counts().bytes_asked - asked_before < 200_000,
        "{:?}",
        service.counts()
    );
}

// ---- Tabs that come and go ----

/// Each tab's service has its own worker. When the last worker left the COM apartment, COM
/// shut the apartment down, and a checker starting at that moment failed with 0x80080008.
#[test]
fn services_that_stop_and_start_one_after_another_all_work() {
    if !have_english("services_that_stop_and_start_one_after_another_all_work") {
        return;
    }
    // Few workers at a time, and short gaps, so that the apartment often empties just as
    // another worker joins it.
    let threads: Vec<_> = (0..3_u64)
        .map(|index| {
            std::thread::spawn(move || -> Result<(), String> {
                let mut view = laid_out("speling");
                let mut ctx = ViewContext::default();
                for round in 0..150_u64 {
                    let mut service = real_service(&view);
                    service.refresh(&mut view, &mut ctx);
                    let until = Instant::now() + Duration::from_secs(60);
                    while service.is_working() {
                        service
                            .poll(&mut view, &mut ctx)
                            .map_err(|e| format!("round {round}: {e}"))?;
                        if Instant::now() > until {
                            return Err(format!("round {round}: no answer"));
                        }
                        std::thread::yield_now();
                    }
                    drop(service);
                    std::thread::sleep(Duration::from_micros(
                        200 + (index * 317 + round * 53) % 1500,
                    ));
                }
                Ok(())
            })
        })
        .collect();
    for thread in threads {
        assert_eq!(thread.join().unwrap(), Ok(()));
    }
}

// ---- The picture ----

fn is_underline(pixel: &[u8], color: Color) -> bool {
    // The same mix of colors at any strength, over white. Text is gray, so it never matches.
    let strength = |paper: u8, ink: u8, have: u8| {
        (f32::from(paper) - f32::from(have)) / (f32::from(paper) - f32::from(ink)).max(1.0)
    };
    let (r, g, b) = (
        strength(255, color.r, pixel[2]),
        strength(255, color.g, pixel[1]),
        strength(255, color.b, pixel[0]),
    );
    pixel[2] > pixel[1].saturating_add(40)
        && pixel[2] > pixel[0].saturating_add(40)
        && (r - g).abs() < 0.35
        && (g - b).abs() < 0.35
}

#[test]
fn red_underlines_show_in_a_saved_picture_through_qe_render() {
    let text = "This line has a speling error.\r\nThis one is fine.\r\nTeh \u{1f600} combo cafe\u{301} wrongg";
    let measure = DirectWriteLayout::with_font(
        "Consolas",
        14.0,
        windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL,
    )
    .unwrap();
    let mut view = EditorView::from_text(text).unwrap();
    view.set_font("Consolas", 14.0).unwrap();
    view.layout(area(520.0, 120.0), &measure);
    settle(&mut view);
    let mut ctx = ViewContext::default();
    // The real checker when it is here. A fake gives the same marks when it is not, and
    // the test says so, because this test is about the painting.
    let real = have_english("the real-checker half of the picture test");
    let mut service = if real {
        real_service(&view)
    } else {
        support::fake_service(&view, &["speling", "Teh", "wrongg"]).0
    };
    service.refresh(&mut view, &mut ctx);
    finish(&mut service, &mut view, &mut ctx);
    let start = text.find("speling").unwrap();
    assert!(
        marks(&view).contains(&(start, start + 7)),
        "{:?}",
        marks(&view)
    );

    let theme = Theme::new(ThemeKind::Light);
    let mut list = qe_api::DisplayList::default();
    view.draw(&mut list, &theme, &measure);
    let fonts: Vec<u32> = list
        .commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::GlyphRun { font_id, .. } => Some(*font_id),
            _ => None,
        })
        .collect();
    let mut renderer = BitmapRenderer::new(520, 120).unwrap();
    renderer
        .set_text_settings(TextSettings::reference())
        .unwrap();
    for id in &fonts {
        renderer.register_font_face(*id, &qe_layout::font_face(*id).unwrap());
    }
    let bitmap = renderer.render_to_bitmap(&list, 520, 120).unwrap();

    let dir = std::env::var_os("QE_SPELL_PICTURE_DIR")
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    let path = dir.join("qe-spell-underlines.bmp");
    let size = 54 + bitmap.bgra.len() as u32;
    let mut bytes = Vec::with_capacity(size as usize);
    bytes.extend_from_slice(b"BM");
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&54_u32.to_le_bytes());
    bytes.extend_from_slice(&40_u32.to_le_bytes());
    bytes.extend_from_slice(&(bitmap.width as i32).to_le_bytes());
    bytes.extend_from_slice(&(-(bitmap.height as i32)).to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&32_u16.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&(bitmap.bgra.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 16]);
    bytes.extend_from_slice(&bitmap.bgra);
    std::fs::write(&path, bytes).unwrap();
    println!("PICTURE {} (real checker: {real})", path.display());

    // Red under the misspelled word, and none on the clean line.
    let rect = view
        .range_rects(ByteOffset(start)..ByteOffset(start + 7))
        .unwrap()[0]
        .rect;
    let pixel = |x: usize, y: usize| {
        let at = (y * bitmap.width as usize + x) * 4;
        &bitmap.bgra[at..at + 4]
    };
    let color = theme.spelling_underline;
    let count_red = |x0: f32, x1: f32, y0: f32, y1: f32| {
        let mut count = 0;
        for y in (y0.max(0.0) as usize)..(y1 as usize).min(bitmap.height as usize) {
            for x in (x0.max(0.0) as usize)..(x1 as usize).min(bitmap.width as usize) {
                if is_underline(pixel(x, y), color) {
                    count += 1;
                }
            }
        }
        count
    };
    let (x0, x1) = (rect.x.0, rect.x.0 + rect.width.0);
    let under_word = count_red(
        x0,
        x1,
        rect.y.0 + rect.height.0 - 5.0,
        rect.y.0 + rect.height.0 + 2.0,
    );
    assert!(under_word > 10, "{under_word} red pixels under the word");
    // The word before it, on the same row.
    let clean = view.range_rects(ByteOffset(5)..ByteOffset(9)).unwrap()[0].rect;
    let under_clean = count_red(
        clean.x.0,
        clean.x.0 + clean.width.0,
        clean.y.0 + clean.height.0 - 5.0,
        clean.y.0 + clean.height.0 + 2.0,
    );
    assert_eq!(under_clean, 0, "red under a word that is spelled right");
}

#[test]
fn the_real_checker_refuses_text_with_no_end_mark() {
    if !have_english("the_real_checker_refuses_text_with_no_end_mark") {
        return;
    }
    let answer = std::thread::spawn(|| {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .unwrap();
        let answer = {
            let mut checker = WindowsChecker::new(Some("en-US")).unwrap();
            qe_spell::Checker::check(&mut checker, &[0x61, 0x62])
        };
        unsafe { CoUninitialize() };
        answer
    })
    .join()
    .unwrap();
    assert!(answer.is_err());
}

// ---- The apartment stays up ----

const MTA_CHILD: &str = "QE_SPELL_MTA_CHILD";

/// True when this process has a multithreaded apartment. A thread with no COM of its own gets
/// the answer "implicit MTA" then, and "not initialized" when there is none.
fn apartment_is_up() -> bool {
    let (mut kind, mut qualifier) = (Default::default(), Default::default());
    unsafe { CoGetApartmentType(&mut kind, &mut qualifier) }.is_ok()
}

#[test]
fn mta_child_body() {
    if std::env::var(MTA_CHILD).is_err() {
        return;
    }
    assert!(
        !apartment_is_up(),
        "this check needs a process with no apartment yet"
    );
    let view = EditorView::from_text("speling").unwrap();
    let (service, log) = support::fake_service(&view, &["speling"]);
    drop(service);
    let until = Instant::now() + Duration::from_secs(30);
    while !log.dropped.load(std::sync::atomic::Ordering::Relaxed) {
        assert!(Instant::now() < until, "the worker never ended");
        std::thread::sleep(Duration::from_millis(1));
    }
    // The worker leaves the apartment just after it lets go of the checker.
    let until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < until {
        assert!(apartment_is_up(), "the last worker took the apartment down");
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("MTA_CHILD_DONE");
}

#[test]
fn the_apartment_stays_up_when_the_last_worker_leaves() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "mta_child_body",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MTA_CHILD, "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success() && text.contains("MTA_CHILD_DONE"),
        "{text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
