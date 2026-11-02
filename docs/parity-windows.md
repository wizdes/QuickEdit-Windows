# QuickEdit for Windows: parity list

Each Mac parity row has one row below. Inventory rows add details from `docs/research/mac-inventory.md`. Windows and speed rows record `docs/plan.md` choices. `same` means the planned behavior matches the Mac app. `changed (why)` and `left out (decision)` record differences. `QUESTION` gives a suggested default where the plan has no answer. All rows start unbuilt.

## Mac parity rows

| Mac ID | Behavior | What Windows does | Builds | Status | Built |
| --- | --- | --- | --- | --- | --- |
| P1 | New opens a plain-text window. | New opens a plain-text window with Ctrl+N. | M14 | changed (Ctrl replaces Cmd) | no |
| P2 | Open accepts plain text and other text files. | Open the same text files. | M14 | same | no |
| P3 | Typing and Save write UTF-8 bytes. | Save new files as UTF-8 without BOM; use Ctrl+S. | M3, M9, M14 | changed (Ctrl replaces Cmd) | no |
| P4 | Untitled Save offers a .txt checkbox. | Offer the .txt checkbox, on by default. | M7, M14 | same | no |
| P5 | Dirty untitled close asks; quit keeps autosaved edits. | Keep both rules. | M14 | same | no |
| P6 | New windows fit 90 characters by 30 lines. | Use 90 by 30. | M6, M14 | same | no |
| P7 | Plain font is fixed-pitch; tab stops are 8 spaces. | Use Cascadia Mono 10 pt and 8-space tabs. | M4, M8 | changed (Windows default font) | no |
| P8 | Undo, redo, clipboard, delete, and select all work. | Keep the edits with Ctrl-based keys. | M9 | changed (Ctrl replaces Cmd) | no |
| P10 | Autosave keeps edits through quit. | Autosave and restore edits. | M14 | same | no |
| P11 | Duplicate, Rename, Move To, Revert, and Browse Versions. | Keep the first four; omit Browse All Versions. | M14 | changed (no Windows versions browser) | no |
| P12 | Save All saves every document. | Save every document. | M14 | same | no |
| P13 | Open Recent has Clear Menu. | Keep Open Recent and Clear Menu. | M14 | same | no |
| P14 | A locked file offers Unlock or Duplicate. | Offer Unlock (clears the read-only flag) or Duplicate; keep the file intact until the user picks. | M14 | same | no |
| P15 | Clean outside changes reload; dirty changes warn. | Reload changed bytes when clean; warn on save when dirty. | M14 | same | no |
| P16 | Windows, selection, and scroll restore. | Restore windows, selection, and first visible character. | M14 | same | no |
| P17 | Open replaces an untouched launch window. | Replace the untouched launch window. | M14 | same | no |
| P18 | Export as PDF saves the document. | Export as PDF. | M13 | same | no |
| P20 | Open offers Automatic and chosen encodings. | Offer Automatic and chosen encodings. | M3, M7 | same | no |
| P21 | Automatic reads Mac tag, BOM, UTF-8, system encoding. | Read BOM, strict UTF-8, then Windows code page. | M3 | changed (Windows has no Mac file tag) | no |
| P22 | Bad chosen encoding shows an open error. | Show an error naming the encoding. | M3, M7 | same | no |
| P23 | Save disables encodings that cannot hold text. | Disable those encodings. | M3, M7 | same | no |
| P24 | Save writes a Mac encoding file tag. | Omit the tag. | M3 | left out (Windows has no Mac file tag) | no |
| P25 | An edit beyond file encoding offers Save As or undo. | Warn and offer Save As or undo. | M3, M9 | same | no |
| P26 | Lossy reads warn on first edit and overwrite. | Warn; offer Duplicate, Cancel, or Overwrite. | M3, M14 | same | no |
| P27 | Keep mixed endings; Return uses the common ending. | Keep both rules. | M3, M9 | same | no |
| P28 | Keep UTF-8 BOM on save. | Keep the BOM. | M3 | same | no |
| P29 | Customize Encodings List changes offered encodings. | Keep the list and panel. | M3, M7, M8 | same | no |
| P30 | Find, Replace, Next, Previous, selection, and jump. | Keep actions with Ctrl-based keys and F3. | M12 | changed (Windows keys) | no |
| P31 | Select Line accepts numbers and ranges. | Keep `N-M`, `+N`, and other Mac forms. | M12 | same | no |
| P32 | Spelling and Grammar menu checks and corrects text. | Keep spelling; omit grammar. | M10 | changed (grammar omitted by decision) | no |
| P33 | Substitutions include Smart Copy, quotes, dashes, replacement, detectors. | Keep all except Data Detectors. | M9, M12 | changed (Data Detectors omitted by decision) | no |
| P34 | Transform selection to upper, lower, or capitalized. | Keep all three. | M12 | same | no |
| P35 | Speech reads selected text. | Omit Speech. | M15 | left out (user decision) | no |
| P36 | Complete uses Option+Esc. | Use Ctrl+Space. | M12 | changed (Windows reserves Alt+Esc) | no |
| P37 | Prevent Editing toggles read-only editing. | Keep the toggle. | M9 | same | no |
| P40 | Wrap to Page or Window toggles its menu title. | Keep both modes and the title change. | M13 | same | no |
| P41 | Page wrap shows paper pages and margins. | Show pages with 72 pt margins. | M13 | same | no |
| P42 | Zoom covers 25% to 1600%. | Keep the range, steps, and Actual Size. | M13 | same | no |
| P43 | Full screen, tabs, and dark mode work. | Use F11, always-visible tabs, and Windows colors. | M6, M14 | changed (Windows window and tab rules) | no |
| P44 | Text width stays fixed when scroller style changes. | Hold text width when the scrollbar changes. | M6, M13 | same | no |
| P50 | Settings set document size, font, wrap, and text options. | Keep these on New Document tab. | M8 | same | no |
| P51 | Settings set encodings and .txt extension. | Keep these on Open and Save tab. | M8 | same | no |
| P52 | Settings persist and apply to new windows. | Persist; line numbers also update at once. | M8, M14 | same | no |
| P60 | Print offers header and rewrap choices. | Ctrl+P saves PDF with both choices. | M13 | changed (real printing omitted by decision) | no |
| P61 | Page Setup changes print paper. | No Page Setup. Paper size comes from the region, as on the Mac. | M8, M13 | left out (Mac decision U1) | no |
| P62 | Export as PDF matches Print output. | Export and Ctrl+P make the same PDF pages. | M13 | same | no |
| P70 | AppleScript opens, edits, saves, and closes. | No scripting. File handoff opens files. | M14 | left out (Mac decision U3) | no |
| P71 | Services uses selected text. | Omit Services. | M15 | left out (user decision) | no |
| P72 | Dropping a file on app or window opens it. | Open Explorer files in a tab of the last used window. | M14 | changed (Windows file handoff rule) | no |
| P73 | Share, Writing Tools, and VoiceOver work. | Omit Share and Writing Tools; expose text to UI Automation. | M11, M15 | changed (user omissions and Windows screen reader API) | no |

