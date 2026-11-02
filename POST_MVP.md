
- qe-render test `a_very_long_run_stops_at_the_right_edge` failed once in a combined `cargo test -p qe-shell -p qe-api -p qe-render` run on 2026-11-02 with other builds running, and passed 5 of 5 runs alone. Its limit is 30 ms under load. Check whether the limit is too tight for a debug build, or whether the run hides a real slow path.

- Saved-picture tests failed once each with a difference of a few color values, with other builds running: qe-shell `top-row-light-96.png` (3 channels) and qe-ui `saved_images_match_in_all_three_themes`. Neither reproduced in 30 repeat runs. Cause unknown; look for shared state or load-dependent font handling in the renderer path.
