use qe_api::{ByteOffset, ViewContext};
use qe_editor::EditorView;
use qe_spell::{SpellService, word_is_candidate};
use windows::{Win32::System::LibraryLoader::GetModuleHandleW, core::w};

#[test]
fn candidate_rule_skips_urls_and_keeps_unicode_words() {
    assert!(!word_is_candidate("https://example.com/speling"));
    assert!(!word_is_candidate("example.com"));
    assert!(word_is_candidate("cafe\u{301}"));
    assert!(!word_is_candidate("😀"));
}

#[test]
fn service_is_off_for_large_files() {
    let before = unsafe { GetModuleHandleW(w!("MsSpellCheckingFacility.dll")) }.is_ok();
    let view = EditorView::from_text("speling").unwrap();
    assert!(
        SpellService::start(&view, false, "en-US", std::sync::Arc::new(|| {}))
            .unwrap()
            .is_none()
    );
    assert!(!SpellService::eligible(50 * 1024 * 1024 + 1, true));
    assert!(!SpellService::eligible(0, false));
    assert!(SpellService::eligible(0, true));
    let after = unsafe { GetModuleHandleW(w!("MsSpellCheckingFacility.dll")) }.is_ok();
    assert_eq!(
        after, before,
        "the spell checker DLL loaded while spelling stayed off"
    );
}

#[test]
fn applying_a_fix_is_one_editor_edit() {
    let mut view = EditorView::from_text("speling\r\n😀").unwrap();
    let mut ctx = ViewContext::default();
    qe_spell::apply_fix(
        &mut view,
        ByteOffset(0)..ByteOffset(7),
        "spelling",
        &mut ctx,
    )
    .unwrap();
    assert_eq!(view.text().as_deref(), Some("spelling\r\n😀"));
}
