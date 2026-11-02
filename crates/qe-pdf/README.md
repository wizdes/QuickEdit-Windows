# qe-pdf

Writes a PDF with no outside program, no printer and no UI. Print and Export make the same pages, as on the Mac.

```text
worker_snapshot -> qe-layout rows at the page's width -> pages, written to a temp file as they fill
                                                      -> fonts, footers and the page tree at the end
temp file -> check from disk -> cancel check -> rename over the target
```

## Calling it

```rust,ignore
let (width, height) = (frame.paper().width, frame.paper().height);
let options = PdfOptions {
    paper_width: width,
    paper_height: height,
    font_family: family,
    font_points: points_at_100_percent,
    tab_width,
    header_and_footer: number_pages_when_printing,
    rewrap: frame.page_wrap() || wrap_to_fit_when_printing,
    window_width_dips: editor.text_area().width.0,
    title: display_name,
    date: header_date_now(),
    ..PdfOptions::default()
};
let job = export_worker(editor.worker_snapshot(), options, path, Some(wake))?;
```

On each wake from the job, a host does two things:

1. If `job.wants_snapshot()` says true, it calls `job.offer_snapshot(editor.worker_snapshot())`. The label can say the file is still being read.
2. If `job.is_finished()` says true, it calls `job.join()`.

Read `job.fraction()` for a progress bar, and call `job.cancel()` to stop. The Mac shows its progress sheet only when a write still runs after 1 second, and it reads the fraction 10 times a second while the sheet shows.

## How the file is made

- **Line breaks:** the same engine settings as the editor's Wrap to Page. Lines past the long-line limit go through `layout_segment`, as on screen.
- **Pages:** a page takes each whole row that fits. A row taller than the page goes alone, and the text area's clip cuts it.
- **Fonts:** every font is an embedded subset, as CID-keyed CFF. Outlines come from DirectWrite's `GetGlyphRunOutline`, so TrueType, CFF, variable instances and simulated bold take one path. Direct2D's `Outline` removes overlaps, because Windows' own PDF reader showed holes at them. Hinting is not kept.
- **Errors:** a font whose license forbids embedding, or a glyph stored only as a picture, stops the export with a plain error.
- **Text:** each CID is one (glyph, text) pair with a ToUnicode entry. `LayoutGlyphRun` has no cluster map, so `text.rs` finds clusters from the caret stops. All of a run's text always reaches some glyph.
- **Color emoji:** COLR layers are drawn as colored shapes, one form per glyph, over invisible text for selecting and searching. This matches `qe-render`. A layer's own transparency is ignored.
- **Header and footer:** Segoe UI 9 pt. The name and date go in the top margin, and "Page N of M" in the bottom margin. Footers are written at the end, when M is known, so no second layout pass is needed.
- **Failure:** a failed or cancelled export leaves no file, and any old file as it was. The temp file's name owes nothing to the target's, so any valid target name works.

## Only a whole document becomes a PDF

A mapped file's snapshot holds what its index knew when it was taken. Every line in it can look whole, so the only sign of a missing tail is its status.

- `export_to_file` refuses a snapshot that is not complete, with `PdfError::NotComplete`. It writes nothing.
- `export_worker` waits instead. It asks the host for newer snapshots until one is complete, and wakes the host about 10 times a second while it asks. The wait takes the first 10% of the progress bar.
- A store failure, before or during the export, gives `PdfError::Read` and no PDF. Dropping the job or calling `join` during the wait gives `NotComplete`.

## What reaches the PDF's text

The rule is that copying from the PDF gives the document's text, in the store's order within each run.

- **Tabs:** layout makes no glyph for a tab. Each tab goes in the file as a CID with an empty shape, the width of the tab and a map entry that says tab, so a reader's selection covers it. `pdftotext` turns it into a space or keeps it, as that reader does for any PDF.
- **Characters that draw nothing:** soft hyphen, zero-width space, the byte-order mark, direction marks and control characters reach the text the same way when layout gives them no glyph. When a glyph holds them, they stay with that glyph's entry.
- **Line endings:** these stay out. A new row is how a PDF says this.
- **A glyph with no text of its own:** the second part of a split letter, or a glyph past a cluster's characters, goes in the file as a shape and not as text. Readers disagree about a CID with no map entry, and MuPDF returns its number as a character. A selection therefore covers only the cluster's first glyph, and gives the whole cluster's text.
- **A long cluster:** Poppler reads a map entry into 256 bytes, so an entry holds at most 60 UTF-16 units. A longer cluster's text goes on in blank CIDs right after its glyph. `check_pdf` refuses a longer entry, and an entry that splits a character.
- **The header:** a title's control characters print as spaces. The whole name is in the file's `/Title`.
- **The Mac:** its source (`PDFRenderer.swift`) draws through `NSLayoutManager.drawGlyphs` and has no code for tabs, so what its PDF does with them cannot be read from the source. This was not run on a Mac.

## Limits

The real limits are the disk, 9,999,999,999 bytes for the 10-digit table of objects, and the 8,388,607 objects that PDF 1.7 allows readers to stop at. That is about 4 million pages with no header, and about 2.7 million with one. Before anything is written, the export works out the fewest pages the line count can take, and stops with `PdfError::TooLong` if those cannot fit. During the write, it stops at the page where the file passes either limit. A page tree with millions of pages in one flat list has not been tried in a reader. Memory grows with pages (about 40 bytes each, plus the object table) and with the glyphs used, and not with the text's bytes.

`qe_pdf::check::check_pdf` parses a whole file strictly, and the tests use it. Damaged input gives an error and never a panic. It checks each stream's header, stored lengths and checksum, and refuses a drawn glyph with no text, so a broken compressor or a made-up character fails every PDF test with no outside program.

Tests that need `pdftotext` are marked ignored, so cargo never counts them as passed on a PC without it. Run them with `cargo test -p qe-pdf --locked -- --ignored`. With no `pdftotext` on PATH they fail.
