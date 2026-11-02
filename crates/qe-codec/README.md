# qe-codec

This crate turns file bytes into text and text into bytes, and it saves files safely.
It follows `QuickEdit/Document/PlainTextIO.swift` on the Mac.

## Opening

- A chosen encoding wins, and a code page refuses bytes that do not fit it with `InapplicableEncoding`.
- UTF-16 and UTF-32 read as on the Mac. An odd last byte or an unfinished last unit is dropped, and a lone UTF-16 surrogate shows as U+FFFD. These files open marked `lossy`. A UTF-32 value that is not a character gives `InapplicableEncoding`.
- Automatic tries these steps in order: BOM, strict UTF-8, the Windows system code page, lossy UTF-8.
- A UTF-16 or UTF-32 BOM ends the search, as on the Mac. A UTF-8 BOM waits for the UTF-8 step, so a bad UTF-8 body moves on to the next step.
- The last step marks the file `lossy` and takes any bytes. So under Automatic every file opens, except one with a UTF-32 BOM and a body that is not UTF-32.
- A file whose bytes would change on save is marked `lossy`.
- To open a file from disk, use `decode_file`. It never holds the file's bytes. See "Memory of `decode_file`".
- `decode` takes bytes already in memory, so it holds the input and the text at once. Shift JIS text made only of kanji is 1.5 times the file size in UTF-8, so `decode` peaks at about 2.5 times the file size for that text.
- `transcode_to_utf8_file` writes the text to a file and uses only a few MiB of memory.
- `decode_file` and `transcode_to_utf8_file` give `SourceChanged` if the file changes while they read it. The checks are the size, the last write time, and which file the name points to.

### Memory of `decode_file`

The first 1 MiB sets the size of one block of memory: the text that piece would give for the whole file, plus about 3%. The text fills that block in one pass, and the block never grows, because growing would hold two blocks at once.

| File | Passes | Peak memory |
| --- | --- | --- |
| UTF-8 | 1 | the file size |
| Its start is like the rest | 1 | the text plus about 3% and a few MiB |
| Its start gives less text per byte than the rest | 2, and part of a third | the text plus a few MiB |
| Its start gives more text per byte than the rest | 1 | up to the first rate, at most 3.1 times the file. The extra part is reserved and not touched |

If the first block cannot be reserved, the file is measured, and only a text that truly does not fit gives `TooLarge`.

### Progress

All the calls that report progress share these rules:

- `done` never passes `total`, and it only rises.
- A file that grows stops the call with `SourceChanged` at the first byte past `total`.
- An empty file reports `(0, 0)` once, as its finish. A caller that shows a percentage must handle a total of zero.
- `total` is the size of one file. `transcode_to_utf8_file` under Automatic is the one exception: its total is twice that, because of its scan.
- A second pass of `decode_file` holds the number still until it catches up.

## Saving

`atomic_write` does these steps:

1. It follows a link to the real file. The link stays as it is. A link to nothing is resolved as Windows resolves it: from the link's real folder, with `..` in the link's text removed as text.
2. It stops if the target is read-only, or if an access rule denies writing.
3. It stops with `NotReplaceable` if this account may not delete or rename the target. The swap in step 7 could never work, so no temp file is made. There is no route that writes in place, because a crash during such a write loses the text. The app should offer Save As.
4. It creates a temp file beside the target. The temp file has the target's access rules from its first moment, before any byte is written.
5. It writes the bytes and flushes them to disk.
6. A new name uses a rename that cannot replace. If a file appeared there, the result is `TargetExists`.
7. An existing target uses `ReplaceFileW`, which keeps the creation date, access rules, hidden mark and alternate streams. It tries about 10 times over 500 ms while another program holds the file.
8. Just before each try, it compares the target's state with what it saw in step 2. If they differ, the result is `TargetChanged`. No swap runs, the target keeps the other program's bytes, and the temp file stays at `new_text`.
9. After the swap, it compares the old file with that same state. If they differ, another program wrote in the instant after the check. The result is `TargetChanged` with `other_version`, which is the file that holds that program's bytes.
10. A failed replace is an error with the Windows code. The code checks the disk before it says that the original is intact, and an error names a temp or backup file only if it is there.

