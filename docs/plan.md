# QuickEdit for Windows: research results and 15-milestone plan

## Summary
I will build a new Windows 11 app called QuickEdit in Rust, on the raw Windows API, with its own text control. It looks like the Mac app, except for a top row that copies Windows Terminal. It ships through the Microsoft Store. The work is Tier 3, because it is a new project with a store launch, so every milestone gets the full tests and two fresh reviews.

## Context
QuickEdit is Swift and AppKit, so none of its code runs on Windows. You want a new app with the same UI, Windows window buttons, and the best speed I can reach. Anything not decided below follows what the Mac app does.

## What the research found

### Stack: Rust + raw Win32 + custom text control
| Choice | Why | Source type |
| --- | --- | --- |
| No WinUI, WPF or Electron | One 2025 test of empty apps: native C++ about 0 s, WinUI 3 with AOT (compiled ahead of time) 0.09 to 0.18 s, .NET 8 about 0.5 s from process start to window. It was a small, rough test in a VM. | Measured by a blogger |
| Rust with the `windows` crate | Compiled to one small exe with no runtime. Rust is already on both PC accounts. Strict types catch agent errors early. | My judgment |
| Custom text control, not RichEdit | Microsoft's own table shows the plain OS text calls are faster than RichEdit for simple text. Windows 11 Notepad uses RichEdit. | Microsoft blog |
| Custom text control, not Scintilla | Scintilla keeps the whole file in memory, which limits large files. Wrap to Page and viewing files over 512 MiB would be custom work anyway. | Notepad2 FAQ, reviewer |
| Piece tree (edits stored as a tree of pieces) | VS Code moved to this design for low memory and stable edit time. Editable files load into private memory on a background thread. Only read-only files over 512 MiB are memory-mapped. | VS Code blog, reviewer |
| Text shaping by DirectWrite | Windows handles every script and right-to-left text, so I never write that by hand. A fast path covers plain ASCII in fixed-width fonts. | Windows docs |
| Renderer: decided by measurement | M1 races 4 options (see M1). Windows Terminal and Zed both moved to Direct3D with a glyph atlas (a texture of drawn letters), but loading the GPU driver may slow the first frame. | Terminal and Zed repos |
| Painted UI for tabs, ⌄ list, find bar, Settings | Windows menus and common controls have no documented dark mode; the known fixes use hidden `uxtheme` functions. | Microsoft docs, Notepad++ source |
| System Open and Save dialogs | Kept native, with an encoding pop-up added through `IFileDialogCustomize`. | Reviewer |
| Spell check through `ISpellChecker` | Built into Windows and needs no package identity. Loaded only when turned on. | Microsoft docs |

### The five biggest risks, and when each is tested
1. **Mapped files:** another app can shrink or lock the file. M2 tests truncate, replace and save-over.
2. **Very long lines and full layout:** a 100 MB single line breaks per-line shaping. M4 tests this with benchmark files.
3. **Japanese and other input methods (TSF), and screen readers (UIA):** both need UTF-16 positions from the text store. M1 builds a small trial of both.
4. **GPU startup cost:** M1 measures it on this PC.
5. **Size of the painted UI:** every control needs keyboard and screen reader support. M1 fixes the widget design, and M7 builds it.

### How QuickEdit works (the copy target)
The full list came from reading the Swift code, and M1 commits it as `docs/parity-windows.md`. It starts from the 70+ behaviors in `Projects/QuickEdit/docs/parity.md`. Exact values come from the code:
- New window of 90 x 30 characters, line numbers off, document map on (100 px wide, box fades in over 0.15 s and out over 0.25 s).
- Zoom 25% to 1600%. Wrap to Page with 72 pt margins.
- Over 50 MB, spelling, gutter and map turn off. Over 512 MiB, the file is read-only.
- Line endings are kept exactly as found.

**Screenshot gap:** only 2 Mac screenshots exist on this PC (light and dark window). The product page also has a tabs picture. I cannot reach a Mac from here, so Settings, Find and Select Line get built from the sizes in the code. If you can run the Mac app, screenshots of those 3 would help, but I will not wait for them.