## Mac inventory details

These rows cover details that the parity rows do not spell out. Each menu row covers one menu or submenu.

| Mac ID | Behavior | What Windows does | Builds | Status | Built |
| --- | --- | --- | --- | --- | --- |
| I01 | QuickEdit menu has About, Settings, Hide, Show All, Quit. | Put About, Settings, Quit in caret list; omit Mac hide actions. | M7, M8 | changed (Windows menu and no Mac hide actions) | no |
| I02 | File menu has document, save, PDF, Print, and Share actions. | Put retained actions in caret list; omit Share. | M7, M13, M14 | changed (caret menu; Share omitted) | no |
| I03 | Edit menu has undo, clipboard, delete, Complete, Select All. | Put these actions in caret list with Windows keys. | M7, M9, M12 | changed (caret menu and Windows keys) | no |
| I04 | Find submenu has find, replace, navigation, selection, Select Line. | Put these actions in caret list. | M7, M12 | changed (caret menu) | no |
| I05 | Spelling menu has show, check now, typing, grammar, autocorrect. | Keep spelling actions; omit grammar. | M7, M10 | changed (grammar omitted) | no |
| I06 | Substitutions menu has copy, quotes, dashes, detectors, replacement. | Keep all except Data Detectors. | M7, M12 | changed (Data Detectors omitted) | no |
| I07 | Transformations menu has upper, lower, capitalize. | Keep all three in caret list. | M7, M12 | changed (caret menu) | no |
| I08 | Format menu toggles Prevent Editing and Wrap to Page. | Keep both in caret list. | M7, M9, M13 | changed (caret menu) | no |
| I09 | View menu has size, zoom, gutter, map, full screen. | Keep actions; use F11 for full screen. | M7, M13 | changed (F11 and caret menu) | no |
| I10 | Window menu has minimize, zoom, tab navigation, move, merge. | Keep actions in caret list. | M6, M7, M14 | changed (Windows controls and caret menu) | no |
| I11 | Help menu is empty. | No Help group, because the Mac Help menu is empty. About stays in the caret list. | M7 | left out (nothing to show; choice to check) | no |
| I12 | Settings is 440 wide, fixed size, with three tabs. | Keep three tabs and 440 device-independent pixels. | M8 | same | no |
| I13 | Restore All Defaults sits at lower left. | Keep this button. | M8 | same | no |
| I14 | Settings apply to new windows; line numbers apply now. | Keep this timing. | M8 | same | no |
| I15 | New Document starts with Wrap to Page off. | Start with it off. | M8 | same | no |
| I16 | Window size starts 90 by 30; fields accept 1–10000. | Keep values and reject junk. | M8 | same | no |
| I17 | New Document has a font chooser and font name. | Show Cascadia Mono 10 pt by default. | M8 | changed (Windows font) | no |
| I18 | Spelling, grammar, and autocorrect start off. | Start spelling and autocorrect off; omit grammar. | M8, M10 | changed (grammar omitted) | no |
| I19 | Smart copy starts on; quotes and dashes off; replacement uses system value. | Keep these defaults. | M8, M12 | same | no |
| I20 | Line numbers start off. | Start them off. | M8, M13 | same | no |
| I21 | Add .txt starts on; encoding choices are Automatic or named. | Keep these controls. | M3, M8 | same | no |
| I22 | Keyboard tab has 394×464 panel, 132-wide key column, 18-high rows. | Keep these sizes and the two-column recorder. | M8 | same | no |
| I23 | Escape cancels recording; Delete resets; clashes warn. | Keep these rules. | M8 | same | no |
| I24 | Changed keys show a marker and reset arrow. | Keep both marks. | M8 | same | no |
| I25 | Tab width 8 and autosave delay 30 have no settings control. | Keep both without controls. | M8, M14 | same | no |
| I26 | Paper size defaults to Letter in US/Canada, else A4. | Keep these defaults, with no Settings control, as on the Mac. | M8, M13 | same | no |
| I27 | Text is 13 pt; gutter and map sit outside 90 by 30 text. | Use Cascadia Mono 10 pt; keep side views outside text. | M4, M13 | changed (Windows font size) | no |
| I28 | Gutter fits digits plus 6 left and 8 right, at least 3 digits. | Keep gutter sizing. | M13 | same | no |
| I29 | Gutter numbers use 85% font size, at least 9 pt, at 62% color. | Keep sizing, color, and right alignment. | M13 | same | no |
| I30 | Gutter has 1 px separator and redraws 0.1 s after typing. | Keep both rules. | M13 | same | no |
| I31 | Map is 100 px wide with 2 px lines and narrow blocks. | Keep map geometry. | M13 | same | no |
| I32 | Map box uses 10% fill, 22% outline, 8 px pointer trigger. | Keep box geometry and trigger. | M13 | same | no |
| I33 | Map fades 0.15 s in, 0.25 s out; Reduce Motion skips fade. | Keep timing and accessibility rule. | M13 | same | no |
| I34 | Map click centers; drag scrolls. | Keep both actions. | M13 | same | no |
| I35 | Zoom doubles in, halves out, and Actual Size resets. | Keep steps within 25%–1600%. | M13 | same | no |
| I36 | Pages have 72 pt margins and Letter, A4, Legal sizes. | Use 612×792, 595×842, 612×1008 pt. | M13 | same | no |
| I37 | Warnings, dim text, and headers have Mac colors. | Use Windows light or dark colors with flat Mac look. | M5, M7 | changed (Windows theme setting) | no |
| I38 | Select Line panel is 253 by 62 with field and button. | Keep panel size and controls. | M12 | same | no |
| I39 | Encoding panel is 336 by 187 with checks and list buttons. | Keep panel size and controls. | M7 | same | no |
| I40 | Save dialog adds encoding and .txt controls. | Add both to native Save dialog. | M3, M7 | same | no |
| I41 | Auto decode uses file tag, BOM, UTF-8, then system encoding. | Use BOM, UTF-8, then Windows code page. | M3 | changed (no Mac file tag) | no |
| I42 | Default encodings include UTF-16, UTF-8, Mac Roman, CP1252, Mac Japanese, Shift JIS, Mac Chinese Traditional, Mac Korean, Mac Chinese Simplified, GB 18030. | Keep this list. | M3, M8 | same | no |
| I43 | New files use UTF-8 without BOM; save keeps a UTF-8 BOM. | Keep both rules. | M3 | same | no |
| I44 | UTF-16/32 use little-endian BOM; no-BOM reads write big-endian without BOM. | Keep byte order and BOM rules. | M3 | same | no |
| I45 | Save writes atomically. | Write a replacement safely, then replace old file. | M3 | same | no |
| I46 | Paste keeps line endings untouched. | Keep pasted endings. | M3, M9 | same | no |
| I47 | Restore keeps selection and first visible character. | Restore both. | M14 | same | no |
| I48 | Blank untitled text closes without restore; over 1 MB is not blank. | Keep whitespace and size rules. | M14 | same | no |
| I49 | Launch window can be replaced until edited, moved, or resized. | Keep replacement rule. | M14 | same | no |
| I50 | Date-only outside change keeps text, caret, and undo. | Compare bytes before reload. | M14 | same | no |
| I51 | Over 50 MiB, spelling, substitutions, gutter, map turn off. | Turn them off and show large-file title. | M10, M12, M13, M14 | same | no |
| I52 | Over 512 MiB, file is read-only; read and export still work. | Keep limit, title, and read tools. | M2, M14 | same | no |
| I53 | Save As on large read-only file writes original bytes. | Copy original bytes. | M3, M14 | same | no |
| I54 | Prevent Editing and Wrap to Page belong to window. | Keep choices per window. | M13, M14 | same | no |
| I55 | New docs use wrap setting; disk files start Wrap to Window. | Keep starting modes. | M13, M14 | same | no |
| I56 | Select Line accepts absolute and relative ranges; zero is invalid. | Keep `N`, `N-M`, `+N`, `-N`, and ranges. | M12 | same | no |
| I57 | Select Line Return closes; button stays; bad input beeps. | Keep actions and error text. | M12 | same | no |
| I58 | Line breaks include LF, CR, CRLF, NEL, LS, PS. | Index all six forms. | M2, M12 | same | no |
| I59 | PDF options default header off and rewrap on. | Keep choices and defaults. | M13 | same | no |
| I60 | Mac tab bar appears at two or more tabs. | Always show tab row. | M6 | changed (user decision) | no |
| I61 | APFS clone copies large files. | Copy the bytes for large-file originals, because Windows has no plain clone call on NTFS. | M2, M3, M14 | changed (no APFS clone) | no |
| I62 | Tab shortcuts include Ctrl+Tab, Ctrl+Shift+Tab, Cmd+1–9, Cmd+0. | Use Ctrl+Tab, Ctrl+Shift+Tab, Ctrl+1–9, Ctrl+0. | M8, M14 | changed (Ctrl replaces Cmd) | no |
| I63 | A chosen bad encoding says the file could not open using that encoding. | Name the encoding in the open error. | M3, M7 | same | no |
| I64 | Over 50 MiB and 512 MiB, title shows the large-file or read-only reason. | Show the same reason and size in the title. | M14 | same | no |

