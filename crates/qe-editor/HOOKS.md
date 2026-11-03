# Hooks for the crates built on the editor

`qe-ime` and `qe-spell` (M10), `qe-a11y` (M11), `qe-tools` (M12), `qe-views` and `qe-pdf` (M13), and M14's Save use the editor only through the calls on this page. None of them needs to edit `qe-editor`. Every example here runs as a doc test, with `cargo test -p qe-editor --locked --doc`.

**Rules for all of them**

- All calls on `EditorView` run on the UI thread, because the view lives there. Only `worker_snapshot`'s result, `prepare_edits` and its result go to other threads.
- No call here waits for disk on the UI thread, so some answers are "not yet". Geometry and the input method's layouts never shape on the UI thread.
- Two kinds of call can shape one short piece of text on the UI thread, as every arrow key and Delete does: `word_range_at`, and an edit, which finds its caret stop. The piece is at most 2 KiB around the byte and only when non-ASCII text is next to it. Plain ASCII shapes nothing. `release_budget_a_word_query_on_shaped_text_has_a_bounded_cost` in `tests/hooks.rs` measures and limits it.
- "Not yet" is one of two things. `GeometryError::NotLaidOut` means that the layout has not arrived. `LookupError::NotYetKnown` means that the index of a mapped file has not reached this text, or that its page is not in memory. Ask again after a later `poll_layout`. The host already calls `poll_layout` from its timer and from the wake that `set_wake` sets.
- Offsets are bytes in UTF-8, unless a name says UTF-16. All rectangles and points are in window coordinates, in device-independent pixels.

## Reading text (all)

A worker reads a snapshot that never changes. A search or a spell check stays current by asking what changed since its last revision.

| Call | Thread | Not yet |
| --- | --- | --- |
| `worker_snapshot()` | Get it on the UI thread, use it on any thread | Its plain queries wait for disk on a mapped file, so use only its `try_` queries on the UI thread |
| `revision()` | UI | Always answers. See "Revisions name their document" |
| `document_number()` | UI | Always answers. Differs for each view, and goes up with each `adopt` |
| `changes_since(revision)` | UI | `Current`, `Changes(list)` or `Rescan` |
| `byte_to_utf16`, `utf16_to_byte` | UI | `LookupError` |
| `store_status()` | UI | Says how much of a mapped file is known |

- Each change has its offsets in bytes and in UTF-16, in the text just before that change.
- The log holds the last 4,096 changes. Past that, or after a new document, the answer is `Rescan`.
- With `begin_edit_group`, each edit is its own change. An undo or a redo of a group is one change, from the group's first byte to its last. `apply_edits` is one change.
- A mapped file's revision stays the same while its index grows, so read `store_status`.

**Revisions name their document.** The value from `revision()`, from `worker_snapshot().revision()`, and in each `TrackedChange` holds the document's number as well as a count of edits. So:

- Treat it as a whole value. Within one document it goes up with each edit. Do not take numbers from it.
- `changes_since` says `Rescan` for a value from another document or view.
- `apply_edits` refuses edits that were prepared from another document or view, or from a snapshot that did not come from `worker_snapshot`.
- `mark_save_point_at` ignores a value from another document or view.

No caller has to compare document numbers for these. A check of `revision()` alone is enough to know that the text is the same.

```rust
use qe_api::ByteOffset;
use qe_api::{ViewContext};
use qe_editor::{ChangesSince, EditorView};

let mut view = EditorView::from_text("a\u{1f600}b").unwrap();
let seen = view.revision();
let snapshot = view.worker_snapshot();
std::thread::spawn(move || assert_eq!(snapshot.line_count(), 1)).join().unwrap();

let mut ctx = ViewContext::default();
view.replace_range(ByteOffset(5)..ByteOffset(6), "xy", &mut ctx);
match view.changes_since(seen) {
    ChangesSince::Changes(changes) => {
        assert_eq!(changes[0].change.replaced, ByteOffset(5)..ByteOffset(6));
        assert_eq!(changes[0].utf16.start.0, 3);
    }
    ChangesSince::Current | ChangesSince::Rescan => panic!("it changed"),
}
```

## Word and line queries (M12)

