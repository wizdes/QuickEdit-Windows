
- qe-render test `a_very_long_run_stops_at_the_right_edge` failed once in a combined `cargo test -p qe-shell -p qe-api -p qe-render` run on 2026-11-02 with other builds running, and passed 5 of 5 runs alone. Its limit is 30 ms under load. Check whether the limit is too tight for a debug build, or whether the run hides a real slow path.

- Saved-picture tests failed once each with a difference of a few color values, with other builds running: qe-shell `top-row-light-96.png` (3 channels) and qe-ui `saved_images_match_in_all_three_themes`. Neither reproduced in 30 repeat runs. Cause unknown; look for shared state or load-dependent font handling in the renderer path.

- The trial programs in spikes/ fail clippy with warnings denied (collapsible if, manual C string, chunks_exact). They are throwaway; decide at M15 whether to fix or remove them.

- Five kinds of test give different answers in the `agent` account than in the signed-in account: two qe-codec link tests stop on error 448, three qe-render window tests get access-denied or invalid-handle errors, qe-shell's DPI setup gets 0x80070005, and a qe-shell image test finds the plus sign's left edge at 20 where it expects 19 (seen in the M9 review at 206fa52). Check each one and make it skip with its reason, or fix the cause. The plus sign one has no known cause yet.

- qe-buffer: add `UndoHistory::take_release_list`, so waiting history is freed on a worker. Today Blur and idle free it on the UI thread: 97 MiB took 11.6 ms.
- qe-buffer: add `PieceStore::try_new`, using `try_reserve` for each leaf. Today the load checks for room first, but another program can take that memory, and the app then aborts and other tabs lose unsaved text.
- Paste decodes and inserts on the UI thread: 247 ms to decode 100 MiB and 1,252 ms for 512 MiB, plus `store.splice_text`. It needs a way to build the tree on a worker.
- Word keys stop at a gap over 256 KiB, so a very long gap can take several key presses. Check this against the Mac.
- `qe-editor` test `p12_a_dropped_page_key_says_so` failed in 3 mutation runs that had nothing to do with it. The likely cause, not proven, is that background reads brought the page into memory first. Its setup now looks for a place where the key really waits, and it has passed 5 of 5 full runs since. Watch for it.
- Not compared on a Mac: undo selecting the restored text, the fourth click, Backspace on a cluster, double-click and word keys on emoji.
- The `open-recent` test command takes a plain path, so no app-level test goes through the hex decoding in `open_recent`. Add a route that uses the real menu row data; today only the `recent_menu_path` test covers it.
- A different-size outside write right after a save leaves the document clean (`save_succeeded`, `not_ours`), so it can reload and drop the just-saved text from the view. Only a same-size write forces it dirty. Check whether this is intended.
