# QuickEdit UI Automation bridge

This crate shows the editor's text and the painted widgets to Windows UI Automation (UIA), the API that screen readers use. All text positions are UTF-16 units.

```mermaid
flowchart LR
    Client[Screen reader] -->|WM_GETOBJECT| Slot[UiaSlot]
    Slot --> Root[Root]
    Root --> Widgets[Painted widgets]
    Root --> Document[The one document object]
    Document --> Ranges[Text ranges]
    Ranges --> Marshal[Move to the window's thread]
    Widgets --> Marshal
    Marshal --> Host[UiaHost in the shell]
    Host --> Editor[EditorView hooks]
```

## What the shell must do

UIA calls a provider on its own worker thread, not on the window's thread. `src/marshal.rs` moves each host call to the window's thread, so `UiaHost` methods always run there. The shell writes no marshaling code.

1. **Keep one `qe_a11y::UiaSlot` for each window**, somewhere the window procedure can reach it, such as `Rc<UiaSlot>` in the window's state. It starts empty. Its methods take `&self`, and none holds a borrow while host code runs. So host code may call any of them from anywhere: inside an action, inside a dialog, or in WM_DESTROY. Do not put the slot itself in a `RefCell`.

2. **Add one call at the top of the window procedure.** It handles WM_GETOBJECT and the crate's own message:

   ```rust
   if let Some(answer) = slot.on_window_message(hwnd, msg, wparam, lparam, || make_host()) {
       return answer;
   }
   ```

   Other messages cost one comparison, and `make_host` runs only for UIA's first request.

3. **Write the host.** Wrap anything that is not `Send` in `UiThreadOnly`:

   ```rust
   struct Host { ui: qe_a11y::UiThreadOnly<Rc<WindowState>> }

   impl qe_a11y::UiaHost for Host {
       // The painted tree as it stands. The root is the window. Do not include the editor.
       fn widgets(&self) -> Option<Widget> { self.ui.get()?.root_widget() }
       // Run the action. See "Actions" below.
       fn action(&self, id: WidgetId, action: WidgetAction) -> windows::core::Result<()> { /* ... */ }
       // Bounds, name (the tab title), focus and a stable ID for the editor.
       fn document_widget(&self) -> Option<Widget> { /* ... */ }
       // A number for the tab on show. A range from another tab then reports "gone".
       fn view_id(&self) -> Option<u64> { /* ... */ }
       // Give the view on show, with try_borrow_mut. Return false if it is busy.
       // Afterward, apply ctx.requests as for any input, because Select and scroll use them.
       fn with_editor(&self, body: &mut dyn FnMut(&mut EditorView, &mut ViewContext)) -> bool { /* ... */ }
       // The family and the size in points, from settings and zoom.
       fn font(&self) -> Option<(String, f64)> { /* ... */ }
       // The window's DPI, because the shell tracks it. Left out, the crate asks Windows.
       fn dpi(&self) -> Option<u32> { /* ... */ }
       // True for a menu item that can be checked. See "For root" below.
       fn is_checkable(&self, id: WidgetId) -> bool { /* ... */ }
   }
   ```

   Except for `action`, these calls must not wait for disk, shape text or pump messages.

4. **Report input.** After each input batch, and after `poll_layout` changes the text, call `slot.after_input()?`. After any change a client can read in a widget, call `slot.widgets_changed()?`. See "For M14" for the list. With no client ever, each reads one `Option`. With a provider and no listener, each makes one system call. Neither needs a timer.

5. **Close.** In WM_DESTROY, call `slot.shutdown()?`, and show its error if it gives one. It must run while the window still exists. After it, every object a client holds says `UIA_E_ELEMENTNOTAVAILABLE`, and WM_GETOBJECT falls through to `DefWindowProcW`. It works inside a sent message too. It first calls `ReplyMessage(0)`, because UIA refuses to disconnect while a sender waits, so that sender then hears 0 or "gone". If it still fails, it keeps what it needs, and a later call tries again.

`tests/support/mod.rs` is a working copy of steps 1 to 5 on a hidden window, and `tests/recipe.rs` runs it with actions that report input, close the window and act as a modal dialog.

## Actions