`word_range_at(byte)`, `line_range(line)` and `line_of_byte(byte)` run on the UI thread. Each returns `LookupError::NotYetKnown` for text that the index or memory does not hold. `word_range_at` uses the same rule as a double click, so it can shape its one short piece of text, as the rules at the top say. In a release build on an idle PC, the slowest of 800 queries took under 0.4 ms. On a busy PC, one run saw 30 ms for its slowest query. The budget test limits p99 to 2 ms. Complete uses the first, and Select Line uses the other two. `line_range` includes the line ending.

```rust
use qe_api::{ByteOffset, LineIndex};
use qe_editor::EditorView;

let mut view = EditorView::from_text("hello big world\nsecond").unwrap();
assert_eq!(view.word_range_at(ByteOffset(7)).unwrap(), ByteOffset(6)..ByteOffset(9));
assert_eq!(view.line_range(LineIndex(1)).unwrap(), ByteOffset(16)..ByteOffset(22));
assert_eq!(view.line_of_byte(ByteOffset(20)).unwrap(), LineIndex(1));
```

## Geometry (M10, M11, M13)

These answer from layouts on hand, on the UI thread, in window coordinates. They cost little, because they never shape.

| Call | Answer |
| --- | --- |
| `range_rects(range)` | `Vec<RangeRect>`: one for each row, and one for each direction run on a mixed row. An empty range gives the caret's place. Over 1,024 lines gives `TooManyLines` |
| `caret_rect()` | The caret. While a composition shows, its cursor |
| `byte_at_point(point)` | `HitTest`. Below the text, the end of what is known |
| `visible_rows()` | `VisibleRows`: `rows` that have a layout, and `complete` |
| `visible_range()` | First byte on screen to the end of the last row, or `NotLaidOut` until `complete` |

- `VisibleRow` has `line`, `first_byte`, `y`, `height` and `continuation`, which is true for a wrapped row. The gutter numbers rows where `continuation` is false.
- With wrap off, a long line gives one row, for the part at the view's left edge.
- A range far off screen has no layout. It says `NotLaidOut`, and that may stay true. Use it for text on screen.
- While a composition shows, all of these answer for what is painted. The text after a composition has its moved place. Bytes stay document bytes, so `byte_at_point` over the composition gives its place in the text, and selected text that gives way to a composition has an empty range at that place.

```rust
use qe_api::{ByteOffset, Dip, Rect, TextMeasurer, View};
use qe_editor::{EditorView, GeometryError};
# let measure = qe_layout::DirectWriteLayout::new().unwrap();

let mut view = EditorView::from_text("hello").unwrap();
let bounds = Rect { x: Dip(0.0), y: Dip(0.0), width: Dip(400.0), height: Dip(300.0) };
view.layout(bounds, &measure);
match view.range_rects(ByteOffset(0)..ByteOffset(5)) {
    Ok(rects) => assert_eq!(rects.len(), 1),
    // Ask again after the next `poll_layout`.
    Err(GeometryError::NotLaidOut) => {}
    Err(other) => panic!("{other:?}"),
}
```

## Decorations (M10, M12)

`set_decorations(kind, ranges, ctx)` replaces all marks of one kind, and `decorations(kind)` reads them back. The kinds are `FindMatch` and `CurrentFindMatch` (fills behind the text, under the selection), `SpellingUnderline` (a wavy line over the text) and `CompositionUnderline` (a straight line over the text). The colors are `Theme::find_match`, `Theme::current_find_match`, `Theme::spelling_underline` and `Theme::text`.

**How an edit moves marks:**

1. A mark wholly before the edit stays.
2. A mark wholly after the edit moves with its text.
3. A mark with any of its bytes replaced is dropped. So is a mark with an insertion strictly inside it.
4. An insertion at either end of a mark leaves the mark on its old text.
5. An undo or a redo of a group, and `apply_edits`, count as one edit from the first byte to the last, so they drop every mark in that span. With `begin_edit_group`, each edit moves marks by itself, so a mark between two edits stays until an undo of the group.

**Cost.** Setting marks takes time in proportion to the list, and it sorts them. After that, a key costs only the distance between edits in the list, and a frame only the marks on screen. Marks do not paint on a line that a composition stands on. A ghost line, shown while its new layout is on the way, paints with no marks.

