# qe-docs

The document layer of QuickEdit for Windows. It opens and saves files, keeps unsaved text through a kill, follows changes made by other programs, and passes files from a second launch to the running one. It owns no window and no message loop.

```text
qe-app (part B)                          DocumentStore, on the UI thread
  menu command, key, window message  ->  a call that returns at once
  on wake, or at next_deadline       ->  poll(now), then take_events and take_view_requests
                                              |  short-lived worker threads
                                              v
                                         file read, atomic save, autosave, list writer,
                                         file checks, folder watch, pipe server
```

## The rules

- **No call on `DocumentStore` waits on disk.** The one exception is `wait_for_quit`, for `WM_ENDSESSION`.
- **No timer at idle.** `next_deadline` says `None` when nothing is due. A folder watch and the pipe server sleep in `WaitForMultipleObjects` with no timeout.
- **Nothing waits for a person.** A decision comes out as `DocEvent::Question`, and `answer` takes the choice later.
- **Errors have names.** `DocError` has a `kind` for code, a `message` for the user, and `left_on_disk` for any file of ours that stayed.

## Wiring list for part B

### At start

1. Call `handoff::start(&names, &paths, wake)` first, before loading anything else. `PipeNames::for_this_process()` gives the names.
   - `LaunchRole::HandedOff`: exit now.
   - `LaunchRole::Primary(server)`: keep `server`. Each wake, `server.take_requests()` gives files for `open_paths(.., OpenTarget::LastUsedWindow, None)`. A request with no paths means "come forward". See "Choices to check".
   - `LaunchRole::Alone { error }`: show `error`, then run as an instance of your own. It uses its own session folder, and the next launch merges the two.
2. `DocumentStore::new(StoreConfig::from_settings(&settings, wake))`. `wake` runs on a worker and must only post a message.
3. `begin_restore()`. Make no launch window yet. When `RestoreFinished { windows: 0 }` comes, call `new_window(true)`, then open the launch's files. A file opened before that lands beside the restored windows, not in them.
4. On that posted message, and when a one-shot timer set to `next_deadline()` fires: call `poll(Instant::now())`, take the events, and set the timer again. After input, set it again too, because `note_view_changed` can arm it.

### While it runs

| Event | Call | What the UI must show |
| --- | --- | --- |
| Input that may change text, an undo, a paste | `note_view_changed(doc)` after `EditorView::handle` or `run` | A `Question` may follow |
| Caret or scroll moved | `note_place_changed(doc)` | Nothing |
| A window comes to the front | `window_activated(window)` and `app_activated()` | Nothing |
| A tab is clicked | `set_active_tab(window, doc)` | |
| A window moved or resized | `set_window_frame(window, frame, show, by_user)` | `by_user` true ends the launch state |
| A tab dragged | `move_tab(doc, window, index)` | `TabsChanged` for both windows |

`view(doc)` and `view_mut(doc)` give the `EditorView`, which stays in the store. Pass it to `ViewFrame` and the rest by reference. Each view has `set_wake` set to the store's wake, so the host's `poll_layout` runs from the same message.

### File menu rows (`crates/qe-ui/src/menu_groups.rs`)

Turn rows on and off from `commands(doc)`.

