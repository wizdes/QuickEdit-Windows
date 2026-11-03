# qe-settings

This crate holds the settings store, the key table, the Settings window and its Keyboard recorder. Startup uses only the store: one small text file, read and resolved in under 1 ms. The window's code runs when the user opens Settings. Nothing here uses a timer, and no thread runs at idle.

## The wiring list for M14

1. **Open the store once, at startup.** A problem with the file is for the user to see.

   ```rust
   let path = default_settings_path()?;            // %LOCALAPPDATA%\QuickEdit\settings.txt
   let store = SettingsStore::open(path);          // never fails: bad input gives defaults
   if let Some(problem) = store.load_problem() { /* show it */ }
   ```

2. **Keys and menus.** Give the store to the menu builder, and look up each key press.

   ```rust
   let groups = menu_bar_groups_with_keys(&|command| store.key_of(command));
   let command = store.command_for_press(virtual_key, modifiers); // Option<CommandId>
   ```

3. **Read settings when a window is made.** New windows follow changes, and open windows keep what they had. Line numbers are the exception.

   ```rust
   store.get_integer(SettingKey::WindowWidthCharacters);
   store.get_bool(SettingKey::ShowDocumentMap);
   store.font();                                    // FontChoice { family, points }
   store.encoding_choice(SettingKey::OpenEncoding); // EncodingChoice
   store.enabled_encodings();                       // Vec<TextEncoding>
   store.paper_size();                              // the saved size, or the region's
   ```

4. **Listen for changes.** This is how line numbers apply at once and how menus show a new key.

   ```rust
   store.subscribe(|change| match change {
       SettingChange::Value(SettingKey::ShowLineNumbers) => { /* update every open window */ }
       SettingChange::Keys => { /* rebuild the menu models */ }
       SettingChange::WriteFailed(message) => { /* show it, if Settings is closed */ }
       _ => {}
   });
   ```

5. **Hear about writes while Settings is closed.** A write runs on its own short-lived thread. A wake callback posts a message to a window of yours, and that message's handler calls `poll_writes`. The open Settings window does this for itself.

   ```rust
   store.add_write_wake(Arc::new(move || post_to_main_window()));
   // On that message:
   store.poll_writes();
   ```

6. **At exit,** call `store.flush()`. It waits for the writer. This is the only wait, and it must not run while the user is working.

7. **Give the window what it needs to paint,** with `SettingsHost`. `qe-settings` links no renderer.

   ```rust
   impl SettingsHost for AppHost {
       fn measurer(&self) -> Arc<dyn TextMeasurer> { /* qe_layout::DirectWriteLayout */ }
       fn theme(&self) -> Theme { /* the current theme, also for high contrast */ }
       fn make_renderer(&self, w: u32, h: u32, dpi: u32) -> Result<Box<dyn Renderer>, String> {
           /* qe_render::BitmapRenderer::new(w, h), then set_dpi(dpi) */
       }
       fn window_message(&self, hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
           /* answer WM_GETOBJECT through qe-a11y, from SettingsWindow::widgets, or from
              encoding_panel_widgets for that panel's window */
       }
       fn announce(&self, hwnd: HWND, text: &str) { /* raise a UI Automation notification */ }
   }
   ```

   `crates/qe-settings/pictures/tests/pictures.rs` has a working host for the first three.

8. **Open it from `CommandId::Settings`.** Keep one `SettingsWindow` for the app's life. A second `open` brings the open window forward.

   ```rust
   let mut settings = SettingsWindow::new(store.clone(), Rc::new(AppHost));
   settings.open()?;
   ```

9. **The message loop must leave this window's keys alone.** Do not call `IsDialogMessage` or `TranslateAccelerator` for `settings.hwnd()`. Settings takes Tab itself, and the recorder must see every key, such as Ctrl+O.

10. **Process DPI.** The window sizes itself from `GetDpiForWindow`. The app's manifest sets per-monitor DPI awareness, as for its other windows.