```rust
use qe_api::{ByteOffset, ViewContext};
use qe_editor::{DecorationKind, EditorView};

let mut view = EditorView::from_text("speling here").unwrap();
let mut ctx = ViewContext::default();
let word = ByteOffset(0)..ByteOffset(7);
view.set_decorations(DecorationKind::SpellingUnderline, vec![word], &mut ctx);
view.replace_range(ByteOffset(8)..ByteOffset(8), "x", &mut ctx);
assert_eq!(view.decorations(DecorationKind::SpellingUnderline).len(), 1);
```

## Input method (M10)

Send the `InputKind` events through `handle`, or call the methods. Both keep their order behind a waiting key when they come through `handle`.

| Event | Call | What it does |
| --- | --- | --- |
| `ImeStart` | `ime_start()` | Starts at the selection's start |
| `ImeUpdate { text, cursor_utf16 }` | `ime_update(text, cursor, ctx)` | Shows the text. A worker lays it out, and the last layout stays until then |
| `ImeEnd { text }`, text not empty | `ime_commit(text, ctx)` | One edit, in place of the selection |
| `ImeEnd { text }`, text empty | `ime_cancel(ctx)` | Ends it |

**The store and the selection do not change until a commit**, so a cancel has nothing to restore. On screen, a selection inside the line gives way to the composition. A selection that goes past the line stays highlighted.

- **`composition()`** gives the text, the cursor, the place, and `showing`, which is false until the first layout arrives. **`composition_rects()`** gives the rectangles for a candidate list, and `caret_rect()` gives its cursor.
- **Esc or `ime_cancel`** throws the text away. These are the only calls that do.
- **Any other edit, a caret move, a click, an undo, Prevent Editing or `adopt` commits the text first**, as its own undo step. A range or a place passed to that call is in the text as it stood, and it moves with the commit. `apply_edits` then refuses edits that were prepared before the commit.
- **Both ask the host to tell the input method** with `ViewRequest::CancelComposition`, which the shell passes on as `ShellEvent::CancelComposition`. `take_composition_end()` says once which it was: `Committed { revision }` or `Cancelled`.
- **Clauses:** `ime_set_clauses(clauses, ctx)` sets the parts to underline, in UTF-16 units. The target clause gets a heavier line. `composition_clauses()` reads them.
- **Places:** `composition_range_rects(utf16_range)` gives the rectangles of part of the text, for a candidate list. It says `NotLaidOut` while newer text waits for its layout, which `composition().current` also says. `composition_place_at_point(point)` gives the UTF-16 place under a point. `byte_at_point` answers from what shows on a composition's line, and a point on the composition's own text gives the byte where it would go.
- **`select_directed(anchor, caret, ctx)`** sets a selection with its caret at either end.
- **A change of width, wrap, font or margins** lays the composition out again. Until the worker answers, the plain line shows.
- **If a layout fails**, a warning says so, and the last good layout stays.
- **`flush_input` does not end a composition.** Its text is not in the store. A Save decides whether to ask TSF to commit or cancel it first.
- **A late `ImeEnd`** has no composition number. After `CancelComposition`, the host must not pass on text from that composition, because the view would put it at the caret a second time.
- **A read-only view** counts the attempt in `refused_edits` and shows nothing.
- **Line breaks** in the text show as spaces while composing. A commit keeps them.
- **A cursor** inside a surrogate pair steps back to its start. Past the end, it stops at the end.
- **On a wrapped line**, the lines below move down while the composition is tall, and move back at its end.
- **On a line the text makes long**, all of the line still shows, whatever its size was before.
- **On a line already long**, the composition shows inside its 2 KiB segment, so the text after it moves only inside that segment. An extra row can overlap the next segment until the commit.

```rust
use qe_api::{ByteOffset, ViewContext};
use qe_editor::EditorView;

let mut view = EditorView::from_text("abc").unwrap();
let mut ctx = ViewContext::default();
view.ime_update("\u{4e2d}", 1, &mut ctx);
assert_eq!(view.text().unwrap(), "abc");
assert_eq!(view.composition().unwrap().at, ByteOffset(0));
view.ime_commit("\u{4e2d}", &mut ctx);
assert_eq!(view.text().unwrap(), "\u{4e2d}abc");
```

## Space and scrolling (M13)