- **Invoke, Toggle, Expand, Collapse and Select** run from a posted message after the client's call has returned S_OK. The call checks that the control exists and is turned on. Later failures go to `UiaHost::action_failed`, and the default prints them, so a shell that wants them on screen overrides it.
- **A modal dialog** may open from these. Pump messages through the same window procedure, and hold no `RefCell` borrow of the view or the widget tree while the dialog runs. WM_GETOBJECT and read calls keep working during the dialog, and a test proves this. A borrowed view makes a call say `UIA_E_TIMEOUT`, not fail for good.
- **The action may destroy the window.** A posted action on a Close item that leads to WM_DESTROY inside the action is safe.
- **SetValue, SetRange, SelectText and Focus** run inside the call, so they can report a failure. They must not open a dialog or pump messages. If they run past the 2-second wait, the client hears `UIA_E_TIMEOUT` and the work still finishes.

UIA itself sets focus on an element before it invokes it, so a host sees a Focus action first. In a real client run, SetValue also brought a Focus action first.

## The rules a client sees

- **Line:** a visible row where a layout is on hand. Text off screen, or with no layout yet, counts by logical line, because shaping on the UI thread is not allowed. So a range's line can change as the view scrolls, and a 100 MB line off screen is one line.
- **Paragraph:** a logical line with its ending.
- **Word:** a run of word characters, or one other character, with the blank text after it. Runs come from the editor's `word_range_at`.
- **Character:** a grapheme cluster, by the table in `src/clusters.rs`. See the gaps below.
- **Format and Page:** the whole document, because plain text has one format and no pages.
- **Move:** an empty range stays empty. A range with text first goes to the start of the unit it is in, then moves, and it stays one unit long. A move makes at least 64 steps, then stops at 8 ms and returns how far it got, as UIA allows. A range with text moves to the very end only by line or paragraph, onto a final empty line, and it then stands there as an empty range. That rule is from memory of what Edit and RichEdit do.
- **GetText:** at most `GET_TEXT_CAP` units (1,048,576), whatever the limit. **A whole-file read of a huge file cannot be served in one call, and `GetText(-1)` shortens silently: the client gets the first part, with no sign that more exists.** A client reads on with a later range. A limit that ends inside a surrogate pair stops before the pair. The read runs on the calling UIA thread on a snapshot, so it answers in one call even for text not in memory. If any part cannot be read, the answer is an error, so a short answer always means the limit or the cap. A direct call on the window's own thread reads only text in memory, and otherwise says `UIA_E_TIMEOUT`.
- **FindText:** the search runs on the calling UIA thread on a snapshot, so the window's thread is held only long enough to hand it over, and one call gets the answer. It costs the text's length plus the query's length, not their product. It stops at `FIND_PATIENCE` (15 seconds) and then says `UIA_E_TIMEOUT` once, rather than claim text is absent. UIA's default transaction timeout is 20 seconds, from memory. No match is a real null, with null written over whatever the caller's out value held.
- **A direct FindText on the window's own thread:** UIA does not do this. It reads only text in memory, up to 64 KiB, and otherwise says `UIA_E_TIMEOUT`.
- **Not yet:** a busy window or a borrowed view says `UIA_E_TIMEOUT`, on every call that needs them. So does a move or a line lookup on text not in memory, and a page read starts by itself. While a mapped file's index grows, DocumentRange covers the known part. A window that stays busy for 2 seconds makes a call say the same, and the work either never runs or finishes with nobody listening.
- **Stale:** a range from another tab or document, or older than the editor's 4,096-change log, says `UIA_E_ELEMENTNOTAVAILABLE`, on every method. Within those limits a range follows edits and undo. A position inside replaced text goes to where that text began.
- **Rectangles:** only text on screen has them, clipped to the text area. An empty range has none, as UIA's contract says, so a caret range gives an empty list. A range off screen, or with no layout yet, gives an empty list too.
- **RangeFromPoint:** any point gets the nearest text, as UIA's contract says, even far outside the window. A point that is not a number is `E_INVALIDARG`. In a text field the position is an estimate, by share of the field's width.
- **ScrollIntoView:** it puts the range's start at the top or its end at the bottom, and the selection stays. Text with a layout lands exactly. Other text is placed by estimate, so on wrapped text it can land off by the rows the estimate does not know.
- **Read-only:** `IsReadOnly` is true, and the document offers Value with `IsReadOnly` true and `SetValue` refused. Value stops at the same cap as GetText. An editable document offers no Value.
- **Events:** at most one text change and one selection change per `after_input`. UIA has no caret event, so a caret move raises the selection change. The first batch after a client starts listening always raises both, even if nothing changed. `widgets_changed` raises focus, structure, field text, field selection, element selected, and property events for name, value, on or off, turned on or off, expanded and range value. Its first report after a client arrives only sets the baseline for these.
- **Runtime IDs:** a widget's is its number. The document's has a fourth part, so a widget and the document can share a number.