Temp and backup names are created with create-new and a random part, so a save never overwrites an older file of ours.

The backup starts as an empty reservation. Only a backup that is still empty and still that reservation is ever deleted after a failed save. Bytes in it always came from the target.

`prepare_large_copy` and `PreparedCopy::commit` do the same for a file too large to hold in memory, such as Save As on a file over 512 MiB. `copy_large_original` is the form for a new name. If the input changes during the copy, the result is `SourceChanged` and the partial copy is deleted. `PreparedCopy::discard` throws a copy away and reports a temp file that could not go. A drop does the same but cannot report.

### A change since the document was opened

1. Call `file_stamp` when the document opens, and again after each save, because a save changes the stamp.
2. Save with `atomic_write_if_unchanged`, or `PreparedCopy::commit_if_unchanged` for a large copy.
3. If the file changed since the stamp, or was deleted, the result is `TargetChanged` and nothing was replaced.

`atomic_write` alone sees only changes made after the call began.

### Failures of cleanup

A file of ours that could not be deleted is always named. Save errors have a `leftover` field for it. Any other error, such as `Cancelled`, is wrapped in `CleanupFailed`, which holds the first error and the file. `transcode_to_utf8_file` treats its `output` the same way.

## Recovery after a crash

Temp and backup files have this name: `.qe-<name hash>-<process id>-<random>.tmp` or `.bak`.

`find_leftovers` takes a folder or a document path and lists these files. It deletes nothing. M14 should call it for each document folder at launch. The app should offer recovery when `offer_recovery` is true.

| File | When `offer_recovery` is true | What the file holds |
| --- | --- | --- |
| `.bak` | It has bytes, and its process has ended | The old text. If the document is gone, this is the only copy. |
| `.tmp` | It has bytes, the document is gone, and its process has ended | New text, which may be partial |

A `.tmp` file beside an intact document is not offered, because a crash before the swap leaves the document as it was. An empty `.bak` file is a name reservation and holds nothing.

A process number can be reused. Until the new process ends, its leftovers show `owner_running` and are not offered.

`delete_leftover` deletes one listed file when the user asks. It works even if the file's copied rules deny this user the delete. It refuses with `LeftoverKept` if the name is not one of ours, if the file changed since it was listed, or if another running process made it. It also refuses a file that may hold the user's text, unless the call says `IncludeRecoverable`.

## Known limits

- **Hard links:** a save splits them. The saved name gets a new file, and other names of that file keep the old bytes. This matches the replace method.
- **EUC-JP:** code page 20932 does not write standard JIS X 0212, so `can_encode` and `encode` refuse any character that the page cannot write in standard form. Nothing changes silently. Reading is complete: this crate turns the standard three-byte form into the page's own form before the call. A file with such a character opens marked `lossy`, and a save in EUC-JP gives `Unencodable`. Bytes outside the standard form give `InapplicableEncoding`, which includes the page's own two-byte form.
- **FAT and exFAT:** these file systems have no access rules, so there is nothing to copy to the temp file. Write times on FAT move in 2-second steps, so a change inside one step can go unseen if the size stays the same. A file number may come from a folder slot, so the save compares size and time as well. This was tested only by forcing the same state on NTFS.
- **Time steps:** on NTFS the file time moves about every 16 ms. A write of the same size inside one step of the last write can go unseen.
- **Open writers:** a program that keeps its handle open may write without moving the file's time until it closes the handle. A change of the same size can then go unseen.
- **Last instant:** no check can close the gap between the last compare and `ReplaceFileW`. Step 9 keeps the other program's bytes when that gap is hit.
- **Parent links on odd drives:** if a file system driver cannot report a final path, the name is used as given.
