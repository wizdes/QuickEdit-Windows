# Wiring `qe-tools` into a window

One `ToolsHost` serves one window. All its calls run on the UI thread. With everything closed, `poll` returns at once, `layout` gives back the same area, `draw` paints nothing, and no thread runs.

`Window` in `tests/review.rs` follows the event path, the layout, the key pairs and `Resize` below. It has no key table or menu, so its tests call `run_command` directly, and it never paints. The pictures in `tests/pictures.rs` cover painting. No test here runs in a real window.

```rust
// Once, when the window is made.
let mut tools = ToolsHost::new();
tools.set_wake(wake);                       // Must only post a message to the window.
tools.set_substitutions(for_this_document);

fn relayout(..) {
    let rest = tools.layout(text_area, &measure);  // The bar takes the top of the text area.
    view.layout(rest, &measure);
}

// A window resize or a new DPI. See "Window size" below.
text_area = new_text_area;
relayout();

// Each event from the window, in the order Windows sends it: `KeyDown`, then its `Char`, then
// `KeyUp`. Include Focus, Blur, DpiChange, Resize, the mouse and the wheel.
if tools.handle_input(&event, &mut view, &measure, &mut ctx) == Handled::No {
    if let Some(command) = key_table(&event) {
        /* run it, below */
    } else if tools.event_for_editor(&event) {
        view.handle(&event, &mut ctx);
    }
}
after_tools();

// Each command, from a key or the menu bar. Send every command here first, including Undo, Redo,
// Cut, Copy, Paste and Select All.
if tools.run_command(command, &mut view, &measure, &mut ctx) == Handled::No {
    /* the host's own command, or one for the view */
}
after_tools();

// From the wake's message, from the store's wake, and after each view.poll_layout and edit.
tools.poll(&mut view, &measure, &mut ctx);
after_tools();

fn after_tools() {
    if tools.take_layout_changed() { relayout(); }   // The bar opened, closed or changed rows.
    if tools.take_beep() { /* MessageBeep(MB_OK) */ }
}

// Paint: after the editor, so that the tools stand on top.
view.draw(&mut list, &theme, &measure);
tools.draw(&mut list, &theme, &measure);

// Menu rows. `None` means that the row's state is not this crate's to say.
if let Some((enabled, checked)) = tools.command_state(command, &view) {
    set_command_state(&mut menu_model, command, enabled, checked);
}

// A tab or window shows another document.
tools.view_switched(Some(&mut old_view), &mut new_view, &mut ctx);
```

`ToolsHost::takes_command` is true for the commands that are always this crate's: `Find`, `FindReplace`, `FindNext`, `FindPrevious`, `UseSelectionForFind`, `JumpToSelection`, `SelectLine`, `Complete`, `UpperCase`, `LowerCase`, `Capitalize`, `SmartCopyPaste`, `SmartQuotes` and `SmartDashes`.

**Edit commands.** Undo, Redo, Cut, Copy, Paste and Select All depend on who has the keyboard, so `takes_command` is false for them and `run_command` decides:

| Keyboard is on | `run_command` | What happens |
| --- | --- | --- |
| The text | `Handled::No` | The host gives the command to the view. |
| A find, replace or Select Line field | `Handled::Yes` | The field does it. A change to the find field searches again. |
| A button, a list or a message | `Handled::Yes` | Nothing. The document does not change while the user looks elsewhere. |

The key table takes these keys before the content does, so this is also the path for Ctrl+C. `command_state` gives `None` with the keyboard in the text, `(true, false)` in a field, and `(false, false)` elsewhere.

**The keyboard.** `tools.focus()` says who has it. The host does not need to tell the view: `ToolsHost` sends the view its own `Focus` and `Blur` as the keyboard moves between the text and the tools, so only one caret blinks. When the window itself gains or loses the keyboard, pass that through `handle_input` as above. It reaches whichever has the keyboard, and an open list closes.

**Key pairs.** Send a key's `Char` even when its `KeyDown` was handled. If a `KeyDown` moved the keyboard, as Tab does when it takes a word from the Complete list, `ToolsHost` drops the `Char` that follows it. If the shell swallowed that `Char` instead, nothing goes wrong, because the next key clears this.

**The mouse and the wheel** that the tools leave always go on to the view, so the text scrolls while the bar has the keys.

**Window size.** On `Resize`, `handle_input` returns `Handled::No`, closes any open list, and does nothing else. `event_for_editor` is false for it, because the view's area is what the bar leaves and not the whole window. So the host must set its text area and call `relayout`, which also moves the panel and its message.

On `DpiChange`, `handle_input` returns `Handled::No`. The view takes the event as any other. Then call `relayout` once the measurer has the new DPI. The shell's own `WM_DPICHANGED` code sends `DpiChange` first, and the new window size arrives after it as a `Resize`.

**Settings.** `ToolsHost` saves nothing.

- As on the Mac, the three Substitutions menu rows change one document only. The Mac's Settings says "Choose Edit > Substitutions to change this option for individual documents."
- So the host keeps `tools.substitutions()` with each tab. It reads it before `view_switched`, and calls `set_substitutions` for the new tab.
- The saved defaults belong to `qe-settings`. A new document starts from them. The menu rows do not write to them.

## A mapped file that is still indexing

`worker_snapshot()` on a mapped file reads only what was indexed when it was taken. So a search stops at that point and waits, and `tools.poll` hands it a newer snapshot once `store_status` knows more. The store's own wake, from `FileLoad::start` or `view.set_wake`, must lead to `tools.poll`, or a search on a file still indexing stops at the part known so far.

If the index stops because of a bad byte or a read error, `store_status().failure` says why. The search then finishes with what can be read, the bar shows the final count and the reason, and a step that was waiting says why too.

## Screen readers

`tools.widgets()` gives the nodes to add to the window's tree. They say what is there and which one has focus. Exactly one of them reports focus while the tools have the keyboard, and none while the text has it. A client can read them and nothing more.

`ToolsHost::widget_action(id, action, &mut view, &measure, &mut ctx)` is the route for a screen reader's actions. It does `Focus`, `Invoke` on each button and `SetValue` on the two fields and the panel's field. `qe-app` raises the focus and value events through `UiaSlot::widgets_changed` after every call into the tools.

No screen reader has been run against any of this.

## Select Line

The panel is a painted part of the window, with its title row and close button, below the bar's rows. It stays fixed in place, and the user cannot drag it. When a step or its Select button reveals a selection under it, the text scrolls so that the selection stands below it. In the first lines of a document the text cannot scroll that far, so a selection there can stay under the panel.

Ctrl+L is the keyboard's way back from the text to the panel, and Tab stays inside the panel. Esc closes it while it has the keyboard. Esc in the text does not. The Mac's tooltip text is only the group's description for now, and nothing paints it on hover.

## What stays over a frame

Replace All's apply also needs an editor change to stay inside a frame. `prepare_edits` builds the text on a worker, but `apply_edits` still does work that grows with it on the UI thread. A bounded design would let a worker hand over a finished buffer that the store adopts by moving it.
