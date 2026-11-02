# Wiring `qe-tools` into a window

One `ToolsHost` serves one window. All its calls run on the UI thread. With everything closed, `poll` returns at once, `layout` gives back the same area, `draw` paints nothing, and no thread runs. `tests/review.rs` follows this page line by line in its `Window` type.

```rust
// Once, when the window is made.
let mut tools = ToolsHost::new();
tools.set_wake(wake);                       // Must only post a message to the window.
tools.set_substitutions(for_this_document);

fn relayout(..) {
    let rest = tools.layout(text_area, &measure);  // The bar takes the top of the text area.
    view.layout(rest, &measure);
}

// Each event from the window, including Focus, Blur, the mouse and the wheel.
if tools.handle_input(&event, &mut view, &measure, &mut ctx) == Handled::No {
    if let Some(command) = key_table(&event) {
        /* run it, below */
    } else if tools.event_for_editor(&event) {
        view.handle(&event, &mut ctx);
    }
}
after_tools();

// Each command, from a key or the menu bar.
if ToolsHost::takes_command(command) {
    tools.run_command(command, &mut view, &measure, &mut ctx);
    after_tools();
}

// From the wake's message, from the store's wake, and after each view.poll_layout and edit.
tools.poll(&mut view, &measure, &mut ctx);
after_tools();

fn after_tools() {
    if tools.take_layout_changed() { relayout(); }   // The bar opened or closed.
    if tools.take_beep() { /* MessageBeep(MB_OK) */ }
}

// Paint: after the editor, so that the tools stand on top.
view.draw(&mut list, &theme, &measure);
tools.draw(&mut list, &theme, &measure);

// Menu rows.
if let Some((enabled, checked)) = tools.command_state(command, &view) {
    set_command_state(&mut menu_model, command, enabled, checked);
}

// A tab or window shows another document.
tools.view_switched(Some(&mut old_view), &mut new_view, &mut ctx);
```

The commands are `Find`, `FindReplace`, `FindNext`, `FindPrevious`, `UseSelectionForFind`, `JumpToSelection`, `SelectLine`, `Complete`, `UpperCase`, `LowerCase`, `Capitalize`, `SmartCopyPaste`, `SmartQuotes` and `SmartDashes`.

**The keyboard.** `tools.focus()` says who has it. The host does not need to tell the view: `ToolsHost` sends the view its own `Focus` and `Blur` as the keyboard moves between the text and the tools, so only one caret blinks. When the window itself gains or loses the keyboard, pass that through `handle_input` as above. It reaches whichever has the keyboard.

**The mouse and the wheel** that the tools leave always go on to the view, so the text scrolls while the bar has the keys.

**Settings.** `ToolsHost` saves nothing.

- As on the Mac, the three Substitutions menu rows change one document only. The Mac's Settings says "Choose Edit > Substitutions to change this option for individual documents."
- So the host keeps `tools.substitutions()` with each tab. It reads it before `view_switched`, and calls `set_substitutions` for the new tab.
- The saved defaults belong to `qe-settings`. A new document starts from them. The menu rows do not write to them.

## A mapped file that is still indexing

`worker_snapshot()` on a mapped file reads only what was indexed when it was taken. So a search stops at that point and waits, and `tools.poll` hands it a newer snapshot once `store_status` knows more. The store's own wake, from `FileLoad::start` or `view.set_wake`, must lead to `tools.poll`, or a search on a file still indexing stops at the part known so far.

## Screen readers

`tools.widgets()` gives the nodes to add to the window's tree. They say what is there and which one has focus. A client can read them and nothing more.

For M11, `qe_a11y::WidgetHost::action(id, action)` needs a route to these controls, and `ToolsHost` has none yet. M11 needs:

1. `ToolsHost::widget_action(id, WidgetAction, &mut view, &measure, &mut ctx)`, which does `Focus`, `Invoke` on each button and `SetValue` on the two fields and the panel's field, by the same paths as a click or typing.
2. A UI Automation focus-changed event when `tools.focus()` changes.
3. A live-region event when the count label changes, so that "3 found" is announced.

No screen reader has been run against any of this.

## Hooks that `qe-editor` still needs

Smart Quotes, Smart Dashes and Smart Copy need the editor to call these functions, and it has no place to do so yet.

```rust
// In EditorView. Called for one typed character, before it goes in, on the UI thread.
pub fn set_typing_filter(
    &mut self,
    filter: Option<Box<dyn FnMut(&str /* typed */, &str /* last 8 bytes before the caret */)
        -> Option<(usize /* bytes to remove before the caret */, String /* text to insert */)>>>,
);
// In EditorView. True when the selection was made by double-click, by word.
pub fn selection_is_word(&self) -> bool;
// In EditorView. Called for Cut, Delete and Paste, as the two calls in qe_tools below need.
pub fn set_clipboard_filter(&mut self, filter: Option<Box<dyn ClipboardFilter>>);
pub trait ClipboardFilter {
    fn cut_range(&mut self, selected: Range<ByteOffset>, before: Option<char>, after: Option<char>,
        was_word: bool) -> Range<ByteOffset>;
    fn paste_text(&mut self, text: &str, before: Option<char>, after: Option<char>,
        was_word: bool) -> String;
}
```

Root then wires them to `qe_tools::substitute_typed`, `smart_delete_range` and `smart_paste`, with `Substitutions::for_document(bytes)` so that quotes and dashes turn off over 50 MiB.

Replace All's apply also needs an editor change to stay inside a frame. `prepare_edits` builds the text on a worker, but `apply_edits` still does work that grows with it on the UI thread. A bounded design would let a worker hand over a finished buffer that the store adopts by moving it.