## For root

1. **`WidgetStates` needs one new field:** `pub checkable: bool`, "this item can be checked, whether or not it is now". Set it in `qe-ui/src/menu.rs`. Until then, the shell answers `UiaHost::is_checkable`. After it, `is_checkable` in the host is one line: `widget.states.checkable`.
2. **Grapheme stops:** add to `crates/qe-editor/src/hooks.rs`:

   ```rust
   pub fn caret_stop_after(&mut self, byte: ByteOffset) -> Result<Option<ByteOffset>, LookupError> {
       self.with_stops(|s| s.after(byte.0)).map(|o| o.map(ByteOffset))
   }
   pub fn caret_stop_before(&mut self, byte: ByteOffset) -> Result<Option<ByteOffset>, LookupError> {
       self.with_stops(|s| s.before(byte.0)).map(|o| o.map(ByteOffset))
   }
   ```

   `src/text.rs` then calls them through `TextDoc`, in place of `src/clusters.rs`. These are proposed and not compiled.
3. **ScrollIntoView:** add `EditorView::reveal_range(range, align_top, ctx)`.

## For M14

These need the shell or `qe-ui`, so the crate cannot do them alone.

1. **Call `slot.widgets_changed()` after every change a client can read in a widget:** a field's text or selection, a tab or list item selected, a checkbox, a list opened or closed, a scroll value, a name, or a control turned on or off. The crate compares each report with the last, and raises the events itself. It cannot see a change that is not reported.
2. **Keep widget IDs unique inside the painted tree.** Check this in `qe-ui`, because a duplicate makes navigation find the wrong node.
3. **Give fields their character boxes,** for `GetBoundingRectangles` and `RangeFromPoint`. This needs a new field on `qe_api::Widget`.
4. **Give `WidgetRange` a step,** for `SmallChange` and `LargeChange`.
5. **Check these in a visible window with Narrator:**
   1. A focus event reaches a client, because a hidden window cannot show this.
   2. Reading and magnifying still follow the caret, because an empty caret range now has no rectangle.

## Known gaps

- **Grapheme stops.** The rule here is not the editor's. It differs from `unicode-segmentation` 1.12 on 0.032% of 24.5 million tested pairs, mostly rare and historic scripts. Emoji, flags, ZWJ, keycaps, CRLF, Hangul, combining marks and Indic conjuncts matched in the sequences tested.
- **A run of flags** stays paired in both directions up to `FLAG_RUN_LIMIT`, which is 65,536 UTF-16 units, or 16,384 flags. Past that, forward and backward stops can differ.
- **A cluster longer than 2,048 units** ends at the window, as in the editor, so forward and backward stops can differ inside it. A test case had 3,000 combining marks on one letter.
- **A text field** gives the whole field's box for any range with text, and an estimated position for a point, because `Widget` does not say where its characters are.
- **RangeValue** reports a step of 1, because `WidgetRange` has no step.
- **Focus events cannot reach a real client from a hidden window.** UIA gives a focus handler only the real keyboard focus, and it refuses a focus handler on one element. The tests check that the event was raised without error.
- **Failed calls** that return an object leave their out value alone, unless they can also answer null. Those write null first. See `src/abi.rs`. UIA's own stubs clear the value before any call.
- **Out of memory:** GetText and Value return `E_OUTOFMEMORY` when Windows cannot make the string. Strings in property answers and Rust's own allocations still end the app, as everywhere in this workspace.
- **Duplicate widget IDs** inside the painted tree are not checked. `find_widget` takes the first match.

## Tests

Run these with `CARGO_PROFILE_DEV_DEBUG=0` and `CARGO_INCREMENTAL=0` set:

```
cargo test -p qe-a11y --locked
cargo clippy -p qe-a11y --locked --tests -- -D warnings
```

- `tests/real_client.rs`, `tests/real_events.rs` and `tests/recipe.rs` run a real IUIAutomation client against a hidden window, on another thread or in another process.
- `tests/com.rs`, `tests/widgets.rs`, `tests/mapped.rs`, `tests/threads.rs`, `tests/busy.rs`, `tests/events.rs`, `tests/review.rs` and `tests/astra.rs` call COM directly.
- `tests/model.rs` runs seeded random moves and edits against a plain model.
- `tests/measure.rs` and `tests/measure_key.rs` write real 512 MiB and 1 GiB files and are run by name, in release:

