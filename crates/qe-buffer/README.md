# qe-buffer

UTF-8 text storage for QuickEdit for Windows.

- `PieceStore` is the editable store.
- `UndoHistory` records edits to a `PieceStore`.
- `MappedStore` is the read-only store for very large files.

## PieceStore

The store is an immutable balanced tree. Each leaf owns up to 4 KiB of text. A snapshot is a copy of the root pointer, so it costs almost nothing and never changes.

- **Balance.** The two children of a branch differ in height by at most 1. `check_tree` walks the tree and proves this and every total. It takes time in proportion to the tree, so it is for tests.
- **Merging.** After an edit, the leaves that meet at each edge merge when they fit in 4 KiB. Typing copies one leaf of at most 4 KiB.
- **Memory.** A leaf owns its text, so deleted text is freed once no snapshot and no undo step holds it.
- **Empty edits.** An edit with an empty range and empty text changes nothing. The revision stays the same and redo survives.

## UndoHistory

An edit records its place, the length of its text in the document, and a tree slice of the text it replaced. Undo swaps those, so no text is copied. A typed run is one edit, however long.

- **Direct edits.** They stay possible, so the history refuses to work after one. `undo`, `redo` and `replace` return `EditError::HistoryOutOfStep` and change nothing. `reset` starts the history again. A copy of the store that was edited, or another store, counts as an outside edit.
- **All or nothing.** A failed undo or redo changes neither the store nor the history.
- **What changed.** `undo` and `redo` return a `Vec<TextChange>` in the order applied. Each change uses the coordinates that the change before it left. An empty list means there was nothing to do.
- **Saved state.** `save_point` marks the text. `is_at_save_point` is true when history leads back to that mark. It follows history and does not compare text, so typing a letter and then deleting it by hand counts as a change.
- **Groups.** Edits between `begin_group` and `end_group` are one undo step. Replace All uses this.
- **Size limit.** `DEFAULT_UNDO_LIMIT_BYTES` is 256 MiB, and `with_limit` sets another. Past the limit, the oldest undo groups go first and then the furthest redo groups. The newest undo group and the next redo group always stay, even when one alone is over the limit. The charge counts text shared with the document, so it can run high.
- **Key press cost.** Clearing redo or dropping old groups moves them to a list. Each edit frees at most 256 tree nodes from that list. `release_memory` frees the rest, so call it when idle.

A direct `PieceStore::replace` frees removed text at once, on the calling thread. An editor goes through `UndoHistory`, which does not.

## MappedStore

The store reads with ReadFile and keeps at most 4 pages of 64 KiB. It does not use a live memory mapping, because Windows then stops other apps from truncating the file.

`open` indexes the first 64 KiB. A worker thread with its own file handle indexes the rest, 4 MiB at a time. A checkpoint every 64 KiB holds running totals of bytes, UTF-16 units and line breaks. A lookup does a binary search on the checkpoints and then scans one block. A block spans at most 2 pages, so `line_range` reads at most 4.

### Which query to use

| Query | Past the indexed part | After a failure |
| --- | --- | --- |
| `try_*` | `Err(NotYetKnown)`, at once | `Err(Failed)`, at once |
| Plain `Option` lookups on a live store | Waits for the worker | `None`, at once |
| Plain `Option` lookups on a snapshot | `None`, at once | `None`, at once |
| `len_utf16`, `line_count` | The total known so far | The total known so far |
| `status` | Gives the known totals and `complete` | Gives the failure |

The UI thread must use `try_*`. It never waits for the worker. An offset inside the indexed part always answers without waiting for the index, and at most it reads its own page. `len_utf16` and `line_count` cannot report anything, so use `status`, `try_len_utf16` or `try_line_count` when the difference matters.

A wait on the worker does no indexing on the calling thread. It ends as soon as the worker fails.

### Failures

A failure stays until the file is opened again. `open_with_notify` runs a callback after the index grows and on a failure, so a window can post itself a message. This is how a worker failure reaches a UI that is not asking at that moment.

| What happened | `valid_up_to` | What still works |
| --- | --- | --- |
| The file changed, or was deleted, renamed or replaced | 0 | Nothing. Old offsets no longer describe the file. |
| A read failed | 0 | Nothing. |
| A byte is not valid UTF-8 at offset X | X | Lookups before X, because the file itself has not changed. Totals for the whole file fail. A line that has no known end fails. |
| Both | 0 | Nothing. The smallest `valid_up_to` wins. |

A bad byte in the first 64 KiB makes `open` return an error, since that file is not UTF-8 text to show as is.

### Snapshots are stable

`TextStore::snapshot` pins the index as it stands. Its line count, totals and revision never move. It never blocks. A lookup past the pinned part gives `NotYetKnown` or `None`, so take a new snapshot to see more. A snapshot holds the file open but does not keep the worker running. A failure of the file still reaches it.

The live store's `revision` goes up each time the index grows, so a cache keyed on revision notices.

### Finding out that the file changed

A check compares the file at the path and the handle we hold with what `open` saw: file id, volume, length, creation time, last write time and NTFS's change time.

The check opens the path and asks the system twice, so its cost is bounded this way:

- `visit_chunks` checks at its start and at its end, every call.
- Other lookups check when 25 ms have passed since the last check. A lookup can therefore answer from the old file for up to 25 ms.
- The worker checks after each 4 MiB read.
- `check_file` checks at once.

### What this cannot detect

A same-size rewrite that restores the modified and creation times is detected, because the change time moves. The test `a_same_size_rewrite_that_restores_the_times_is_detected` shows this. These cases can still mix old and new bytes:

1. A program that also restores the change time, through `SetFileInformationByHandle` or `NtSetInformationFile`. The test `a_rewrite_that_forges_the_change_time_is_not_detected` shows new bytes coming back under the old index.
2. A write inside the time since the last check. Text delivered by `visit_chunks` is covered by its check at the end, which returns false, but the callback has already seen the text.
3. A writer that keeps its handle open, on a file system that delays the time updates until the handle closes.
4. File systems with no change time, such as FAT and some network shares. There, the checks fall back on the other fields.

A cure for all of these needs a hash of the whole file on each read or a share mode that locks other writers out. The first is too slow, and the second would stop other apps from saving. So the limit stays.

Invalid UTF-8 from such a rewrite never reaches a caller as text, since every chunk is validated before it goes out.

### Stopping

The worker stops when the last live `MappedStore` for the file goes away. It checks between 4 MiB scans. The drop does not wait for the worker, so the file closes one scan later at most. A snapshot holds its own handle until it goes away too.

If the OS refuses to start the worker thread, `open` returns the error. `PieceStore::build_async` does the same.
