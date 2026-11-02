# qe-pdf

Pass `EditorView::worker_snapshot()` to `export_worker` with `PdfOptions`,
an output path, a cancel flag, and a progress callback. The returned join
handle reports the page count or a plain error. The worker writes a temporary
file beside the target and renames it only after a complete PDF passes
`check_pdf`. The callback runs on the worker, so the host must send its
updates to the UI thread.

`build_pdf` uses `qe-layout` for row breaks at the same 468 DIP text width
that `PageWrap` uses on Letter paper. It writes a ToUnicode map for printable
ASCII. The current limit is 8 MiB and 100,000 lines. Tabs, non-ASCII text,
font embedding, color emoji, the chosen font, and the Mac header and footer
are not implemented. Such characters return an error before a file is
written. This is not yet a complete PDF exporter for QuickEdit.
