# QuickEdit for Windows: design spec

For builders. Exact values live here. Report every departure from this file; root updates it in the same commit as the code. The copy target is the Mac app in `wizdes/QuickEdit`. When this file says nothing, do what the Mac app does.

## 1. Platform
- Windows 11 x64 only. Rust 1.99.1, edition 2024, `windows` crate 0.62.2. No UI framework, no WinUI, no .NET. No outside crates without root's OK.
- One exe. Per-monitor DPI aware (V2). `panic = "abort"` in release.
- Only root edits the root `Cargo.toml` and `Cargo.lock`. A crate may add `windows` features in its own `Cargo.toml`.

## 2. Crates and who owns them
| Crate | Milestone | Job | May depend on |
| --- | --- | --- | --- |
| `qe-api` | M1 | Shared types and traits only. No Windows calls. Frozen after M1; a change needs root | nothing |
| `qe-buffer` | M2 | Piece tree, undo, line index, UTF-16 positions, read-only mapped files | `qe-api` |
| `qe-codec` | M3 | Encodings, BOM, line endings, lossy checks, atomic save | `qe-api` |
| `qe-layout` | M4 | DirectWrite shaping, line cache, wrap, hit testing | `qe-api` |
| `qe-render` | M5 | Turns a `DisplayList` into pixels, on a window or a bitmap | `qe-api` |
| `qe-shell` | M6 | Top-level window, title row, Snap, tab strip | `qe-api` |
| `qe-ui` | M7 | Painted widgets, popup list, dialogs | `qe-api` |
| `qe-settings` | M8 | Command table, Settings window, Keyboard recorder, settings file | `qe-api`, `qe-ui` |
| `qe-editor` | M9 | Caret, selection, typing, clipboard, scrolling | `qe-api`, `qe-buffer`, `qe-layout` |
| `qe-ime`, `qe-spell` | M10 | TSF text store; `ISpellChecker` | `qe-api`, `qe-editor` |
| `qe-a11y` | M11 | UI Automation providers | `qe-api`, `qe-ui`, `qe-editor` |
| `qe-tools` | M12 | Find, Replace, Select Line, Transformations, Complete | `qe-api`, `qe-ui`, `qe-editor` |
| `qe-views`, `qe-pdf` | M13 | Gutter, map, Wrap to Page, zoom; PDF export | `qe-api`, `qe-layout`, `qe-editor` |
| `qe-docs` | M14 | Documents, tabs, windows, autosave, restore, reload, handoff | all feature crates it needs |
| `qe-app` | M14, M15 | `main`, joining the parts | everything |

## 3. The frozen interfaces (`qe-api`)
Written in M1. Plain data and traits, so that each crate can be tested with no window.
- `TextStore`: read and edit text by byte offset; `len_bytes`, `len_utf16`, `line_count`; convert between byte offset, UTF-16 offset and (line, column); `line_range`; read a slice as chunks without copying the whole document; `is_read_only`. All queries are O(log n) or better. Line breaks follow the Mac rule: LF, CR, CRLF, U+0085, U+2028, U+2029.
- `Layout`: for a line and a width, give rows, glyph runs, height, and hit tests from point to offset and offset to point.
- `DisplayList`: an ordered list of draw commands in device-independent pixels: fill rect, rounded rect, line, glyph run, text run, icon, clip push and pop, opacity push and pop. `qe-render` draws these. Nothing else draws pixels.
- `InputEvent`: key, char, mouse, wheel, focus, DPI change, with a time stamp. Scripts of these drive every replay test.
- `Widget` tree nodes: id, bounds, role, name, value, states, children. This same tree feeds UI Automation.
- `CommandId`: one enum with every menu command. `SettingKey`: one enum with every setting and its default.
- `Theme`: the token set. Every UI color, size and font comes from here, in light and dark. A hardcoded UI value in a feature crate is a bug.
- Replay test format: a text file of `InputEvent`s and expected results, read by a test helper in `qe-api`.

## 4. Speed rules
1. Nothing proportional to document size runs on the UI thread while typing.
2. Files are read and decoded off the UI thread.
3. Launch creates only the first window. Settings, dialogs, the spell checker and every optional COM object load on first use.
4. Layout only what is on screen, plus a small margin.
5. No timers, polling or animation at idle. The caret blink stops after the window loses focus.
6. Import Windows functions lazily where a DLL would otherwise load at launch and is not needed for the first frame.

## 5. Tests
- Saved-image tests name their fonts directly, because system font fallback differs per user.
- Each crate's tests run with `cargo test -p <crate>` and need no desktop. They work as the `agent` account over SSH.
- UI tests replay `InputEvent`s and check the `DisplayList`, or draw it to a bitmap and compare with a saved image at fixed DPI and font.
- Real windows, timing, Snap, IME and Explorer checks belong to root.
- A bug fix lands with a test that fails before the fix.
- Show errors at once: no empty error handling, no silent fallback.

## 6. Files
- `docs/plan.md`: the approved plan. `docs/STATE.md`: where things stand. `docs/parity-windows.md`: every behavior and its status. `POST_MVP.md`: follow-ups.

## 7. Top row (user decisions, 2026-11-01)
- Top row direction B: a rounded tab, as in Windows Terminal. Row 40 tall (32 maximized), tab 32 tall with 8 radius, neutral `#f0f0f0` light and `#262626` dark, and the active tab joins the text area with no rule. No file icon on tabs.
- User decision, 2026-11-02, replacing "all menus live inside the caret": a menu bar sits under the tabs, like Windows 11 Notepad, with File, Edit, Format, View and Window. Alt or F10 focuses the bar, and Alt+letter opens a menu. A later decision the same day removed the caret: the top row is tabs, `+` and window buttons; Settings is the last row of File; About and Quit are gone.
- The caret button was removed on 2026-11-02, so the caret and separator values in this rule are history; `+` is now one 31-wide button with four round corners. The `+` must look very close to Windows Terminal's (user request). Values from its source (`TabRowControl.xaml`) and WinUI's `SplitButton.xaml`: a split button 24 tall with margin 0,4, each part 31 wide, a 1 wide separator, `+` is U+E710 at 12 px, centered in its part, and both glyphs are in Segoe Fluent Icons. The caret is ChevronDownSmall U+E96E at 10 px in grayscale, which gives 8 x 5 ink, the same as a live Windows Terminal at 100%. Its 12 x 12 box is right aligned with padding 0,0,12,0, so that its center is at x = 13 of the 31-wide part. Its color at rest is `TextFillColorSecondary` (`#9E000000` light, `#C5FFFFFF` dark), as Terminal does not override it. The separator is `ControlStrokeColorDefault` (`#0F000000` light, `#12FFFFFF` dark). Hover and pressed are plain fills with only the outer corners round, and the `+` fill also covers the separator's column, 32 wide. Window buttons 46 wide use U+E921, U+E922 (U+E923 when maximized) and U+E8BB at 10 px. M6 proves this with a side-by-side picture against a real Terminal window.

## 8. Renderer (decided by measurement, 2026-11-01)
Arm a won the race: DirectWrite to a bitmap, then BitBlt, with no GPU. See `docs/research/renderer-race.md`. No D3D, D2D, DXGI or DirectComposition DLL loads in the normal path.
