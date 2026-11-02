# Wiring `qe-tools` into a window

One `ToolsHost` serves one window. All its calls run on the UI thread. With everything closed, `poll` returns at once, `layout` gives back the same area, `draw` paints nothing, and no thread runs.

```rust
// Once, when the window is made.
let mut tools = ToolsHost::new();
tools.set_wake(wake);                       // Must only post a message to the window.
tools.set_substitutions(from_settings);     // Smart Copy, quotes and dashes from settings.

// Layout: the bar takes the top of the text area, and the editor gets the rest.
let rest = tools.layout(text_area, &measure);
view.layout(rest, &measure);

// Each input event, before anything else.
if tools.handle_input(&event, &mut view, &measure, &mut ctx) == Handled::Yes { return; }
if let Some(command) = key_table(&event) { /* run it, below */ return; }
if tools.focus() == ToolsFocus::Editor { view.handle(&event, &mut ctx); }

// Each command, from a key or the menu bar.
if ToolsHost::takes_command(command) {
    tools.run_command(command, &mut view, &measure, &mut ctx);
    // The bar may have opened or closed, so lay out again.
    let rest = tools.layout(text_area, &measure);
    view.layout(rest, &measure);
}

// From the wake's message, and after each view.poll_layout and each edit.
tools.poll(&mut view, &measure, &mut ctx);
if tools.take_beep() { /* MessageBeep(MB_OK) */ }

// Paint: after the editor, so that the tools stand on top.
view.draw(&mut list, &theme, &measure);
tools.draw(&mut list, &theme, &measure);

// Menu rows.
if let Some((enabled, checked)) = tools.command_state(command, &view) {
    set_command_state(&mut menu_model, command, enabled, checked);
}

// A tab or window shows another document.
tools.view_switched(Some(&mut old_view), &mut new_view, &mut ctx);

// Screen readers: add these to the window's tree.
let nodes = tools.widgets();
```

The commands are `Find`, `FindReplace`, `FindNext`, `FindPrevious`, `UseSelectionForFind`, `JumpToSelection`, `SelectLine`, `Complete`, `UpperCase`, `LowerCase`, `Capitalize`, `SmartCopyPaste`, `SmartQuotes` and `SmartDashes`.

## The bar's marks

The bar sets `FindMatch` for text near the view, and `CurrentFindMatch` for the match it stands on. It sets the second one again after each edit, because `RangeList::edit` in `qe-editor` can give a wrong range. See the follow-ups in the milestone report.

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

## A mapped file that is still indexing

`worker_snapshot()` on a mapped file reads only what was indexed when it was taken. `HOOKS.md` does not say this yet. So a search stops at that point and waits, and `tools.poll` hands it a newer snapshot once `store_status` knows more. The store's own wake, from `FileLoad::start` or `view.set_wake`, must lead to `tools.poll`, or a search on a file still indexing stops at the part known so far.
