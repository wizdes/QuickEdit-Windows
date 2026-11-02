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

Read `job.fraction()` for a progress bar, call `job.cancel()` to stop, and call `job.join()` once `job.is_finished()` says true. The Mac shows its progress sheet only when a write still runs after 1 second, and it reads the fraction 10 times a second while the sheet shows.

## How the file is made

- **Line breaks:** the same engine settings as the editor's Wrap to Page. Lines past the long-line limit go through `layout_segment`, as on screen.
- **Pages:** a page takes each whole row that fits. A row taller than the page goes alone, and the text area's clip cuts it.
- **Fonts:** every font is an embedded subset, as CID-keyed CFF. Outlines come from DirectWrite's `GetGlyphRunOutline`, so TrueType, CFF, variable instances and simulated bold take one path. Direct2D's `Outline` removes overlaps, because Windows' own PDF reader showed holes at them. Hinting is not kept.
- **Errors:** a font whose license forbids embedding, or a glyph stored only as a picture, stops the export with a plain error.
- **Text:** each CID is one (glyph, text) pair with a ToUnicode entry. `LayoutGlyphRun` has no cluster map, so `text.rs` finds clusters from the caret stops. All of a run's text always reaches some glyph.
- **Color emoji:** COLR layers are drawn as colored shapes, one form per glyph, over invisible text for selecting and searching. This matches `qe-render`. A layer's own transparency is ignored.
- **Header and footer:** Segoe UI 9 pt. The name and date go in the top margin, and "Page N of M" in the bottom margin. Footers are written at the end, when M is known, so no second layout pass is needed.
- **Failure:** a failed or cancelled export leaves no file, and any old file as it was.

## Limits

No limit on document size is written into the code. The real limits are the disk, 9,999,999,999 bytes for the 10-digit table of objects (which the code does not check), and the 8,388,607 objects that PDF 1.7 allows readers to stop at. That is about 4 million pages with no header, and about 2.7 million with one. Memory grows with pages (about 40 bytes each, plus the object table) and with the glyphs used, and not with the text's bytes.

`qe_pdf::check::check_pdf` parses a whole file strictly, and the tests use it.
