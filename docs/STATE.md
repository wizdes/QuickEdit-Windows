# QuickEdit for Windows state

Rewritten at each milestone, never appended. Last rewrite: 2026-11-04.

Milestone: M1 to M14 are built and joined into `QuickEdit.exe`. M15 has its package files, parity sweep and fixes from the final reviews. Store submission has not started.

Next action: the owner's final look before Store submission. Before upload, a person reserves the name in Partner Center, fills the placeholders in `packaging/AppxManifest.xml` and `packaging/PRIVACY.md`, publishes the privacy policy, and makes the checks in `packaging/README.md`.

Reviews: Astra reviewed the final code on 2026-11-03 and found 7 bugs. Four fix rounds followed, each reviewed by Astra, and the first three also by Opus. The last review found 1 rare gap, which is in `POST_MVP.md`. Full suite on the merged code: 2,995 passed, 0 failed, 32 ignored.

Open checks that need a person or a real service: a real input method, Narrator, Accessibility Insights, 150% and 200% scale, a second monitor, high contrast, selecting several files in File Explorer, the App Certification Kit and the Store upload.

Branch: work happens on `dev` in `worktrees/dev`. It merges to `main` at the end of each wave.
Approvals: see "Your decisions" and "Choices to check" in `docs/plan.md`.
