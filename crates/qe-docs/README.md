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
   - `LaunchRole::Primary(server)`: keep `server`. Each wake, `server.take_requests()` gives files for `open_paths(.., OpenTarget::LastUsedWindow, None)`. A request with no paths means "come forward". See "Choices to check". Show each error in a request's `refused`. Show what `server.take_events()` gives: `ServerRestarted` is a note, and `ServerStopped` says later launches each run alone.
   - `LaunchRole::Alone { error }`: show `error`, then run as an instance of your own. It uses its own session folder, and the next launch merges the two.
2. `DocumentStore::new(StoreConfig::from_settings(&settings, wake))`. `wake` runs on a worker and must only post a message.
3. `begin_restore()`. Make no launch window yet. Pass the launch's files and every request to `open_paths` at once: the store holds them until the restore ends, and then they join the last used window. So requests from before and after the restore share one window. When `RestoreFinished { windows: 0 }` comes, nothing waited, so call `new_window(true)`.
   - `LastWindowGone { opened_nothing: true }` means the last window went because its file failed to load. This can come after `RestoreFinished { windows: 1 }`. Call `new_window(true)`.
   - `LastWindowGone { opened_nothing: false }` means the user closed it. Do what the app does then.
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
| `Close` | `request_close(doc)`. For the title bar's X: `request_close_window(window)` | Questions, one at a time. A tab still restoring asks too, as its text is in its copy |
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
| App close | `begin_quit()`. On `ReadyToQuit`: `server.stop_and_take(limit)`. A request in its answer means the launch heard "taken", so call `stop_quit_for_new_files()`, open its files and listen again. Otherwise `settings.flush()` and exit | A failed copy raises `AutosaveFailedAtQuit` first |
| `WM_ENDSESSION` | `wait_for_quit(limit)` | Nothing can be asked then |

### Before Save

- A composition is not in the text, and `flush_input` does not end it. Ask TSF to commit it before you call `save`, `save_as`, `save_all` or `begin_quit`.
- For a Save dialog's encoding list, call `check_encodings(doc, list)`, and turn off what `EncodingFit` names. Past the Mac's limit it names nothing.

### Events to show

| Event | What to do |
| --- | --- |
| `Question` | The store holds one open question for each document and kind, so a second Close adds none. A dialog with `title`, `detail` and a button for each of `choices`. Then `answer(id, choice)`. Show one at a time, from `open_questions()`, which is always current. If `answer` returns an error, such as `Busy`, nothing happened and the question stays open: show the error and show the question again |
| `NeedsSavePanel(request)` | The Save dialog: `suggested_name`, `folder`, `initial_encoding`, the list off if `encoding_fixed`, the ".txt" box from `add_txt`. Then `save_as` or `panel_cancelled(doc)` |
| `SaveFailed { error, offer_save_as }` | The message, with a Save As button when asked. If `error.left_on_disk` has a file, name it. The `doc` may have closed already |
| `SaveFinished { other_version, backup_kept }` | If either is set, name that file |
| `AutosaveFailed` | It says this once for each kind of failure. While `autosave_failure(doc)` says `Some`, keep that on show on the document's own tab: it ends when a copy succeeds or the text needs none. With `None`, show the `error` as a note. The `doc` may have closed already |
| `DocChanged`, `TextReplaced` | Read `display_name`, `title_note`, `is_dirty`, `is_large`, `is_too_large_to_edit`. Turn spelling, substitutions, gutter and map on or off (parity I51, I52, I64) |
| `Reloaded`, `ReloadFailed`, `FileMoved`, `MoveFailed`, `FileDeleted`, `UnlockFailed` | The title follows. Show the error kinds. `Reloaded` means another program changed the file, and nothing else raises it |
| `RestoreProblem`, `RestoredOverNewerFile`, `WatchUnavailable`, `SessionWriteFailed` | A note in plain words |
| `LeftoverFound { path, leftover }` | Offer it. `recover_leftover(leftover, path)` puts the old text back, then `LeftoverRecovered` says to open `path`. `discard_leftover(leftover, path)` deletes it, and `LeftoverDeleted` or `LeftoverNotDeleted` says how that went. Do not call `qe_codec::delete_leftover`, which needs a list only a worker can read |
| `OpenFailed` with `PathRefused` | The path names a device, a stream or a pipe, not a file. Show the message |
| `OpenFailed` with `TooManyFiles` | More than 4,096 files were waiting. Said once for the whole list |
| `take_view_requests()` | Pass each to the view's host, as with any `ViewContext` |

