use qe_a11y::provider::DocumentProvider;
use qe_editor::{EditorView, FileLoad, LoadOptions, LoadResult, LoadedFile};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use windows::{
    Win32::UI::Accessibility::{ITextProvider2, UIA_TextPatternId},
    core::Interface,
};

#[test]
fn actual_mapped_file_is_read_only_and_returns_text() {
    let path = std::env::temp_dir().join(format!(
        "qe-a11y-{}-{}.txt",
        std::process::id(),
        std::thread::current().name().unwrap_or("mapped")
    ));
    std::fs::write(&path, "a😀\r\nb").unwrap();
    let mut load = FileLoad::start(
        &path,
        LoadOptions {
            force_read_only: true,
            ..Default::default()
        },
        Arc::new(|| {}),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let file = loop {
        match load.poll() {
            Some(LoadResult::Loaded(file)) => break file,
            Some(LoadResult::Failed(reason)) => panic!("mapped load failed: {reason}"),
            Some(LoadResult::Cancelled) => panic!("mapped load cancelled"),
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            None => panic!("mapped load timed out"),
        }
    };
    let LoadedFile::ReadOnly(mapped) = file else {
        panic!("force_read_only did not map the file")
    };
    let view = EditorView::from_mapped(mapped).unwrap();
    while !view.store_status().complete && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(view.store_status().complete, "mapped index timed out");
    let provider = DocumentProvider::from_editor(&view).unwrap();
    let text: ITextProvider2 = unsafe { provider.simple().GetPatternProvider(UIA_TextPatternId) }
        .unwrap()
        .cast()
        .unwrap();
    let range = unsafe { text.DocumentRange() }.unwrap();
    assert_eq!(
        unsafe { range.GetText(-1) }.unwrap().to_string(),
        "a😀\r\nb"
    );
    drop(range);
    drop(text);
    drop(provider);
    drop(view);
    std::fs::remove_file(path).unwrap();
}