| Call | What it does |
| --- | --- |
| `set_margins(left, right, ctx)` | Keeps space for a gutter and a map. `layout` and `Resize` still take the whole area. The text, hit tests and drags use the space between, and the text clips to it |
| `text_area()` | The rectangle the text paints in |
| `set_scroll_position(y, ctx)` | Exact, within 0 to `max_scroll_position()`. It ends any wait for a place, as the wheel does |
| `set_top_line(first, ctx)` | Puts a line at the top. See below |
| `reveal_range(range, align_top, ctx)` | Shows a range at the top or the bottom. See below |
| `scroll_offset()`, `horizontal_offset()`, `total_height()` | Read the view. `total_height` counts lines with no layout at their estimate, so it changes as layouts arrive |
| `set_page_wrap(Some(PageWrap { text_width, margin }), ctx)` | Wraps at a fixed width, for Wrap to Page. Needs wrap on. `None` wraps to the view again. The view scrolls sideways when the page is wider |
| `wraps()` | Whether the view wraps text, for a screen reader that counts rows as lines |
| `visible_row_end(&row)` | The real end of a row on screen, even one only partly in view |
| `set_paints_background(false, ctx)` | Lets the host paint paper under the text |
| `apply_font(family, points, ctx)` | Zoom. The same text stays at the top |

Margins, page wrap, wrap and font all keep the same text at the top of the view, with the caret on screen if it was on screen.

Paper margins: `PageWrap::margin` is left of the text. The host paints the paper, and page gaps are not in the text's layout. "Page gaps" below says why, and what it would take.

**Two calls scroll to a place in the text, not to a distance.**

- `set_top_line(first, ctx)` puts a line at the top of the view. A whole number is the line's first row, and a fraction is that share of its height. It reads the line's real place, so it does not depend on how lines above it wrap. Layouts that arrive above it later do not move it off that line. A value past the end means the last line, and a value that is not a number means the top. It ends any wait for a place, as the wheel does.
- `reveal_range(range, align_top, ctx)` puts the range's start at the top of the view, or its end at the bottom. The caret and the selection stay. Text with no layout goes to an estimate at once, then to its exact place as its layout arrives, unless the user scrolls first. With wrap off, the view moves sideways only if the text is out of sight. It returns `LookupError` and moves nothing for a place inside a character, past the end, or not yet known.

```rust
use qe_api::{Dip, ViewContext};
use qe_editor::{EditorView, PageWrap};

let mut view = EditorView::from_text("hello").unwrap();
let mut ctx = ViewContext::default();
view.set_margins(Dip(40.0), Dip(100.0), &mut ctx);
view.set_page_wrap(Some(PageWrap { text_width: Dip(468.0), margin: Dip(72.0) }), &mut ctx);
view.set_scroll_position(0.0, &mut ctx);
assert!(view.total_height() > 0.0);
```

## Edits from tools (M12)

Replace All has two steps. A worker runs `prepare_edits(snapshot, edits, cancel)`, which merges sorted edits into one edit, and refuses a range that does not start and end on character edges. It cuts the merged text into a finished tree of the store's own leaves. The UI thread runs `apply_edits(prepared, selection, ctx)`, which puts that tree in the store by reference, makes one edit and one undo step, and sets the selection that the caller chose. If the text changed in between, or the edits came from another document, `apply_edits` returns an error and changes nothing, so prepare again. A read-only view refuses it. If the edit itself fails, it returns that error and leaves the selection alone.

**Cost.** `apply_edits` does no work that grows with the text. What it still costs is laying out the rows on screen, as any edit that changes the whole screen does, so a taller window costs more. `tests/m14_measure.rs` has the measurement, and `tests/m14_replace_all.rs` has the budget.

**Freeing.** After `apply_edits` returns, dropping the `PreparedEdits` frees little. Edits that were never applied still hold their whole tree, and freeing it takes time in proportion to it, so give those to `discard_edits(prepared)`. History that a Replace All pushed out goes to a worker to free when the view is idle or loses focus.

| Replace All, 100,000 matches in 99 MB, 800 by 600 window | Before | After |
| --- | --- | --- |
| `apply_edits` on the UI thread, median of 4 runs of 7 each | 51.7 ms | 1.85 ms |
| The same, slowest run | 69.4 ms | 3.6 ms |
| `apply_edits` until the layouts on screen are in, median | 52.3 ms | 1.9 ms |
| Undo on the UI thread, median | 2.1 ms | 2.1 ms |
| `prepare_edits` on a worker, median | 94 ms | 121 ms |
| Dropping edits that were never applied, on the calling thread | 2.6 ms | 10 ms, so use `discard_edits` |

