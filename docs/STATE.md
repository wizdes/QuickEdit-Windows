# QuickEdit for Windows state

Rewritten at each milestone, never appended. Last rewrite: 2026-11-02.

Milestone: M1 to M7, M9, the menu bar and M10 to M13 are built and tested as parts. M8, M14 and M15 are not started. Only the top row, the menu bar and the editor run together in a real window, through `crates/qe-editor/examples/editor.rs`.

Next action: M8 waits on the user's Settings and editor picks. Then M14 joins every part into the real app, using the lists in `crates/qe-tools/WIRING.md`, `crates/qe-a11y/README.md`, `crates/qe-views/README.md`, `crates/qe-pdf/README.md` and `crates/qe-editor/HOOKS.md`.

Reviews: Astra's usage limit ends 2026-11-04 21:32. M10 to M13 had a second review from Opus only. Their latest fixes have tests and no fresh review, so Astra reviews the final code when it returns.

Open checks that need a real window, a person or a real service: a real input method, Narrator, Accessibility Insights, the real clipboard, real file dialogs, 150% and 200% scale, high contrast, Store packaging and the speed table on a quiet PC.

Branch: work happens on `dev` in `worktrees/dev`. It merges to `main` at the end of each wave.
Approvals: see "Your decisions" and "Choices to check" in `docs/plan.md`.