**Research gap:** Reddit, YouTube and X data have not been pulled yet, because plan mode allows no file writes. M1 runs X and YouTube. Reddit needs `agy` signed in on this PC; without that it is recorded as not done.

## Your decisions
1. Top row copies Windows Terminal: one tab always shows, then `+` and `⌄`, with minimize, maximize and close on the right. Snap Layouts work.
2. The user changed this on 2026-11-02: File, Edit, Format, View and Window sit in a menu bar under the tabs, like Notepad. Alt or F10 focuses the bar, and Alt+letter opens a menu. `⌄` holds About, Settings and Quit.
3. Cmd becomes Ctrl, plus Ctrl+H, F3 and Alt+F4. All can be changed in Settings.
4. Release through the Microsoft Store.
5. Tests stay hidden where possible. I tell you before any visible UI test run.
6. Spell check stays. Speech, Share and real printing go, so Print saves a PDF as on the Mac.
7. Files from Explorer and second launches open as a new tab in the last used window.
8. New public repo: `Projects/QuickEdit-Windows`, remote `wizdes/QuickEdit-Windows`.
9. I may add a `yjli` SSH key to the `agent` account on this PC, so that Sol builders can run there.

## Choices to check (my defaults; I continue unless you object)
- **Tab bar:** always shows, as you asked. This differs from the Mac, which hides it at one tab.
- **Encoding when opening:** BOM, then strict UTF-8, then the Windows system code page. Windows has no file tag like `com.apple.TextEncoding`, so that step goes. Mac encodings stay available in the list.
- **Revert:** Revert to Saved stays. Browse All Versions goes, because Windows has no match.
- **Complete:** Alt+Esc is taken by Windows, so Complete moves to Ctrl+Space.
- **Full screen:** F11, replacing Ctrl+Cmd+F.
- **Minimum window width:** about 500 px at most, as Snap needs.
- **Default font:** Cascadia Mono 10 pt, about the same size on screen as the Mac's 13 pt (13 px at 100% scale).
- **Colors:** follow the Windows light or dark setting, with the flat look of the Mac window and no Mica.
- **Supported systems:** Windows 11 x64 only, because this PC is the only test machine. It runs build 26300, which looks like a preview build, so speed may differ on retail Windows.
- **Heads-up before a visible test:** a chat note and a push notification, then I start after 2 minutes unless you say hold.
- **Reports:** one private page per milestone, each linking to the others, as you asked. This overrides the "major milestones only" rule in `PAGES.md` for this project.
- **Running app:** Milestones M2 to M8 are parts, so their pages show pictures drawn by the parts. You can first type in a real window at M9. This departs from the "every milestone ends with a working app" rule, because it allows more parallel work.

## Not in this version
- Speech, Share, real printing, Services, Hide Others, Writing Tools, Data Detectors, grammar and auto-update (the Store handles updates).
- Windows 10, ARM64 and a direct .exe download.
- Syntax colors, multiple cursors, plugins and anything else the Mac app lacks.
- Signing outside the Store, and any new spending.

## You need to do (everything else I do myself)
- [ ] Before M1: reserve the app name in Microsoft Partner Center. "QuickEdit" may be taken; I will stop and ask if it is.
- [ ] M1: sign in to `agy` for Reddit data (optional).
- [ ] M1: pick the top row and Settings designs on the mockup page.
- [ ] M1: try the Japanese input and Narrator trials, or tell me to skip them. Only en-US is installed.
- [ ] M15: approve "ship it", then submit to the Store under your account.

## Milestones
A wave runs side by side. Each builder runs in its own clone and owns its own Rust crate (a separate code folder), so builders do not collide. Only root edits `Cargo.toml` and joins the parts.

```
Wave 0                M1 (interfaces, measurements, designs)
                       |
Wave 1 (6 at once)    M2  M3  M4  M5  M6  M7
                       |       |           |
Wave 2 (2 at once)     +-- M9--+           M8
                           |               |
Wave 3 (5 at once)    M10 M11 M12 M13 M14 (M14 also needs M3, M5, M6, M8)
                       |   |   |   |   |
Wave 4                 +---+---M15-+---+
```