## What the keys may be

A row's key is refused, with a message, in these cases:

| Case | Example | Message |
| --- | --- | --- |
| Windows owns it | Alt+F4, Alt+Tab, Alt+Esc, Alt+Space, Ctrl+Esc, F10, Shift+F10, any Windows key chord | "Alt+F4 is kept by Windows." |
| It edits text | Ctrl+Left, Ctrl+Backspace, Ctrl+Insert, Shift+Insert | "Ctrl+Left is the text-editing key for "Move Word Left"." |
| It opens a menu | Alt+F, Alt+E, Alt+O, Alt+V, Alt+W | "Alt+F opens the File menu." |
| It is AltGr on many keyboards | Ctrl+Alt+E | "Ctrl+Alt+E types a character on many keyboards, ..." |
| It has no modifier | K, Shift+K | "A shortcut needs Ctrl or Alt, unless it is a function key." |
| Another command has it | Ctrl+F | "Ctrl+F is already used by "Find" in the Edit > Find menu." |

Some of the Windows keys never reach an app, such as Alt+Tab, so the recorder cannot show that message for them. They are on the list so that a settings file naming one is refused. While a key is being recorded, Alt+F4 is refused and the window stays open. Otherwise it is a normal close, and no table row holds it.

## The file

- One UTF-8 text file, with only the settings that differ from their defaults: `WindowWidthCharacters=120`, `KeyboardShortcut.Find=Ctrl+G`, `KeyboardShortcut.Save=none`.
- Comments, blank lines, unknown keys and shortcuts for unknown commands stay on save.
- A save writes a temp file beside the target, flushes it, and renames it over the target in one step with `MoveFileExW`. A reader finds the old file or the new one, never no file, and a crash leaves one of them whole. `ReplaceFileW` is not used, because it leaves a moment with no file at this name. A read-only file is an error, and the error is shown in the window.
- The next save removes temp files that a crash left, by their exact name (`settings.txt.tmp-<process>-<number>`), once they are a minute old.
- A file that another program holds is tried again: about 50 ms when opening, about half a second when saving. Only then is it an error. A missing file really means a first run.
- A file that cannot be read as settings is copied to `settings.broken.txt` (then `settings.broken-2.txt`, and so on). The app starts with defaults, and `load_problem` says so. One bad value defaults only that setting, with the same copy and message.
- A broken file with the same content as the newest copy makes no new copy. Five copies stay at most: the oldest copy this code made goes first, and the count never stops a save. The launch thread does not flush a copy. The writer flushes it before the first save may replace the original.
- A file over 1 MB is not read and not copied, because a copy could take any time at launch. No save replaces it, and the message says to move or rename it.
- If no copy can be made, or the file cannot be read at all, no save replaces it. Each save then reports why.
- Two running QuickEdit processes do not merge: the last write wins, whole file.

## The footer

The footer shows everything wrong now: a write error, a problem the window met, a paint problem and the load problem. Text that fits stays beside Restore All Defaults. Longer text gets its own lines under that button, the window's width, and the window grows to hold them. The next action on a control clears a problem the window met, and the next good paint clears a paint problem.

A window that must size itself calls `SettingsPanel::fit_to_text`, then `wanted_height`. `layout` measures too.

## Tests

```text
cargo test -p qe-settings --locked
cargo test -p qe-settings --release --locked --test speed -- --nocapture
cargo test -p qe-settings --release --locked --test durability -- --nocapture
cargo test --locked --manifest-path crates/qe-settings/pictures/Cargo.toml --target-dir target
```

`pictures/` is a small package apart from the workspace, because `qe-settings` has no `qe-render` line in the root `Cargo.lock`. Update its saved images with `QE_SETTINGS_UPDATE_IMAGES=1`, then look at each one. Every window in every test stays hidden, and tests set their own settings path.
