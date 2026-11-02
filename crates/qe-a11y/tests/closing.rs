//! What closing leaves behind. These tests read `waiting_work`, which covers the whole process,
//! so they take turns.
mod support;
use qe_a11y::{UiThreadOnly, UiaHost, UiaSlot, WidgetAction};
use qe_api::{Widget, WidgetId, WidgetRole};
use qe_editor::EditorView;
use std::{
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
use support::*;
use windows::{
    Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::{Accessibility::*, WindowsAndMessaging::WM_GETOBJECT},
    },
    core::Interface,
};

/// Posted work that the window never reached ends at close, and does not wait in the table.
#[test]
fn closing_drops_posted_work_at_once() {
    let _one_at_a_time = one_ui_thread();
    let button = widget(2, WidgetRole::Button, "Button", 0.0, Vec::new());
    let window = TestWindow::with_tree(
        EditorView::from_text("abc").unwrap(),
        root_with(vec![button]),
    );
    let ui = window.accessibility();
    let root: IRawElementProviderFragment = ui.root().cast().unwrap();
    let node: IRawElementProviderSimple = unsafe { root.Navigate(NavigateDirection_FirstChild) }
        .unwrap()
        .cast()
        .unwrap();
    let invoke: IInvokeProvider = unsafe { node.GetPatternProvider(UIA_InvokePatternId) }
        .unwrap()
        .cast()
        .unwrap();
    unsafe { invoke.Invoke() }.unwrap();
    assert_eq!(qe_a11y::marshal::waiting_work(), 1, "the action waits");
    ui.shutdown().unwrap();
    assert_eq!(qe_a11y::marshal::waiting_work(), 0, "the action stayed");
    assert!(window.actions_after_pump().is_empty());
}

/// The shell's window state, with the slot inside, as a window procedure might reach it.
struct State {
    slot: UiaSlot,
    /// 0 before this ends, 1 if it ended with the slot finished, 2 if the slot was in use.
    ended: Arc<AtomicU8>,
}
impl Drop for State {
    fn drop(&mut self) {
        let finished = self.slot.current().is_none();
        self.ended
            .store(if finished { 1 } else { 2 }, Ordering::SeqCst);
    }
}

struct LastOwner {
    state: UiThreadOnly<Rc<State>>,
}
impl UiaHost for LastOwner {
    fn widgets(&self) -> Option<Widget> {
        self.state.get()?;
        Some(root_with(Vec::new()))
    }
    fn action(&self, _: WidgetId, _: WidgetAction) -> windows::core::Result<()> {
        Ok(())
    }
}

/// When the host holds the last reference to the state that holds the slot, closing frees the
/// slot. That must be the last thing `shutdown` does, because it uses the slot until then.
#[test]
fn the_slot_is_finished_before_a_host_with_the_last_reference_ends() {
    let _one_at_a_time = one_ui_thread();
    let window = TestWindow::from_text("abc");
    let ended = Arc::new(AtomicU8::new(0));
    let state = Rc::new(State {
        slot: UiaSlot::new(),
        ended: ended.clone(),
    });
    let host: Arc<dyn UiaHost> = Arc::new(LastOwner {
        state: UiThreadOnly::new(state.clone()),
    });
    let raw = &raw const state.slot;
    let answer = state.slot.on_window_message(
        window.hwnd,
        WM_GETOBJECT,
        WPARAM(0),
        LPARAM(UiaRootObjectId as isize),
        || host,
    );
    assert!(answer.is_some(), "the request made the provider");
    drop(state);
    assert_eq!(ended.load(Ordering::SeqCst), 0, "the host holds the state");
    // SAFETY: the host holds the state, and `shutdown` touches nothing after that reference ends.
    unsafe { &*raw }.shutdown().unwrap();
    assert_eq!(
        ended.load(Ordering::SeqCst),
        1,
        "the state must end last, with the slot already finished"
    );
}