| ID | What it builds | Needs | Owns | Done when | Budget |
| --- | --- | --- | --- | --- | --- |
| M1 | Repo, frozen interfaces, renderer race, trials, mockups, parity list, X and YouTube pulls | none | `qe-api`, `spikes/`, `tools/perf` | The race table has real numbers and a winner, `agent` probe results are recorded, you picked the designs, and the interfaces compile | 8 h |
| M2 | Piece tree, undo, line index, UTF-16 positions, read-only mapped files | M1 | `qe-buffer` | Random-edit tests match a plain string; truncate and replace tests pass; a 1 GiB file opens in the test | 4 h |
| M3 | Encodings, BOM, line endings, lossy-file checks, safe atomic save | M1 | `qe-codec` | Byte-for-byte tests pass on fixtures for every encoding and mixed line endings | 3 h |
| M4 | Shaping, line cache, wrap, hit testing, long-line handling | M1 | `qe-layout` | A 100 MB single line stays inside the budget; Arabic and emoji pictures match saved images | 4 h |
| M5 | Production renderer (the M1 winner) | M1 | `qe-render` | Rendered images match saved images in both themes at 100%, 150% and 200% scale | 4 h |
| M6 | Window, Terminal-style title row, Snap, tab strip with `+` and `⌄` | M1 | `qe-shell` | Snap flyout, DPI change and tab drag pass in a visible run; pictures sit beside Terminal's | 4 h |
| M7 | Painted widgets, `⌄` popup list, dialogs, keyboard use | M1 | `qe-ui` | Every widget works by keyboard alone; pictures in both themes pass | 5 h |
| M8 | Command table, Settings (3 tabs), Keyboard recorder, settings file | M1, M7 | `qe-settings` | All Mac settings present with Mac defaults; a clash is refused with a message; changes persist | 4 h |
| M9 | Caret, selection, typing, clipboard, scrolling. **First real editor window** | M2, M4 | `qe-editor` | You can type, select, undo and paste in a real window; the first typing latency numbers are in | 5 h |
| M10 | Input methods (TSF) and spell check | M9 | `qe-ime`, `qe-spell` | Text store tests pass; red underlines and the fix menu work; nothing loads while spelling is off | 4 h |
| M11 | Screen reader support (UI Automation) | M7, M9 | `qe-a11y` | The UIA tree exposes text and selection; Accessibility Insights finds no blocking failures, if it can be installed | 3 h |
| M12 | Find bar, Replace, Select Line, Transformations, Complete | M7, M9 | `qe-tools` | Mac rules pass, including Select Line's `N-M` and `+N` forms and Replace All | 3 h |
| M13 | Gutter, document map, Wrap to Page, zoom, PDF export | M4, M9 | `qe-views`, `qe-pdf` | Map box and fade timing match; PDF pages match Wrap to Page; both turn off over 50 MB | 5 h |
| M14 | Documents, tabs, windows, autosave and restore, reload on outside change, blank-window close, file handoff | M3, M5, M6, M8, M9 | `qe-docs`, `qe-app` | Kill and relaunch keeps unsaved text; 15 files opened from Explorer land in tabs of one window | 6 h |
| M15 | Final joining, MSIX package, full parity sweep, speed check, Store package | all | `packaging/`, `qe-app` | Every parity row is marked; the speed table is filled; the package installs and updates while running; ready for "ship it" | 6 h |

Budgets are my estimates. If a milestone runs 50% over, I send you a note and cut back to its done-test.

### M1 details (everything else depends on it)
- **Interfaces frozen first:** `TextStore`, `DisplayList`, `Layout`, `InputEvent`, the widget tree, command IDs, settings keys and the replay test format, all in `qe-api`.
- **Renderer race, 4 options on this PC:**
  1. DirectWrite drawn to a bitmap, then copied to the window, with no GPU.
  2. Direct2D on a flip-model swap chain (a modern, low-delay way to show frames).
  3. Direct3D 11 with a glyph atlas and a waitable swap chain.
  4. A first frame without the GPU, then Direct3D started on a background thread.
  The winner has the lowest cold first frame among options that meet the typing budget.