These ran one after the other on 2026-11-02 in a release build with `CARGO_PROFILE_RELEASE_LTO=off`, `CODEGEN_UNITS=16` and `STRIP=none`, with the PC 26% to 34% busy from other builds. The app's own fat LTO build was not measured. "On screen" here means that the view has its layouts. It does not include painting or showing the frame.

**Which thread.** `prepare_edits` reads and copies every byte from the first edit to the last, however few the edits are, and a mapped snapshot can wait for disk. So choose by the byte span and not by the count of edits. On an editable view, a span under about 64 KiB can run on the UI thread. Anything larger goes to a worker.

```rust
use qe_api::{ByteOffset, ViewContext};
use qe_editor::{EditorView, TextEdit, prepare_edits};
use std::sync::atomic::AtomicBool;

let mut view = EditorView::from_text("aa bb aa").unwrap();
let edits = [
    TextEdit { range: ByteOffset(0)..ByteOffset(2), text: "x".into() },
    TextEdit { range: ByteOffset(6)..ByteOffset(8), text: "x".into() },
];
let snapshot = view.worker_snapshot();
let prepared = prepare_edits(snapshot.as_ref(), &edits, &AtomicBool::new(false)).unwrap();
let mut ctx = ViewContext::default();
view.apply_edits(&prepared, ByteOffset(0)..ByteOffset(1), &mut ctx).unwrap();
assert_eq!(view.text().unwrap(), "x bb x");
```

## Typing and the clipboard (M14)

Smart Quotes, Smart Dashes and Smart Copy run through these. With no filter set, a key press checks one `Option` and costs nothing more.

- **`set_typing_filter(Some(filter))`.** The filter gets one typed character and the last 8 bytes before the caret, cut back to a character's edge. It returns `None` to type as usual, or `(bytes to remove before the caret, text to put in)`. A read-only view never asks it. Only `InputKind::Char` reaches it, so a composition's committed text does not. A composition goes into the text first, so the filter sees it. If the filter asks to remove more than it saw, or to cut a character in half, the typed character goes in as it was and a warning says so. The result is an edit with `typing` true, so it starts an undo step that later typing continues. Undo after `--` and more typing gives back the one hyphen.
- **`selection_is_word()`.** True when a double click, or a drag from one, made the selection. Any other way to the same bytes is false, and so is an edit.
- **`set_clipboard_filter(Some(filter))`.** `ClipboardFilter::cut_range` decides what Cut and each Delete command remove when a selection exists. A Delete with no selection does not ask. A Cut still copies only the selection. `paste_text` decides what Paste puts in. `before` and `after` are the characters on each side of the selection.
- **`was_word` for a Paste** is true when the last Copy or Cut here was of a word selection and the clipboard still holds exactly that text, so text from another app is never a word. A Copy or Cut over the 1 MiB limit for this thread counts as not a word.
- **A filter's bad range.** The edit fails with its usual warning, and the text stays. A Cut has by then put the selection on the clipboard.

## Caret stops (M14)

`caret_stop_after(byte)` and `caret_stop_before(byte)` give the next stop by the rule the arrow keys use, or `None` at the end or the start of the text. A byte inside a character or past the end gives `LookupError::OutOfRange`. Text not yet known gives `NotYetKnown`. They can shape one short piece of text, as the rules at the top say.

## Page gaps: a design note, not built

`PageWrap` has no `top_margin`, `page_height` or `page_gap` yet. Real pages need more than a small layout change, so they are not started.

**Why it is not small.**

1. `HeightMap` gives each line a height and adds them up, so a line's rows are one solid block. A page gap falls between two rows of one wrapped line, so it cannot be a line's height, and a gap's place moves with every edit above it.
2. All of the view's vertical arithmetic assumes that block. `self.scroll` is on 73 lines of `lib.rs` and `hooks.rs`, and 140 lines use it, the height map or `row_height`. Every painter adds `row.y` to a line's top, so each would have to ask for each row's real place: `paint_line_layout`, `paint_mixed_selection`, `paint_ghost`, the composition, `range_rects`, `caret_rect`, `visible_rows`, `byte_at_point` and `resolve`.
3. Long lines add a second level, because their segments have their own `HeightMap`.
4. The PDF's rule, in `crates/qe-pdf/src/page.rs`, is "a page takes every whole row that fits". Rows can differ in height. So where page 40 starts depends on the real height of every row before it. A line off screen counts at its estimate, so the screen cannot know those breaks until each line above has a layout.

