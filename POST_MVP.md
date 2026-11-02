
- qe-render test `a_very_long_run_stops_at_the_right_edge` failed once in a combined `cargo test -p qe-shell -p qe-api -p qe-render` run on 2026-11-02 with other builds running, and passed 5 of 5 runs alone. Its limit is 30 ms under load. Check whether the limit is too tight for a debug build, or whether the run hides a real slow path.

- Saved-picture tests failed once each with a difference of a few color values, with other builds running: qe-shell `top-row-light-96.png` (3 channels) and qe-ui `saved_images_match_in_all_three_themes`. Neither reproduced in 30 repeat runs. Cause unknown; look for shared state or load-dependent font handling in the renderer path.

- The trial programs in spikes/ fail clippy with warnings denied (collapsible if, manual C string, chunks_exact). They are throwaway; decide at M15 whether to fix or remove them.

- Five kinds of test give different answers in the `agent` account than in the signed-in account: two qe-codec link tests stop on error 448, three qe-render window tests get access-denied or invalid-handle errors, qe-shell's DPI setup gets 0x80070005, and a qe-shell image test finds the plus sign's left edge at 20 where it expects 19 (seen in the M9 review at 206fa52). Check each one and make it skip with its reason, or fix the cause. The plus sign one has no known cause yet.
