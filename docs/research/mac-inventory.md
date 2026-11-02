# What the Mac app does

Read from the Swift source of `wizdes/QuickEdit` on 2026-11-01, at version 1.2.3. Paths are inside that repo's `QuickEdit/` folder. When this file and the source disagree, the source wins. `docs/parity.md` in that repo has the full behavior list.

## Menus (`App/MainMenu.swift`). Keys are Mac defaults; all can be changed
- **QuickEdit:** About, Settings (Cmd+,), Services, Hide (Cmd+H), Hide Others (Cmd+Opt+H), Show All, Quit (Cmd+Q).
- **File:** New (Cmd+N), New Tab (Cmd+T), Open (Cmd+O), Open Recent with Clear Menu, Close (Cmd+W; reads "Close Tab" at 2+ tabs), Save (Cmd+S), Duplicate (Cmd+Shift+S), Save As (Cmd+Shift+Opt+S), Rename, Move To, Save All, Revert to Saved, Export as PDF, Print (Cmd+P; saves a PDF), Share.
- **Edit:** Undo (Cmd+Z), Redo (Cmd+Shift+Z), Cut, Copy, Paste, Delete, Complete (Opt+Esc), Select All; then submenus:
  - Find: Find (Cmd+F), Find and Replace (Cmd+Opt+F), Find Next (Cmd+G), Find Previous (Cmd+Shift+G), Use Selection for Find (Cmd+E), Jump to Selection (Cmd+J), Select Line (Cmd+L).
  - Spelling and Grammar: Show Spelling (Cmd+:), Check Document Now (Cmd+;), Check Spelling While Typing, Check Grammar With Spelling, Correct Spelling Automatically.
  - Substitutions: Show Substitutions, Smart Copy/Paste, Smart Quotes, Smart Dashes, Data Detectors, Text Replacement.
  - Transformations: Make Upper Case, Make Lower Case, Capitalize.
  - Speech: Start Speaking, Stop Speaking.
- **Format:** Prevent Editing, Wrap to Page (Cmd+Shift+W; flips to Wrap to Window).
- **View:** Actual Size (Cmd+.), Zoom In (Cmd+=), Zoom Out (Cmd+-), Show Line Numbers, Show Document Map, Enter Full Screen (Ctrl+Cmd+F).
- **Window:** Minimize (Cmd+M), Zoom, Show Previous Tab (Ctrl+Shift+Tab), Show Next Tab (Ctrl+Tab), Tab 1 to 9 (Cmd+1 to 9), Last Tab (Cmd+0), Bring All to Front, Move Tab to New Window, Merge All Windows.
- **Help:** empty.

## Settings (`Window/SettingsWindowController.swift`, `Support/Defaults.swift`)
Window 440 wide, not resizable, tabs New Document, Open and Save, Keyboard. Restore All Defaults button at bottom left. Settings apply to new windows, except line numbers, which apply at once.
- **New Document:**
  - Format: Wrap to page (`ShowPageBreaks`, off).
  - Window Size: Width 90 characters, Height 30 lines (1 to 10000; junk reverts).
  - Font: "Plain text font:" with a Change button and the name shown.
  - Options, left: Check spelling as you type (off), Check grammar (off), Correct spelling automatically (off), Show Writing Tools (off). Right: Smart copy/paste (on), Smart quotes (off), Smart dashes (off), Text replacement (system value).
  - View: Show line numbers (off).
- **Open and Save:** Add ".txt" extension to plain text files (on). Plain Text File Encoding: Opening files, Saving files; each pop-up has Automatic, the enabled encodings, and Customize Encodings List.
- **Keyboard:** 394 x 464. "Click a shortcut, then press the new keys." Two columns, Command and Shortcut (132 wide), rows 18 high, bold menu headers, indented submenu rows. Click, or select and press Return, Enter or Space, to record. Esc cancels. Delete resets. Changed rows say "(changed)" with a reset arrow. A refused key shows a warning under the table. Restore Keyboard Defaults at bottom right.
- **No control:** `TabWidth` 8, `AutosavingDelay` 30, `PaperSize` (Letter in US and CA, else A4).