**The design that would work.** Keep the height map as a flow with no gaps, and add one pure map between flow and paper.

1. `PageWrap` gains the three fields. A new `PageFold` in `qe-layout` holds `row_height`, `rows_per_page`, `top_margin` and the distance from one page's top to the next. `to_paper(flow_y)` and `to_flow(paper_y)` are arithmetic with no list, so they cost nothing that grows.
2. `self.scroll` and `max_scroll` move to paper space. `visible_rows` and the painters compute each row's place as `to_paper(line_top + row.y)`, and a hit test turns a point back with `to_flow`. A point in a gap goes to the nearest row.
3. Because this assumes one row height, a line with a taller fallback font would shift later rows off the grid. The choice is to make every row on a page the same height in page mode, which needs the PDF to do the same, or to keep a list of page starts that a worker builds. The list is the exact answer, and it needs every line laid out, which the 50 MiB limit on page mode makes possible.
4. Tests: a row never crosses a gap at any scroll, a click in a gap, a caret and a selection across a gap, a drag past one, and `page_breaks.rs` extended so each PDF page's first row is the row that starts that page on screen.

My estimate is one to two days of building and testing, mostly in `qe-editor`. It is a decision for root, because of point 3.

## Commands a menu runs (all)

`run_host(HostCommand, ctx)` runs in order with the keys before it, so a menu action cannot pass a waiting End. The commands are `PreventEditing`, `Wrap`, `Font`, `PageWrap`, `Margins`, `ScrollTo`, `Select` and `ApplyEdits`. A command that fails shows its reason as a warning. Clipboard, undo and delete commands stay in `run(EditorCommand, ctx)`, which keeps the same order.

A right click asks for the context menu with `ViewRequest::ContextMenu(point)`, which reaches the owner as `ShellEvent::ContextMenu(point)`.

```rust
use qe_api::{ByteOffset, ViewContext};
use qe_editor::{EditorView, HostCommand};

let mut view = EditorView::from_text("abc").unwrap();
let mut ctx = ViewContext::default();
let range = ByteOffset(1)..ByteOffset(2);
view.run_host(HostCommand::Select { range: range.clone(), reveal: true }, &mut ctx);
assert_eq!(view.selection(), range);
```

## Save (M14)

1. Call `flush_input(ctx)` so that typing still in the queue goes into the text. Keys that wait for a layout are dropped with their warning. Keys that wait for an index run on what is known.
2. Take `worker_snapshot()`, remember `revision()`, and write the snapshot on a worker.
3. When the write ends well, call `mark_save_point_at(revision)`. It returns true when the view still holds that text, and it is then saved. It returns false in two cases. If the user changed the text during the write, the disk now holds text the view no longer has, so the view reads as unsaved, even after an Undo back to the old save point or to the written text, until the next save. If the revision is from another document, nothing changes. `mark_save_point()` marks the text as it stands.

`is_at_save_point()` reads the result. `flush_input` keeps the warning of a dropped key, even when typing behind that key goes in at the unmoved caret, and a Save should show it. `cut_is_running()` says that a Cut's text is still on its way to the clipboard, and `adopt` refuses until it ends, so Cut then Open cannot lose the text.

```rust
use qe_api::{InputEvent, InputKind, View, ViewContext};
use qe_editor::EditorView;

let mut view = EditorView::from_text("abc").unwrap();
let mut ctx = ViewContext::default();
view.handle(&InputEvent { time_us: 0, kind: InputKind::Char('x') }, &mut ctx);
view.flush_input(&mut ctx);
let written = view.revision();
assert!(view.mark_save_point_at(written));
assert!(view.is_at_save_point());
```

## Scrolling and rows (M11, M14)

`wraps()` says whether long lines wrap. `visible_row_end(&row)` gives the byte after a visible row. `reveal_range(range, align_top, ctx)` brings a range into view with the selection left alone, and says `NotYetKnown` for text the index has not reached. `horizontal_position()` and `set_horizontal_position(x, ctx)` read and set the sideways scroll with wrap off, within the widest text seen. `caret_stop_after(byte)` and `caret_stop_before(byte)` give the caret stops, the same ones the arrow keys use.