## Windows additions

| Mac ID | Behavior | What Windows does | Builds | Status | Built |
| --- | --- | --- | --- | --- | --- |
| W01 | Mac has native window chrome. | Use a Windows Terminal-style top row. | M6 | changed (user decision) | no |
| W02 | Mac hides tab bar at one tab. | Always show one tab. | M6 | changed (user decision) | no |
| W03 | Mac has a new-tab action. | Show `+` beside tabs. | M6 | changed (Windows top row) | no |
| W04 | Mac has a menu bar. | Show caret button beside `+`. | M6 | changed (Windows top row) | no |
| W05 | Mac window buttons use macOS. | Show Windows buttons and support Snap Layouts. | M6 | changed (Windows window controls) | no |
| W06 | Mac menus live in menu bar. | Put every retained menu in caret list. | M7 | changed (user decision) | no |
| W07 | Mac Alt does not open a caret list. | Alt opens caret list. | M7 | changed (user decision) | no |
| W08 | Mac keys mainly use Cmd. | Use Ctrl, Ctrl+H, F3, Alt+F4, Ctrl+Space, F11; allow changes. | M8, M12 | changed (Windows keys and user decision) | no |
| W09 | Mac opens external files in document windows. | Explorer and second launches add tabs to last used window. | M14 | changed (user decision) | no |
| W10 | Mac uses Mac distribution. | Install and update through Microsoft Store. | M15 | changed (user decision) | no |