| `CommandId` | Call | What the UI does then |
| --- | --- | --- |
| `New` | `new_window(false)` | Show the window, 90 by 30 from settings (parity P6) |
| `NewTab` | `new_tab(window)` | Show the tab |
| `Open` | The Open dialog with its encoding list, then `open_paths(&paths, OpenTarget::Window(current), Some(choice))` | A tab shows at once and fills in. `OpenFailed` shows its `error` |
| `OpenRecent` | `open_paths(&[path], OpenTarget::Window(current), None)`, with the path from `MenuView::take_chosen_data` | `OpenFailed` for a missing file |
| `ClearRecent` | `clear_recent()` | On `RecentChanged`: `set_recent_files(model, &recent_menu_rows(store.recent()))` |
| `Close` | `request_close(doc)`. For the title bar's X: `request_close_window(window)` | Questions, one at a time |
| `Save` | `save(doc)` | `SaveFinished`, `SaveFailed`, a question, or `NeedsSavePanel` |
| `Duplicate` | `duplicate(doc)` | The new tab is beside its source |
| `SaveAs` | `request_save_as(doc)`, then the dialog, then `save_as(doc, path, encoding)` | Ask about replacing a file in the dialog. Run the typed name through `filename_with_txt_rule(name, box)` first |
| `Rename` | Ask for the name, then `rename(doc, name)` | `FileMoved` or `MoveFailed` |
| `MoveTo` | The dialog, then `move_to(doc, path, replace)` | `FileMoved` or `MoveFailed` |
| `SaveAll` | `save_all()` | Each returned error, then each `NeedsSavePanel` as it comes |
| `RevertToSaved` | `request_revert(doc)` | `ConfirmRevert`, then `TextReplaced` |
| `MoveTabToNewWindow` | `move_tab_to_new_window(doc)` | Window menu |
| `MergeAllWindows` | `merge_all_windows()` | Window menu |
| `PreventEditing` | `set_prevent_editing(doc, on)` | Format menu. The row shows `prevent_editing(doc)`, and is off when `commands(doc).prevent_editing` is false |
| `WrapToPage` | `set_wrap_to_page(doc, on)`, then `ViewFrame` | Format menu. Read `wrap_to_page(doc)` when a tab first shows (parity I55) |
| A file dropped on a window | `open_paths(&paths, OpenTarget::Window(window), None)` | As Open (parity P72) |
| App close | `begin_quit()`. On `ReadyToQuit`: `settings.flush()` and exit | A failed copy raises `AutosaveFailedAtQuit` first |
| `WM_ENDSESSION` | `wait_for_quit(limit)` | Nothing can be asked then |

### Before Save

- A composition is not in the text, and `flush_input` does not end it. Ask TSF to commit it before you call `save`, `save_as`, `save_all` or `begin_quit`.
- For a Save dialog's encoding list, call `check_encodings(doc, list)`, and turn off what `EncodingFit` names. Past the Mac's limit it names nothing.

### Events to show

| Event | What to do |
| --- | --- |
| `Question` | A dialog with `title`, `detail` and a button for each of `choices`. Then `answer(id, choice)`. Show one at a time, from `open_questions()` |
| `NeedsSavePanel(request)` | The Save dialog: `suggested_name`, `folder`, `initial_encoding`, the list off if `encoding_fixed`, the ".txt" box from `add_txt`. Then `save_as` or `panel_cancelled(doc)` |
| `SaveFailed { error, offer_save_as }` | The message, with a Save As button when asked. If `error.left_on_disk` has a file, name it |
| `SaveFinished { other_version, backup_kept }` | If either is set, name that file |
| `AutosaveFailed` | A note in the window. It says this once for each kind of failure |
| `DocChanged`, `TextReplaced` | Read `display_name`, `title_note`, `is_dirty`, `is_large`, `is_too_large_to_edit`. Turn spelling, substitutions, gutter and map on or off (parity I51, I52, I64) |
| `Reloaded`, `ReloadFailed`, `FileMoved`, `MoveFailed`, `FileDeleted`, `UnlockFailed` | The title follows. Show the error kinds |
| `RestoreProblem`, `RestoredOverNewerFile`, `WatchUnavailable`, `SessionWriteFailed` | A note in plain words |
| `LeftoverFound { path, leftover }` | Offer it. `recover_leftover(leftover, path)` puts the old text back, then `LeftoverRecovered` says to open `path`. `qe_codec::delete_leftover` removes it |
| `take_view_requests()` | Pass each to the view's host, as with any `ViewContext` |

## Open

The encoding order is: the Open dialog's choice for this file, then the document's earlier choice, then the Open setting, then `qe_codec`'s Automatic steps: BOM, strict UTF-8, the system code page, lossy UTF-8. A revert and a reload use the same choice. Windows has no encoding tag on a file, so after a Save As in another encoding the document remembers that encoding, and the session keeps it.

| Size | What happens |
| --- | --- |
| Up to 50 MB | Normal |
| Over 50 MB | `is_large`. The title has "Large file: spelling and substitutions off". Text stays editable |
| Over 512 MiB | Read-only through `MappedStore`. The title has "Read only: file too large to edit (600 MB)". No save over it, no autosave |

The mapped store shows a file's own UTF-8 bytes only. A large file that is not plain UTF-8 reads through a UTF-8 copy in `%LOCALAPPDATA%\QuickEdit\large`. That covers another encoding, a BOM, and a bad byte the index finds later, past the first 64 KiB, which restarts the load with a copy. The copy goes when the document closes, or at the next launch after a kill.

