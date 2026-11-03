# Store listing draft

A draft for the Store page. It uses only facts from this repo and the Mac app's README. A person checks each line against the finished app before upload.

## Name

QuickEdit

## Short description

A fast plain-text editor for Windows 11.

## Description

QuickEdit is a plain-text editor. It opens, edits and saves plain text, and it starts quickly.

It works with text only. There are no fonts per word, no colors, no images and no tables. It never writes RTF or Word files.

Open a file from File Explorer and it joins the window you used last, as a new tab. Unsaved text is copied to your PC as you work, so it comes back after a crash or a restart.

New files are saved as UTF-8, and you can pick another encoding when you open or save a file.

Very large files open too. A file over 512 MiB opens read-only, so you can view it, search it and copy from it.

QuickEdit collects nothing about you. It has no network code and contacts no server. If you open a file on a network share, Windows reads it for you.

## Features

- Tabs, with one tab bar in the style of Windows Terminal
- Find and Replace
- Spell check, using the checker built into Windows. Words you add go to your Windows dictionary
- Word wrap and zoom
- Keyboard shortcuts you can change in Settings
- Reads and writes many text encodings
- Autosave that brings back unsaved text after a crash
- Print saves a PDF. QuickEdit never talks to a printer

## Good to know

- Needs Windows 11 on a 64-bit Intel or AMD PC.
- Uninstalling deletes QuickEdit's settings and any autosaved text that you have not saved to a file. Save your work first.

## Keywords

text editor, plain text, txt, markdown, log viewer

## Copyright

© 2026 Yi Li

## What is new in this version

The first release.

## Privacy answer for Partner Center

Answer Yes to "accesses, collects or transmits personal information", and give the address where you published `PRIVACY.md`. Store policy 10.5.1 asks every Win32 app for a policy, and this app reads whatever files a person opens.

If there is a box for a short description, use this:

QuickEdit reads and writes the files you open or save, and keeps settings and autosaved text in its own folder on your PC. Words you add in spell check go to your Windows dictionary. QuickEdit has no network code and contacts no server, and if you open a file on a network share, Windows reads it for you.

## Check before upload

These lines come from plans and need a look at the finished app:

- "Keyboard shortcuts you can change in Settings" comes from `docs/plan.md`, decision 3.
- "Word wrap and zoom" comes from `qe-views` and `docs/design-spec.md`.
- "Open a file from File Explorer" needs the real test in `README.md`, under "Several files at once". Windows reserves `.txt` for its own app, so check whether Explorer offers QuickEdit for it before the listing leans on that line.
