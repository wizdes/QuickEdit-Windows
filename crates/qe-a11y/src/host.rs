//! What the shell gives this crate.
use crate::text::TextRange;
use qe_api::{ViewContext, Widget, WidgetId};
use qe_editor::EditorView;
use std::{mem::ManuallyDrop, thread::ThreadId};
use windows::core::Result;

#[derive(Clone, Debug, PartialEq)]
pub enum WidgetAction {
    Invoke,
    Toggle,
    SetValue(String),
    Select,
    Expand,
    Collapse,
    SetRange(f64),
    Focus,
    /// Select this text in a field, in UTF-16 units of its value.
    SelectText(TextRange),
}

/// Every call runs on the window's thread. None of them may wait for disk or shape text, and only
/// `action` for a posted action may open a dialog. See the README.
pub trait UiaHost: Send + Sync {
    /// The painted tree as it stands. Its root is the window, and its bounds and every
    /// child's are in window coordinates, in device-independent pixels.
    fn widgets(&self) -> Option<Widget>;
    /// Runs one widget action. Invoke, Toggle, Expand, Collapse and Select run from a posted
    /// message after the client's call has returned, so they may open a modal dialog or close
    /// the window. SetValue, SetRange, SelectText and Focus run inside the call, and must not.
    fn action(&self, id: WidgetId, action: WidgetAction) -> Result<()>;
    /// The editor's place in the tree: its bounds, name, focus and a stable ID. `None` when
    /// this window shows no editor.
    fn document_widget(&self) -> Option<Widget> {
        None
    }
    /// A number for the editor view on show, different for each tab. A range keeps it, so a
    /// range from a closed or hidden tab is stale.
    fn view_id(&self) -> Option<u64> {
        None
    }
    /// Runs `body` with the view on show, and returns true. Returns false if the view is
    /// already borrowed or this is not the window's thread, and the call then tells the
    /// client to ask again. The host applies `ctx.requests` afterward, as for any input.
    fn with_editor(&self, _body: &mut dyn FnMut(&mut EditorView, &mut ViewContext)) -> bool {
        false
    }
    /// The font in use: its family, and its size in points. `None` tells a client that the
    /// font is not reported.
    fn font(&self) -> Option<(String, f64)> {
        None
    }
    /// The window's DPI, if the shell tracks it. `None` asks Windows.
    fn dpi(&self) -> Option<u32> {
        None
    }
    /// True for a menu item that can be checked, checked or not. `qe_api::WidgetStates` has no
    /// field for this yet, so this asks the host. See the README's list for root.
    fn is_checkable(&self, _id: WidgetId) -> bool {
        false
    }
    /// A posted action failed after its call had already said yes. Show the reason at once.
    fn action_failed(&self, id: WidgetId, action: &WidgetAction, error: &windows::core::Error) {
        eprintln!("The action {action:?} on widget {} failed: {error}", id.0);
    }
}

/// Lets a host that holds `Rc`, `RefCell` or an `EditorView` meet `Send + Sync`. Only the
/// thread that made it can see the value. If the last reference ends on another thread, the
/// value leaks instead of being dropped on the wrong thread.
pub struct UiThreadOnly<T> {
    thread: ThreadId,
    value: ManuallyDrop<T>,
}

impl<T> UiThreadOnly<T> {
    pub fn new(value: T) -> Self {
        Self {
            thread: std::thread::current().id(),
            value: ManuallyDrop::new(value),
        }
    }
    /// The value, on the thread that made it. `None` on any other thread.
    pub fn get(&self) -> Option<&T> {
        (std::thread::current().id() == self.thread).then_some(&*self.value)
    }
}

impl<T> Drop for UiThreadOnly<T> {
    fn drop(&mut self) {
        if std::thread::current().id() == self.thread {
            // SAFETY: this is the only drop of `value`, and `get` cannot run after it.
            unsafe { ManuallyDrop::drop(&mut self.value) };
        }
    }
}

// SAFETY: `get` hands the value out only on the thread that made it, and `drop` only drops
// it there. Other threads can move or share the wrapper, but cannot reach the value.
unsafe impl<T> Send for UiThreadOnly<T> {}
unsafe impl<T> Sync for UiThreadOnly<T> {}