A file already open comes forward instead of opening twice. Path text catches most cases at once. Volume and 128-bit file number, read on the worker, catch hard links, junctions and other spellings.

## Save

```text
save -> flush_input -> worker_snapshot + revision -> worker: encode, atomic_write_if_unchanged
     -> UI: mark_save_point_at(revision), new stamp
```

- Typing during a write stays in the view and is dirty. The file holds the known snapshot. As `HOOKS.md` says, Undo back to the written text still reads as unsaved until the next save.
- A second Save during a write runs at its end.
- `is_dirty` means that the view is away from its save point, or that disk does not hold the text: a restored copy, a duplicate, or a deleted file.

## Autosave and restore

```text
%LOCALAPPDATA%\QuickEdit\session\session.lock   held open while this copy uses the folder
                         session\session.txt    windows and tabs, replaced whole
                         session\docs\<key>.<n>.qea  the unsaved text, generation n
                         recent.txt             Open Recent, 10 rows
```

- The delay comes from `AutosavingDelaySeconds`, counted from the first unsaved edit, so typing does not push it back. Zero turns the timer off, and a quit still writes.
- A document with no unsaved change writes no file. Undo back to the save point, a Save, and Don't Save each remove the copy.
- A generation goes to a `.tmp` name, is flushed, and is renamed. Only then does the older one go. A kill at any moment leaves a whole `.qea`. Its length and hash are checked on restore, and a damaged one goes to `.damaged` and the next older is tried.
- A `.qea` describes itself. So a kill between its rename and the list, or a damaged list, still brings the text back.
- Autosave streams chunks of a snapshot from a worker, so the UI thread pays only for the snapshot. A 500 MiB document costs that much disk each time.
- Read-only files have no copy: they cannot change. They come back by their path. A read-only copy with no file comes back from the file it copied.
- A file newer than its autosave keeps the autosaved text in the window, sends `RestoredOverNewerFile`, and asks at Save. A file gone while away comes back with `Deleted` in its title, and a Save writes it again.
- A blank untitled document stays off the list and has no copy, as on Mac v1.2.0.
- A second copy that cannot take `session` uses `session-2`. The next launch with no holder merges them.

## Outside changes

The choice: **`ReadDirectoryChangesW` on one thread for each folder, plus a check at activation.** The watch reloads a clean document while another app has the focus, as the Mac's file presenter does, which a check at activation alone could not. It needs no timer. The check at activation covers places the watch cannot hear, such as some network shares, and each new watch makes one check of its own, for a change that came before it started.

| What happened | Clean document | Dirty document |
| --- | --- | --- |
| Bytes changed | Reload. Caret and scroll stay, moved inside the text. Undo goes | No reload. Save asks `ChangedOnDisk` |
| Date only, or same text | Text, caret and undo stay. The stamp moves | Same |
| Replaced by rename | As bytes changed | As bytes changed |
| Renamed or moved on the volume | The document follows, by the rename event and by file number | The same, and it stays dirty |
| Deleted, or in the Recycle Bin | Stays open, `Deleted` in the title, counts as unsaved, and a Save writes it again | The same |
| Deleted then created within 300 ms, as some editors save | A reload, with no word about a deletion | As bytes changed |

Edits are not refused during the read of a reload. See "Mac rows changed".

## File handoff

A launch writes a length-counted list of paths to `\\.\pipe\QuickEdit-<user>-<session>-<level>`. The server thread puts them in a queue, wakes the UI and answers one byte, without waiting for the UI.

| Case | Result |
| --- | --- |
| No instance | `Primary`. The name is created with first-instance rights, so two launches at once cannot both win, and the loser hands off |
| Killed instance | Windows removes the pipe, so the next launch is `Primary` |
| Hung instance | `StaleInstance` within about 4.5 s, then `Alone` |
| Instance at another integrity level | `DifferentIntegrity`, then `Alone`, because each level has its own pipe name |
| A launch that connects and stalls | Dropped after 1 s, and the next launch is served |

## Tests

```text
cargo test -p qe-docs --locked
cargo test -p qe-docs --release --locked --test release_budget -- --nocapture
cargo run -p qe-docs --release --locked --example handoff_bench
```

`tests/kill.rs` starts this test binary as a child and kills it for real. No test shows a window, touches the clipboard, or uses the user's folder.