## Visual values
- **Text:** fixed-pitch 13 pt. New window 90 x 30 characters. Gutter and map sit outside that text area. Tab stops every 8.
- **Gutter** (`Window/LineNumberRulerView.swift`): digits x digit width + 6 left + 8 right, at least 3 digits. Number size max(9, round(0.85 x text size)). Right-aligned on the first row of each line. Number color black or white at 62%. 1 px separator on the right. Redraw after typing is delayed 0.1 s.
- **Document map** (`Window/DocumentMapView.swift`): 100 wide, each line 2 tall, each column 1 wide, blocks 1.4 tall, text color at 45%, no separator. Box fill at 10% with a 1 px outline at 22%. The box shows when the pointer is within 8 of the map or during a drag. Fade in 0.15 s, out 0.25 s. Reduce Motion makes it instant. Click centers the box; drag scrolls.
- **Zoom:** 25% to 1600%. In doubles, out halves. Actual size resets.
- **Wrap to Page:** 72 pt margins. Letter 612 x 792, A4 595 x 842, Legal 612 x 1008. Pages on the under-page background.
- **Colors:** warning `#b3261e` light, (1.0, 0.54, 0.47) dark. Dim text at 62%. Group headers at 85%.
- **Select Line panel:** 253 x 62, field 138 x 22 with placeholder "Line number", Select button 77 x 32.
- **Customize Encodings panel:** 336 x 187, resizable, a checkbox per encoding, Select all, Clear all, Revert to default.
- **Save panel extra:** "Plain Text Encoding:" pop-up with no Automatic item, and "If no extension is provided, use .txt".

## File behavior (`Document/PlainTextIO.swift`, `Document/TextDocument.swift`)
- **Automatic reading order:** file tag, BOM (UTF-32, UTF-16, UTF-8), strict UTF-8, system default encoding. No guessing. A chosen encoding that does not fit shows "could not be opened using text encoding X".
- **Lossy files:** if bytes would not round-trip, the first edit asks, and overwrite asks Duplicate, Cancel or Overwrite.
- **Default encodings:** UTF-16, UTF-8, Mac Roman, CP1252, Mac Japanese, Shift JIS, Mac Chinese Traditional, Mac Korean, Mac Chinese Simplified, GB 18030.
- **Writing:** new files UTF-8 with no BOM. UTF-8 BOM is kept. UTF-16 and UTF-32 write a little-endian BOM. A file read with no BOM writes back big-endian with no BOM. Writes are atomic.
- **Line endings:** kept as found, mixed allowed. Return types the file's most common ending. Paste is untouched.
- **Autosave:** on, restore keeps selection and first visible character.
- **Blank untitled:** empty or whitespace-only text with no file closes at once and is not restored. Over 1 MB counts as not blank.
- **Transient window:** the launch window is replaced in place by an opened file until it is edited, moved or resized.
- **Outside change:** reloads when clean if bytes changed; a date-only change keeps text, caret and undo. With edits, nothing reloads and a warning comes on save.
- **Large files:** over 50 MB (50 x 1024 x 1024), spelling, substitutions, gutter and map turn off, and the title gains "Large file: spelling and substitutions off". Over 512 MiB, read-only, with title "Read only: file too large to edit (size)". Find, select, copy, Select Line, Save As and Export as PDF still work. Save As writes the original bytes.
- **Per window, not saved in the file:** Prevent Editing, Wrap to Page. A new document takes Wrap to Page from settings; a file from disk starts in Wrap to Window.

## Editing
- **Select Line:** `N`, `N-M`, `+N`, `-N`, `+N-M`, `-N-M`, spaces skipped. Line 0 is an error. Return selects and closes; the Select button keeps the panel open. Errors beep and show "Invalid line specification". Lines end at LF, CR, CRLF, U+0085, U+2028, U+2029.
- **Find:** a find bar with replace. Read AppKit's `NSTextFinder` behavior from the parity list.
- **PDF:** Print and Export both write a PDF. The panel has "Print header and footer" (off) and "Rewrap contents to fit page" (on, Wrap to Window only).
- **Tabs:** 2 or more show a bar on the Mac. Windows always shows it, by user decision.

## Mac-only, so left out or replaced on Windows
See "Your decisions" and "Choices to check" in `docs/plan.md`. Left out: Services, Share, Hide, Speech, Data Detectors, Writing Tools, grammar, Browse All Versions, the file tag. Replaced: APFS clone for large files (Windows has no plain match; M2 and M3 decide), Mac text-key bindings (Windows keys instead).