## Speed goals

These are goals, not measurements. M15 records real numbers.

| Mac ID | Behavior | What Windows does | Builds | Status | Built |
| --- | --- | --- | --- | --- | --- |
| S01 | No Mac speed row. | Warm first frame without MSIX under 60 ms; 30 runs. | M15 | changed (Windows speed goal) | no |
| S02 | No Mac speed row. | Cold first frame under 150 ms after clearing file cache. | M15 | changed (Windows speed goal) | no |
| S03 | No Mac speed row. | Report MSIX cost; fail above 30 ms across four launch paths. | M15 | changed (Windows speed goal) | no |
| S04 | No Mac speed row. | Key handler p99 under 2 ms on 1 MB and 512 MiB files. | M15 | changed (Windows speed goal) | no |
| S05 | No Mac speed row. | Key to screen p50 under 25 ms and p95 under 33 ms. | M15 | changed (Windows speed goal) | no |
| S06 | No Mac speed row. | Second launch to visible tab under 50 ms. | M15 | changed (Windows speed goal) | no |
| S07 | No Mac speed row. | 1 GiB read-only first screen under 100 ms; index under 2 s. | M15 | changed (Windows speed goal) | no |
| S08 | No Mac speed row. | 100 MB editable first screen under 150 ms. | M15 | changed (Windows speed goal) | no |
| S09 | No Mac speed row. | Idle has no timers and no wakeups. | M15 | changed (Windows speed goal) | no |
| S10 | No Mac speed row. | Empty window uses under 40 MB private memory. | M15 | changed (Windows speed goal) | no |