- **Builder probe:** I run a small program as `agent` over SSH to see which of these work there: software Direct3D (WARP), DirectWrite, `ISpellChecker` and hidden windows. If any fail, the test plan changes.
- **Packaging check:** the same exe, launched four ways (direct, from the Start list, by opening a file, and by a second launch), to measure what MSIX adds.
- **Design:** at least 3 mockups each of the top row and `⌄` list and of Settings, critiqued by Astra and a fresh reviewer, on one page with my recommendation. You pick before M6 and M7 finish.

## How I run it
- **Builders:** Sol on `agent`, one per milestone, up one level on failure. `agent` cannot show windows and the runner refuses Opus there, so a failed Sol build goes to a native Opus builder.
- **Builder tests (no window needed):** replay input scripts, draw to a bitmap, and compare with saved images.
- **Root tests:** real windows, all timings, Snap, DPI, Explorer opening and MSIX install. I run routine checks on a hidden desktop, if that works. I never take timings there.
- **Reviews:** Astra and a fresh Opus reviewer at the end of each milestone, with a 900 s limit. I check every finding myself.
- **Milestone report:** a private page with what was done, real screenshots I have looked at, numbers and next steps. I do not wait for a reply.
- **Commits:** each accepted milestone is committed and pushed. Tier 3 work happens in a worktree that is merged at the end of each wave. A diagram of what changed sits next to the code.
- **Status:** a full checklist in chat when a wave starts or ends, and a push notification when the work ends or truly blocks.

## Speed budgets (goals, not measurements)
| What | Goal | How measured |
| --- | --- | --- |
| Warm launch to first frame, without MSIX | under 60 ms | timer in the launcher, 30 interleaved runs |
| Cold launch | under 150 ms | same, after clearing the file cache |
| Extra cost of MSIX | report it; fail above 30 ms | the four launch paths |
| Key press handled, up to the present call | p99 under 2 ms | timer in the app, on 1 MB and 512 MiB files |
| Key press to screen | p50 under 25 ms, p95 under 33 ms | `SendInput` to a captured frame, also with PresentMon. This PC's display reports 60 Hz |
| Second launch to visible tab | under 50 ms | timer across both processes |
| 1 GiB read-only: first screen | under 100 ms; line index under 2 s | test file |
| 100 MB editable: first screen | under 150 ms | test file |
| Idle | no timers and no wakeups | ETW trace |
| Empty window memory | under 40 MB private | process counters |

I will also run Windows Notepad, Notepad++ and the renderer options through the same launch and typing tests, if I can install them, so the numbers have a comparison. I will report any missed budget as missed, with its number.

## Verification at the end
1. `cargo test` for all crates, plus the full replay and saved-image suite. A retry pass, a skip or a zero-test run counts as a failure.
2. A visible UI test run on the MSIX install, after the heads-up, following `docs/parity-windows.md` row by row.
3. Side-by-side pictures of the Mac window and the Windows window in light and dark.
4. The final speed table, run on a clean reboot if you allow one. A reboot has been pending on this PC since 2026-10-30, and I will ask first.
5. Final review through the `review` skill, then your "ship it".

## Critical files
- New: `Projects/QuickEdit-Windows/crates/qe-api/src/lib.rs`, `crates/qe-app/src/main.rs`, `packaging/AppxManifest.xml`, `docs/plan.md`, `docs/STATE.md`, `docs/parity-windows.md`.
- Copy target, read only: `Projects/QuickEdit/QuickEdit/**`, `Projects/QuickEdit/docs/parity.md`, `Projects/QuickEdit/docs/design-spec.md`.
- After approval, I also update memory (`quickedit.md`, a new `quickedit_windows.md`, and the `yjli` key in `remote_machines.md`).
