use qe_api::{ByteOffset, ViewContext};
use qe_editor::EditorView;
use qe_ime::TextStore;

#[test]
fn utf16_reads_and_insertion_keep_surrogates_and_crlf() {
    let mut view = EditorView::from_text("a😀\r\nb").unwrap();
    let mut ctx = ViewContext::default();
    let mut store = TextStore::new(&mut view, &mut ctx);
    assert_eq!(store.read(1..5).unwrap(), "😀\r\n");
    store.replace(5..5, "é").unwrap();
    assert_eq!(store.read(0..7).unwrap(), "a😀\r\néb");
    assert_eq!(store.selection().unwrap(), 6..6);
    assert_eq!(store.view().text().as_deref(), Some("a😀\r\néb"));
    assert_eq!(store.view().byte_to_utf16(ByteOffset(5)).unwrap().0, 3);
}

#[test]
fn read_only_refuses_edit() {
    let mut view = EditorView::from_text("abc").unwrap();
    view.set_prevent_editing(true);
    let mut ctx = ViewContext::default();
    let mut store = TextStore::new(&mut view, &mut ctx);
    assert!(store.replace(0..1, "x").is_err());
    assert_eq!(store.view().text().as_deref(), Some("abc"));
}

#[test]
fn nested_lock_refuses_sync_and_queues_async() {
    let mut view = EditorView::from_text("").unwrap();
    let mut ctx = ViewContext::default();
    let mut store = TextStore::new(&mut view, &mut ctx);
    assert_eq!(
        store.request_lock(qe_ime::LockKind::Read, true),
        qe_ime::LockReply::Granted
    );
    assert_eq!(
        store.request_lock(qe_ime::LockKind::ReadWrite, true),
        qe_ime::LockReply::SyncRefused
    );
    assert_eq!(
        store.request_lock(qe_ime::LockKind::ReadWrite, false),
        qe_ime::LockReply::AsyncQueued
    );
    assert_eq!(store.release_lock(), Some(qe_ime::LockKind::ReadWrite));
}

#[test]
fn composition_commit_and_cancel_have_different_text_effects() {
    let mut view = EditorView::from_text("ab").unwrap();
    let mut ctx = ViewContext::default();
    let mut store = TextStore::new(&mut view, &mut ctx);
    store.start_composition();
    store.update_composition("中", 1);
    assert_eq!(store.read(0..2).unwrap(), "ab");
    store.cancel_composition();
    assert_eq!(store.view().text().as_deref(), Some("ab"));
    store.start_composition();
    store.update_composition("😀", 2);
    store.commit_composition("😀");
    assert_eq!(store.view().text().as_deref(), Some("😀ab"));
}

#[test]
fn surrogate_middle_is_rejected_and_empty_document_reads() {
    let mut view = EditorView::from_text("😀").unwrap();
    let mut ctx = ViewContext::default();
    let store = TextStore::new(&mut view, &mut ctx);
    assert_eq!(store.read(0..0).unwrap(), "");
    assert_eq!(store.read(1..2), Err(qe_ime::StoreError::InvalidPosition));
}

#[test]
fn geometry_reports_not_laid_out() {
    let mut view = EditorView::from_text("abc").unwrap();
    let mut ctx = ViewContext::default();
    let store = TextStore::new(&mut view, &mut ctx);
    assert!(matches!(
        store.caret_screen_rect(|r| r),
        Err(qe_ime::StoreError::NotLaidOut)
    ));
}
