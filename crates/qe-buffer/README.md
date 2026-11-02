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
- **Byte order mark.** `PieceStore::new` keeps exactly what it is given. `qe-codec` strips a UTF-8 BOM before text gets here.

### Freeing big text

Dropping the last holder of a big tree frees all of it on that thread. A direct `PieceStore::replace` frees removed text at once. An editor avoids both costs on the UI thread this way:

1. It edits through `UndoHistory`, which frees text a little at a time.
2. A snapshot goes to a worker, and the worker drops it. The UI thread never drops the last copy of a snapshot from before a big deletion. If it would have to, it sends that copy to a worker thread to drop. This crate does not enforce this rule, and no test proves that the editor follows it.

## UndoHistory

An edit records its place, the length of its text in the document, and a tree slice of the text it replaced. Undo swaps those, so no text is copied. A typed run is one edit, however long.

- **Binding.** `UndoHistory::for_store` binds the history to a store and counts that store's text as saved. The editor uses this. `new` and `with_limit` bind at the first recorded edit. Until then `is_at_save_point` is false and `save_point` marks nothing, because there is no store to compare with.
- **Direct edits.** They stay possible, so the history refuses to work after one. `undo`, `redo` and `replace` check this first and return `EditError::HistoryOutOfStep`, with nothing changed, even for an empty edit or a bad range. `reset` starts the history again. A copy of the store that was edited, or another store, counts as an outside edit.
- **All or nothing.** A failed undo or redo changes neither the store nor the history.
- **What changed.** `undo` and `redo` return a `Vec<TextChange>` in the order applied. Each change uses the coordinates that the change before it left. An empty list means there was nothing to do.
- **Input methods.** `replace_tracked`, `undo_tracked` and `redo_tracked` return `TrackedChange`, which adds a `Utf16Change` with the start, the old end and the new end in UTF-16 units. TSF's `OnTextChange` takes those three. A direct `PieceStore::replace` gives bytes only, so a caller that edits directly reads `byte_to_utf16` for the start and the old end before the edit, and for the new end after it.
- **Saved state.** `save_point` marks the text. `is_at_save_point` is true when history leads back to that mark and the store is the bound one. It follows history and does not compare text, so typing a letter and then deleting it by hand counts as a change.
- **Groups.** Edits between `begin_group` and `end_group` are one undo step. Replace All uses this. `undo` and `redo` return `EditError::GroupOpen` while a group is open, and change nothing. `save_point` closes any open group.
- **Typing.** With `typing` true, an insert at the caret and a delete that ends at the caret (Backspace) continue one undo step. Typing over a selection starts a step that typing then continues. A jump of the caret, `cursor_moved`, `break_group`, `save_point`, undo and redo all end the run. A forward delete starts a new step each time. The Mac app leaves this to `NSTextView` and calls `breakUndoCoalescing` only at a save. This rule is meant to match it, but nobody has compared the two on a Mac.

### Memory rules

- **Limit.** `DEFAULT_UNDO_LIMIT_BYTES` is 256 MiB, and `with_limit` sets another. Past the limit, the oldest undo groups go first and then the furthest redo groups. The newest undo group and the next redo group always stay, even when one alone is over the limit. The charge counts text shared with the document, so it can run high.
- **Work per edit.** One edit drops at most 64 groups, so the charge can stay over the limit for a few edits. Clearing redo moves the whole list in constant time.
- **Release list.** Dropped history waits on a list. Each edit frees the larger of 256 tree nodes and 4 nodes for each leaf of text that it added. Every node was made by some edit, so the list keeps pace with the edits and stays bounded even if `release_memory` is never called. `memory_to_release_bytes` gives an estimate of the list's size. Call `release_memory` when idle to free the list at once.

## MappedStore

The store reads with ReadFile and keeps at most 16 pages of 64 KiB. It does not use a live memory mapping, because Windows then stops other apps from truncating the file.

Three threads share the store. Each file reader has its own handle, because reads on one handle wait for each other. Short plain chunk reads share a fourth handle on the caller's own thread, because they may wait for the disk anyway and a trip to another thread cost more than the read.

| Thread | What it does |
| --- | --- |
| The caller | `try_*` and `status` read memory only. Plain queries wait. |
| Worker | Indexes the file 4 MiB at a time. Stops when the last live store goes away. |
| Reader | Loads pages and runs file checks on request. Stops when the last store or snapshot goes away. |

