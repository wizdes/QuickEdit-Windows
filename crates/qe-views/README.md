# qe-views

`ViewFrame` owns one window's line-number gutter, document map, Wrap to Page paper and zoom. It also owns the editor's side margins. The values come from the Mac app's `LineNumberRulerView`, `DocumentMapView`, `MultiplePageView`, `ScalingScrollView` and `PageLayout`.

```text
input -> ViewFrame::handle -> map or gutter, or EditorView
timer or wake -> poll_layout, then ViewFrame::sync -> requests for exactly what changed
paint -> ViewFrame::draw -> paper or text, gutter, map, from the state the frame holds
key -> 0.1 s wait -> worker_snapshot -> map worker -> rows -> wake -> sync
```

## What a host calls

| When | Call |
| --- | --- |
| New window | `ViewFrame::new(system_paper(), family, points)`, then `set_wake(waker)` |
| File opened | `set_file_bytes(Some(size))` |
| Content area changes | `frame.layout(&mut editor, content_rect, &measure, &mut ctx)` |
| `ShellEvent::ContentInput(event)` | `frame.handle(&mut editor, &event, &measure, &mut ctx)`, in place of `editor.handle` |
| `ShellEvent::Timer`, `ShellEvent::Wake` | `editor.poll_layout(&mut ctx)`, then `frame.sync(&mut editor, &measure, &mut ctx)` |
| An editor change that did not come through `handle` | `frame.sync(...)` |
| Paint | `frame.draw(&editor, &mut list, &theme, &measure)`, in place of `editor.draw` |
| `ShellEvent::ContentCaptureLost` | `frame.capture_lost(&mut ctx)` |
| Pointer leaves the window | `frame.pointer_left(&mut ctx)` |
| `WM_SETTINGCHANGE` | `frame.set_reduce_motion(system_reduce_motion())` |
| View menu | `set_line_numbers`, `set_document_map` |
| Format menu | `set_page_wrap`, with `wrap_menu_title(frame.page_wrap())` as the item's title |
| Zoom keys and menu | `zoom_in`, `zoom_out`, `actual_size`. `can_zoom_in` and `can_zoom_out` enable the items |

The host applies `ctx.requests` as usual. The shell has one view timer and keeps the last request, so call the frame after the editor with the same `ctx`. The frame then asks only for a timer sooner than the ones already asked for. The frame asks for a timer only while a fade or a 0.1 s wait runs, and with no idle timer.

## Rules copied from the Mac

- **Gutter:** off by default. Digits are at least 3, and the width is `ceil(digits * digit width + 6 + 8)`. The size is 85% of the text, rounded, at least 9 pt. One number shows on each line's first row, right-aligned and centered on that row. A 1-dip separator is the last column. Typing inside a line redraws after 0.1 s, and a line count change redraws at once.
- **Map:** 100 dips wide, on by default, no separator. One row of 2 dips for each line, blocks 1.4 dips tall, one column per UTF-16 unit, 2 dips in from the left. It redraws at most every 0.1 s while typing.
- **Box:** 10% fill and 22% outline. It shows when the pointer is within 8 dips left of the map, or during a drag. It fades in over 0.15 s and out over 0.25 s, a part-way fade takes its share of the time, and Reduce Motion jumps.
- **Click and drag:** a press on the box holds it where pressed. A press elsewhere centers the box on the pointer.
- **Page:** Letter in the US and Canada, A4 elsewhere, Legal by setting. Margins are 72 pt. The gutter and map hide. The paper is white with dark text in both themes, with a light gray frame at the text's sides.
- **Zoom:** 25% to 1600%. In doubles, and from 40-60% snaps to 100%. Out halves, and from 180-220% snaps to 100%.
- **Over 50 MiB:** the gutter and the map turn off with no message. The window's title carries `LARGE_FILE_TITLE_NOTE`.

## Differences from the Mac

- **Box place:** the box follows the scroll position, not whole lines, so a drag is exact. On rows that do not wrap, its top is within one map row (2 dips) of the Mac's, and its height is the same.
- **Number overlap:** the skip test uses the digit's ink, 0.72 of the font size. Segoe UI's line box is 16 dips at 9 pt and Cascadia Mono's 10 pt row is 15.6, so the Mac's test would hide every second number.
- **Zoom:** zoom goes through `apply_font`, so the gutter's numbers grow with the text. A page's width and margins grow with it, which keeps its line breaks.
- **Page size:** a point of paper is 96/72 dips, so a Letter text area is 624 dips wide at 100%.
- **Page gaps:** the text's layout has no top margin and no gaps between pages, so the paper is one continuous sheet. The hook needs a change before these can show.

## Tests

- `cargo test -p qe-views --locked` runs the behavior tests against the real editor, with a fake clock.
- `cargo test --locked --manifest-path crates/qe-views/check/Cargo.toml` runs the tests that need `qe-render`, `qe-buffer` and `qe-pdf` together: partial against full repaint, pictures, PDF against page line breaks, and mapped files. They sit in a workspace of their own so that the root `Cargo.toml` and `Cargo.lock` do not change. Set `CARGO_TARGET_DIR` to the root `target` to save disk. Pictures go to `QE_CHECK_OUT`, or to a folder under temp.
- Timings run by hand, with `cargo test --release --manifest-path crates/qe-views/check/Cargo.toml --test measure -- --ignored --nocapture --test-threads=1`.
- `check/tests/font_identity.rs` is ignored until `face_key` in `crates/qe-layout/src/fonts.rs` tells two files of one family apart. Windows and Windows Terminal each bring a Cascadia Mono, with different glyph numbers, and both get one font id. Arabic in that font then draws as wrong letters, on screen and in a PDF. The fix adds `face.GetGlyphCount()` to the key. Remove the `#[ignore]` with that fix.
