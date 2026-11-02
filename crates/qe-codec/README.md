# qe-codec

This crate turns file bytes into text and text into bytes, and it saves files safely.
It follows `QuickEdit/Document/PlainTextIO.swift` on the Mac.

## Opening

- A chosen encoding wins. Bytes that do not fit it give `InapplicableEncoding`.
- Automatic tries these steps in order: BOM, strict UTF-8, the Windows system code page, lossy UTF-8.
- The last step marks the file `lossy`, so every file can open.
- A file whose bytes would change on save is marked `lossy`.
- To open a file from disk, use `decode_file`. It never holds the file's bytes, so its peak is the text plus a few MiB.
- `decode` takes bytes already in memory, so it holds the input and the text at once. Shift JIS text made only of kanji is 1.5 times the file size in UTF-8, so `decode` peaks at about 2.5 times the file size for that text.
- `transcode_to_utf8_file` writes the text to a file and uses only a few MiB of memory.

## Saving

`atomic_write` does these steps:

1. It follows a link to the real file. The link stays as it is.
2. It stops if the target is read-only, or if an access rule denies writing.
3. It creates a temp file beside the target. The temp file has the target's access rules from its first moment, before any byte is written.
4. It writes the bytes and flushes them to disk.
5. A new name uses a rename that cannot replace. If a file appeared there, the result is `TargetExists`.
6. An existing target uses `ReplaceFileW`, which keeps the creation date, access rules and alternate streams. It tries about 10 times over 500 ms while another program holds the file.
7. A failed replace is an error with the Windows code. The code checks the disk before it says that the original is intact.

Temp and backup names are created with create-new and a random part, so a save never overwrites an older file of ours.

`prepare_large_copy` and `PreparedCopy::commit` do the same for a file too large to hold in memory, such as Save As on a file over 512 MiB. `copy_large_original` is the form for a new name.

## Recovery after a crash

Temp and backup files have this name: `.qe-<name hash>-<process id>-<random>.tmp` or `.bak`.

`find_leftovers` takes a folder or a document path and lists these files. It deletes nothing. M14 should call it for each document folder at launch. The app should offer recovery when `offer_recovery` is true.

| File | When `offer_recovery` is true | What the file holds |
| --- | --- | --- |
| `.bak` | It has bytes, and its process has ended | The old text. If the document is gone, this is the only copy. |
| `.tmp` | It has bytes, the document is gone, and its process has ended | New text, which may be partial |

A `.tmp` file beside an intact document is not offered, because a crash before the swap leaves the document as it was. An empty `.bak` file is a name reservation and holds nothing.

A process number can be reused. Until the new process ends, its leftovers show `owner_running` and are not offered.

## Known limits

- **Hard links:** a save splits them. The saved name gets a new file, and other names of that file keep the old bytes. This matches the replace method.
- **EUC-JP:** code page 20932 does not write standard JIS X 0212. `can_encode` and `encode` refuse any character that the page cannot write in standard form, so nothing changes silently.
- **FAT and exFAT:** these file systems have no access rules, so there is nothing to copy to the temp file.
- **Parent links on odd drives:** if a file system driver cannot report a final path, the name is used as given.