`open` reads the first 64 KiB and keeps it in memory, so the first screen never waits. `open` does touch the disk, so run it off the UI thread. A checkpoint about every 64 KiB holds running totals of bytes, UTF-16 units and line breaks. A lookup does a binary search on the checkpoints and then scans one block. A block spans at most 2 pages, so `line_range` reads at most 4. The line breaks of the 4 latest blocks stay in memory, so a frame of nearby lines scans once.

### Which query to use

| Query | Page not in memory | Past the indexed part | After a failure |
| --- | --- | --- | --- |
| `try_*` | `Err(NotYetKnown)` and a background read | `Err(NotYetKnown)` | `Err(Failed)` |
| Plain `Option` or `bool`, live store | Waits | Waits for the worker | `None` or false |
| Plain `Option` or `bool`, snapshot | Waits | `None` or false | `None` or false |
| `len_utf16`, `line_count` | Never need a page | The total known so far | The total known so far |
| `status` | Never needs a page | The known totals and `complete` | The failure |

**The UI thread must use only `try_*` and `status`. The plain queries wait for the disk and the worker, so they must not run on the UI thread.** After `NotYetKnown`, the UI waits for `Notify` and asks again. `len_utf16` and `line_count` cannot report anything, so use `status`, `try_len_utf16` or `try_line_count` when the difference matters.

`try_visit_chunks` gives the whole range in one callback from memory, or no callback at all. A range over 448 KiB gives `TooLong`, so read it off the UI thread with `visit_chunks` or `visit_chunks_while`. Plain chunk reads always come fresh from the file: each byte that leaves matched its page's hash and then matched the file, on that read.

### A very long line

`try_known_line` gives the known part of a line, with `complete` false until the index finds the line's end. This lets a very long line paint before the index ends: read the known part with `try_visit_chunks`. `try_position_to_byte` needs only the part of the line up to the column, so it answers on an unfinished line. `try_line_range` still gives `NotYetKnown` until the line's end is known.

### Failures

A failure stays until the file is opened again.

| What happened | `valid_up_to` | What still works |
| --- | --- | --- |
| The file changed, was deleted, renamed or replaced | 0 | Nothing. Old offsets no longer describe the file. |
| A page's bytes differ from its hash | 0 | Nothing. |
| A read failed with anything but a lock error | 0 | Nothing. |
| Another app held a lock for over 30 s | 0 | Nothing. |
| A byte is not valid UTF-8 at offset X | X | Lookups before X, because the file itself has not changed. The worker goes on indexing up to X, even when a chunk read found the byte first. Totals for the whole file fail. |
| Two of these | The smallest wins | |

A bad byte in the first 64 KiB makes `open` return an error, since that file is not UTF-8 text to show as is.

**Locks are not failures at first.** A lock error (Windows error 33) or a sharing error (32) means "try again". A `try_*` query gives `NotYetKnown`, and a plain query keeps waiting. The worker retries every 50 ms. If the same lock lasts over 30 s, it becomes a failure, so that no plain query can wait for ever.

### Notify

`open_with_notify` takes a callback that runs after the index grows, a page loads or the store fails. It runs on one of the store's threads, so it must only post a message and return.

- No call comes from the work that `open` does itself.
- Calls can start as soon as the threads start, which can be just before `open` returns.
- No new call starts after the last live store is dropped. One already running may still finish. A store with only snapshots left gets no calls.

### Snapshots are stable

`TextStore::snapshot` pins the index as it stands. Its line count, totals, revision, lookups and chunk reads never move. A query past the pinned part gives `NotYetKnown` or `None`, even when the worker has gone further, so take a new snapshot to see more. A snapshot holds the file open but does not keep the worker running. A failure of the file still reaches it.

The live store's `revision` goes up each time the index grows. The index and the revision change together under one lock, so one revision always names one state.

### Finding out that the file changed

Two checks work together.

**The file check** compares the file at the path and the handle we hold with what `open` saw: file id, volume, length, creation time, last write time and NTFS's change time. It runs on the reader or the worker, never on the caller.