### Rules for the host

1. **Events come in order, and nothing follows `DocClosed(doc)` that needs the document.** `DocChanged` and `TextReplaced` never follow it. A `Question` not yet taken is removed when its document closes. `SaveFailed` and `AutosaveFailed` can still name a closed document, since their error is still news.
2. **Stop edits to a document while its question is open.** Input does not stop by itself, and Cancel undoes one group of edits. So an edit made during a first-edit question would go with the Cancel, or the Cancel would undo the wrong thing. Keep the dialog modal to that document's window, or send no input to that view.
3. **Edits are refused during a Revert's read.** `EditorView` refuses them from the answer until the text arrives. A reload's read does not refuse them; see "Mac rows changed".
4. **`open_paths` may return fewer ids than paths.** It makes at most 32 tabs in one call or one poll, and the rest arrive with `TabsChanged`. Read tabs from `tabs(window)`, not from the returned list. Set the timer from `next_deadline` after each poll, as step 4 says, or a long list stops until the next wake.
5. **`ReadyToQuit` means what it says.** After a failed copy, the document's question must be answered and any Save it caused must end, including its dialog and its own questions. A failed Save asks again. Cancel at any of those steps ends the quit, and the app stays open.

## Open

The encoding order is: the Open dialog's choice for this file, then the document's earlier choice, then the Open setting, then `qe_codec`'s Automatic steps: BOM, strict UTF-8, the system code page, lossy UTF-8. A revert and a reload use the same choice. Windows has no encoding tag on a file, so after a Save As in another encoding the document remembers that encoding, and the session keeps it.

| Size | What happens |
| --- | --- |
| Up to 50 MB | Normal |
| Over 50 MB | `is_large`. The title has "Large file: spelling and substitutions off". Text stays editable |
| Over 512 MiB | Read-only through `MappedStore`. The title has "Read only: file too large to edit (600 MB)". No save over it, no autosave |

The mapped store shows a file's own UTF-8 bytes only. A large file that is not plain UTF-8 reads through a UTF-8 copy in `%LOCALAPPDATA%\QuickEdit\large`. That covers another encoding, a BOM, and a bad byte the index finds later, past the first 64 KiB, which restarts the load with a copy. The copy goes when the document closes, or at the next launch after a kill.

A file already open comes forward instead of opening twice. Path text catches most cases at once, with letter case ignored past ASCII as well. Volume and 128-bit file number, read on the worker, catch hard links, junctions and other spellings.

Every path passes `check_open_path` first, which reads no disk and no network. It takes an absolute path on a local drive, or a path on a network share. It refuses a device name in any folder (`NUL`, `COM1.txt`), `\\.\` names, `\\?\GLOBALROOT` and other device roots, alternate data streams, and the `pipe`, `mailslot` and `IPC$` shares.

A path on a network share reads in its own 2 load slots, apart from the 4 for everything else, and its file checks run apart too. So a share that does not answer holds up only other shares. A drive letter mapped to a share cannot be told apart without asking the network, so it uses the 4 ordinary slots.

## Save

```text
save -> flush_input -> worker_snapshot + revision -> worker: encode, atomic_write_if_unchanged
     -> UI: mark_save_point_at(revision), new stamp
```

- Typing during a write stays in the view and is dirty. The file holds the known snapshot. As `HOOKS.md` says, Undo back to the written text still reads as unsaved until the next save.
- A second Save during a write runs at its end.
- `is_dirty` means that this document holds text not on disk: the view is away from its save point, or a restored copy, a duplicate or a deleted file holds it. A tab still loading from its copy counts. A close, a quit and a reload all decide by this, and never by the view's save point alone.

## Autosave and restore

```text
%LOCALAPPDATA%\QuickEdit\session\session.lock   held open while this copy uses the folder
                         session\session.txt    windows and tabs, replaced whole
                         session\docs\<key>.<n>.qea  the unsaved text, generation n
                         recent.txt             Open Recent, 10 rows