```
cargo test -p qe-a11y --release --locked --test measure --test measure_key -- --ignored --nocapture --test-threads=1
```

Several test threads with UIA windows at once in one process make UIA fail with E_FAIL or an empty tree, and the cause was not found. The tests with a real client take turns inside one test program. The app has one UI thread, and a test covers two windows on one thread.

## Measurements

These ran on 2026-11-02 on a PC with other builds running, in a release build with `CARGO_PROFILE_RELEASE_LTO=off`, `CODEGEN_UNITS=16` and `STRIP=none`. The app's own fat LTO build was not measured. Each call ran on the window's thread. Times are medians, with the worst in brackets, in milliseconds.

| Call | 512 MiB in memory | 512 MiB mapped | 1 GiB mapped |
| --- | --- | --- | --- |
| DocumentRange | 0.0003 (0.0109) | 0.0003 (0.0184) | 0.0005 (0.0084) |
| GetSelection | 0.0004 (0.0130) | 0.0004 (0.0109) | 0.0007 (0.0072) |
| GetText(200), middle | 0.0041 (0.0080) | 0.0751 (0.0967) | 0.0922 (0.1376) |
| Move, character | 0.0110 (0.0336) | 0.2272 (0.2834) | 0.2989 (0.4976) |
| Move, word | 0.2501 (0.3306) | 0.9874 (1.88) | 1.28 (1.77) |
| Move, line | 0.0267 (0.0415) | 0.2556 (0.4578) | 0.4233 (0.7219) |
| Move, huge character count | 8.01 (8.01) | 8.02 (11.41) | 8.03 (10.40) |
| GetBoundingRectangles, whole document | 0.0451 (0.0742) | 0.0817 (0.1108) | 0.0929 (0.1494) |

GetText(-1) and FindText run on the UIA thread, so each row is one call from another thread with the window's messages running. The second number is the longest that one pass of the window's message loop took during that call.

| One call | 512 MiB in memory | 512 MiB mapped | 1 GiB mapped |
| --- | --- | --- | --- |
| GetText(-1), the whole cap, from the middle | 2.4 ms, 0.018 ms | 3.9 ms, 0.018 ms | 7.8 ms, 0.045 ms |
| FindText far, exact case | 113.2 ms, 0.058 ms | 601.4 ms, 0.027 ms | 1,318.1 ms, 0.219 ms |
| FindText far, ignoring case | 2,331.9 ms, 0.068 ms | 2,288.7 ms, 0.150 ms | 4,967.9 ms, 0.161 ms |
| FindText absent, exact case | 82.0 ms, 0.051 ms | 688.6 ms, 0.148 ms | 1,989.8 ms, 10.770 ms |
| FindText absent, ignoring case | 2,304.9 ms, 0.066 ms | 2,455.4 ms, 0.267 ms | 8,651.4 ms, 36.266 ms |

At 8.7 seconds per GiB in the slowest run, 15 seconds covers about 1.7 GiB ignoring case on this PC while it is loaded, and a bigger file fails honestly once.

- **Over a frame:** one pass of the message loop took 36.266 ms during a 1 GiB search. That pass only ran the test's pump, and the search runs on another thread, so load from other builds is the likely cause. This is not proven.
- **Repetitive ignore-case search, on the window thread:** an all-a document with a half-length run of a followed by b. 4,096 units took 0.043 ms, 8,192 took 0.081 ms, 16,384 took 0.198 ms, and 65,536 took 0.819 ms. A review measured 232 ms at 16,384 units before the linear search.
- **A long query:** 60,000 units with a 30,001-unit query took 0.795 ms ignoring case and 0.779 ms exact.
- **While a 1 GiB file indexed:** 717 calls gave text from the known part, and the worst took 0.276 ms.
- **A key with no client:** 239.6 microseconds with the check, 239.3 without, which is no difference above noise. The check takes 2.0 ns when told no client listens. `UiaClientsAreListening` takes 4.4 ns.
- **That PC said a client was listening** on all 1,000,000 calls, so the real "no" path did not run in this timing. A test tells the check "no", and with an empty slot nothing runs at all.
- **Real client round trip, same process:** GetText(200) 1.512 ms, Move by line 1.517 ms. The test's message loop sleeps 1 ms between pumps, so the real loop should be faster. This was not measured in the app.