- A `try_*` query asks for a check when 25 ms have passed since the last one, and does not wait. A query that comes before the answer can still give an answer from pages in memory.
- A plain query waits for a check when one is due.
- `visit_chunks` of 64 KiB or more waits for a check on the reader at its start and at its end.
- A shorter `visit_chunks` checks on its own thread, right before the text leaves. It always checks the file behind the store's handle, and it checks the path too when 1 ms has passed since the last check. See "Plain chunk reads" for what that leaves out.
- The worker checks after each 4 MiB read.
- `check_file` waits for a check at once. It returns any stored failure. `request_file_check` does not wait.
- With no queries running, nothing runs, so an idle store does no work.

**The page hash** catches changed bytes that the file check misses. Every page gets a 64-bit hash (`hash.rs`, not cryptographic) the first time anything reads it. Each later read of that page, by the worker or the reader, must match. A mismatch is a failure at once, so changed bytes never reach the index, the cache or a caller. The hash costs 8 bytes per page, 131 KiB for 1 GiB, plus the time to hash each page that is read.

### Plain chunk reads

PDF export makes about 3 reads per line, so this route has to be cheap per call. Each short read does this, with no trip to another thread:

1. It takes the bytes from the page cache, loading the page once when it is not there. A loaded page always meets its hash.
2. It reads the same bytes from the file and compares them. A mismatch is a failure, so no text leaves that differs from the file.
3. It asks the OS about the file behind the handle. A shrink, a write, a delete or a replace shows there.
4. It gives out the text.

The path check opens the file by its name, which costs several times what the other steps cost together. A short read runs it when 1 ms has passed since the last check. `examples/visit_bench.rs` measures all this.

A read of 64 KiB or more works as it always has: each page comes from the file and meets its hash on the reader thread, with a whole file check on the reader at the start and at the end.

### What this cannot detect

1. **A page already in memory.** `try_*` queries answer from pages in memory. If such a page changes on disk, the answers stay right for the old file until the file check notices, and for good if the writer forged every time. The test `a_forged_rewrite_of_a_page_in_memory_is_the_stated_limit` shows this. These answers are old, never mixed, because a page in memory matches the index. Plain chunk reads compare their own bytes with the file, so they do not have this limit for the text they give. A forged change elsewhere in that page waits until something reads those bytes, or until the page leaves memory and meets its hash again.
2. **A change the file check has not reached yet, to a page in memory.** Up to 25 ms after the last check, plus the check's own time.
3. **Time steps.** File times move in steps of the system clock. A same-size rewrite in the same step as the last write can leave the times alone. Then the hash catches it only on a page that is not in memory, and the page that `open` keeps can give old answers. Plain chunk reads re-read the file, so they still catch it.
4. **A writer that keeps its handle open,** on a file system that delays the time updates until the handle closes. The hash still catches its changes on read.
5. **File systems with no change time,** such as FAT and some network shares. There, the check relies on the other fields and the hash.
6. **A hash collision.** A change gives the same 64-bit hash with a chance of about 1 in 2^64.
7. **A rename away or a replace, for under 1 ms, on a short read.** If the handle shows no change, a short read in that time gives text from the file as it was opened. That text is exact and matches the index, so it is old, not mixed. On NTFS a replace or a delete drops the old file's link count, so the handle does show those at once. A rename away, and a replace on a file system that keeps the link count, wait for the next path check.

A same-size rewrite that restores the modified and creation times is detected, because the change time moves. A rewrite that forges every time is caught by the hash as soon as a changed page is read. These limits remain:

Invalid UTF-8 from such a rewrite never reaches a caller as text, since every chunk is validated before it goes out.

### Stopping

The worker stops when the last live `MappedStore` for the file goes away. It checks between 4 MiB scans. The drop does not wait, so the file closes one scan later at most. A snapshot keeps the reader and its handle until the snapshot goes away too.

If the OS refuses to start either thread, `open` returns the error and stops the thread that did start. `PieceStore::build_async` returns the error as well.

### Paths and the byte order mark

`open` makes the path absolute, so a later change of the process's folder does no harm.

`MappedStore` gives the file's bytes as they are. A UTF-8 BOM stays in the text as one character, U+FEFF, at byte 0: 3 bytes and 1 UTF-16 unit. It does not strip the mark, because every offset is a file offset. The layer above must hide it, by starting its first line's paint at byte 3. `qe-codec` strips the mark on the route that copies into a `PieceStore`.