```

- The delay comes from `AutosavingDelaySeconds`, counted from the first unsaved edit, so typing does not push it back. Zero turns the timer off, and a quit still writes.
- A document with no unsaved change writes no file. Undo back to the save point, a Save, and Don't Save each remove the copy.
- A copy goes only after its text is safely on disk elsewhere, or after the person chose to discard it.
- A failed removal names itself, and the copy still cannot come back. The list holds a mark, `dead key=... below=N`: copies of that document below generation N hold nothing. The next launch removes them. A copy at or above N was written later, and it is live, which is what lets a kill between a copy's rename and the list still bring text back.
- If the list write fails too, or a kill comes first, the next launch compares the base inside the copy with the file. The Save changed the file, so the copy restores with a warning and a Save asks.
- A generation goes to a `.tmp` name, is flushed, and is renamed. Only then does the older one go. A kill at any moment leaves a whole `.qea`. Its length and hash are checked on restore, and a damaged one goes to `.damaged` and the next older is tried.
- A `.qea` describes itself. So a kill between its rename and the list, or a damaged list, still brings the text back.
- Autosave streams chunks of a snapshot from a worker, so the UI thread pays only for the snapshot. A 500 MiB document costs that much disk each time.
- Read-only files have no copy: they cannot change. They come back by their path. A read-only copy with no file comes back from the file it copied.
- A file newer than its autosave keeps the autosaved text in the window, sends `RestoredOverNewerFile`, and asks at Save. The session keeps that state, so the warning comes at each restore until the person saves or reverts. A file gone while away comes back with `Deleted` in its title, and a Save writes it again. If that file returns, the text and its copy stay, and a Save asks.
- An autosaved copy that cannot be read now, because it is held or memory is short, stays as it is. The document opens from its file under a new key, so nothing it does touches that copy, and the next launch tries the copy again.
- A copy with a damaged head goes to `.damaged` and is named once. No clean-up removes a `.damaged` file. See "Choices to check".
- A blank untitled document stays off the list and has no copy, as on Mac v1.2.0.
- A second copy that cannot take `session` uses `session-2`. The next launch with no holder merges them. Every file moves first, including `.damaged` files and a damaged list, then the list is written, and only then does the folder go, with its lock held to the end. A file that will not move stays with its row and its folder, and the next launch tries again. A kill in the middle cannot merge a row twice, because rows are told apart by key.

## Outside changes

The choice: **`ReadDirectoryChangesW` on one thread for each folder, plus a check at activation.** The watch reloads a clean document while another app has the focus, as the Mac's file presenter does, which a check at activation alone could not. It needs no timer. The check at activation covers places the watch cannot hear, such as some network shares, and each new watch makes one check of its own, for a change that came before it started.

| What happened | Clean document | Dirty document |
| --- | --- | --- |
| Bytes changed | Reload. Caret and scroll stay, moved inside the text. Undo goes | No reload. Save asks `ChangedOnDisk` |
| Date only, or same text | Text, caret and undo stay. The stamp moves | Same |
| Replaced by rename | As bytes changed | As bytes changed |
| Renamed aside, then a new file at the old name within 300 ms, as many editors save | The document stays at its path and reloads. The backup means nothing to it | It stays at its path, its text stays, and a Save asks |
| Renamed or moved on the volume, and nothing comes back at the old name within 300 ms | The document follows, by the rename event and by file number | The same, and it stays dirty |
| One name of a file with several hard links deleted | `Deleted` in the title. It does not move to another link | The same |
| Deleted, or in the Recycle Bin | Stays open, `Deleted` in the title, counts as unsaved, and a Save writes it again | The same |
| Deleted then created within 300 ms, as some editors save | A reload, with no word about a deletion | As bytes changed |
| The file returns after `Deleted` | Reload, under the rows above | The text and any copy stay, and a Save asks |
| A read that fails, such as a file held open | `ReloadFailed`. The stamp stays, so a later Save asks | The same |

The 300 ms is one check at a one-shot deadline, so it costs no timer at idle. A file too large to edit that has gone cannot be written again from its map, because the map refuses to read once the file has gone. Its Save row is off, a Save named in a close question fails by name, and Don't Save still works. One read through a UTF-8 copy can be written again.

A hard link is told apart by the file's count of names, which each look at the file refreshes. Any change in the folder causes a look. A link made in another folder and deleted with no look between, and no window activation, reads as a move.

Edits are not refused during the read of a reload. See "Mac rows changed". A key typed during that read stays, and the reload is dropped.

After a Save, the stamp comes from a look just after the swap. A write by another program in that instant that changes the size reads as a change, so the document reloads or a Save asks. One that keeps the same size is not seen, and closing that gap would need a stamp from inside the swap in `qe-codec`.

## File handoff

A launch writes a length-counted list of paths to `\\.\pipe\QuickEdit-<user>-<session>-<level>`. The server thread puts them in a queue, wakes the UI and answers one byte, without waiting for the UI.

| Case | Result |
| --- | --- |
| No instance | `Primary`. The name is created with first-instance rights, so two launches at once cannot both win, and the loser hands off |
| Killed instance | Windows removes the pipe, so the next launch is `Primary` |
| Hung instance | `StaleInstance` within about 4.5 s, then `Alone`. Each launch has its own server thread, so only a hung server does this, never another launch |
| Instance at another integrity level | `DifferentIntegrity`, then `Alone`, because each level has its own pipe name |
| A launch that connects and stalls, or sends one byte at a time | Dropped 1 s after it connected, for the whole message, and every other launch is served meanwhile |
| A launch that never reads its answer | Dropped 1 s after the answer. Its files were taken already |
| A message over 1,024 paths or 1 MiB of path text | Refused, and that launch runs `Alone` |
| More than 4,096 files waiting for the UI | That launch is refused and runs `Alone`, so its files are not lost |
| The server thread fails | `HandoffEvent::ServerRestarted`, and it runs again. A second failure gives `ServerStopped` |

## Mac rows changed

| Mac | Here | Why |
| --- | --- | --- |
| The views take no edits during a reload's check (`reloadAfterOutsideChange` in `TextDocument.swift`) | Edits are allowed during a reload's read, and a key typed then wins: the reload is dropped and a Save asks | A reload starts with no action by the user, and it can take a long time on a large file. A Revert does refuse edits, because the person asked for it |
| The Save row is off for a file too large to edit (`TextDocument.swift`) | The same, except for a deleted file read through a UTF-8 copy, whose Save writes it again | Its text can still be read, and it exists nowhere else |
| AppKit's file presenter reports a rename | A rename waits 300 ms before it is followed, so that a new file at the old name reads as a replacement | Windows reports the two steps apart. The Mac was not run through this sequence for a comparison |

## Choices to check

These are safe defaults, for the user to confirm or change.

1. **A second launch with no files.** A request with no paths means "come forward". This crate makes no window for it, and part B brings the last used window forward. The other choice is a new window. The Mac was not compared on this.
2. **Save over a hard link splits the links.** A Save swaps in a new file, so the other name keeps the old bytes, with no word to the user. The Mac may do the same. This keeps it, and says nothing.
3. **Undecodable bytes.** When another program writes bytes that cannot be decoded, the stamp moves, as on the Mac, so a later Save overwrites that file and does not ask. Every other failed read leaves the stamp, so its Save asks. Asking here too would be safer, and it would differ from the Mac.
4. **`.damaged` files stay forever.** Nothing removes them, so disk use can grow if copies keep being damaged. A clean-up after some days would need the user's OK, as it deletes what a message said stays.
5. **Limits.** 32 tabs per wake, 4,096 files waiting, 1,024 paths in one message, and 2 load slots for network paths. These come from no Mac rule.
6. **A cancelled Save dialog during a quit review ends the quit.** The other choice is to bring the question back. Ending the quit keeps the app open with the text in place.
7. **Reverting from a `ChangedOnDisk` question that came from a close** leaves the tab open. The other choice is to finish the close.
8. **A dropped launch past 64 at once.** A launch past that limit is dropped and runs `Alone`. Each one in progress ends within 2 s.

## Tests

```text
cargo test -p qe-docs --locked
cargo test -p qe-docs --release --locked --test release_budget -- --nocapture
cargo run -p qe-docs --release --locked --example handoff_bench
```

`tests/kill.rs` starts this test binary as a child and kills it for real. No test shows a window, touches the clipboard, or uses the user's folder.
