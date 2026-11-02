# qe-views

`ViewFrame` owns the side views and page and zoom state. Create it with the
window's region, editor font family, and base point size. The window calls
`layout(editor, bounds, measurer, ctx)` at resize; `sync(editor, measurer, ctx)`
after edits, scrolls, layout delivery, or changed view settings; and
`draw(editor, list, theme, measurer)` for each frame. Forward pointer movement
to `pointer_near`, the requested fade timer to `tick`, and map presses and
drags to `map_down`, `map_drag`, and `map_up`. Send `ViewContext` requests to
the shell. `set_page_wrap` and `set_zoom` change the editor; call `sync` after
zoom so the gutter width follows the font.

```text
window -> ViewFrame -> EditorView margins, page wrap, font, scroll
                    -> worker_snapshot -> map runs -> DisplayList
         ViewContext timer -> Fade -> DisplayList
```

The map worker reads bounded text from a snapshot and never reads a mapped
file on the UI thread. Its pending request uses a 16 ms timer until the worker
returns; it has no timer while idle. The editor's own layout may still be
pending; the gutter then draws only rows that `visible_rows` knows.

The current paper is one continuous sheet. Page gaps need editor layout
support. The map worker does not yet implement the Mac's exact top-line
rebalancing as the document scrolls. Saved bitmap comparisons at 96 and
144 DPI still need a host or a render test dependency.
