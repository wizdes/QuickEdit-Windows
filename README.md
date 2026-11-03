# QuickEdit for Windows

A plain-text editor for Windows 11. It opens quickly, and it edits, saves and searches plain text and nothing else. It has no fonts per word, no colors and no images. It is written in Rust on the Windows API, and it looks and behaves like [QuickEdit for Mac](https://github.com/wizdes/QuickEdit), except that its top row works like Windows Terminal.

## What it does

- Tabs in one top row, and more than one window. Files from File Explorer join the last window you used as new tabs.
- Unsaved text is copied to your PC as you type, and comes back after a crash or a restart.
- Find and Replace, word wrap, zoom, and keyboard shortcuts you can change in Settings.
- Spell check, using the checker built into Windows.
- Many text encodings. New files are UTF-8.
- Files over 512 MiB open read-only, so you can view, search and copy from them.
- Print saves a PDF. The app never talks to a printer.

The app has no network code and contacts no server. `packaging/PRIVACY.md` says what it reads and writes.

## Build and test

You need Rust 1.99.1, the Visual Studio C++ build tools and the Windows SDK. `rust-toolchain.toml` picks the Rust version, and `.cargo/config.toml` puts the C runtime inside the exe.

```powershell
cargo build --release
cargo test --workspace --exclude spike-probe --exclude spike-renderers --exclude spike-trials --locked --no-fail-fast
```

The tests start the real app with hidden windows and their own data folders, so nothing shows on screen and your own settings stay as they are.

For a release to the Microsoft Store, build with `packaging\build.ps1`. It also removes the build folder's path from the exe, and it checks the result. A plain `cargo build --release` leaves that path in, so do not ship that exe. `packaging/README.md` has the steps for a Store upload.

## Where things are

| Folder | What it holds |
| --- | --- |
| `crates/qe-app` | `QuickEdit.exe`, which joins all the other crates |
| `crates/qe-docs` | Open, save, autosave, and passing files from a second launch to the running app |
| `crates/qe-editor`, `qe-buffer`, `qe-layout`, `qe-render` | The text control, its storage, line layout and drawing |
| `crates/qe-ui`, `qe-shell`, `qe-views`, `qe-settings`, `qe-tools` | Windows, the top row, panels, Settings, Find and Replace |
| `crates/qe-codec`, `qe-pdf`, `qe-spell`, `qe-ime`, `qe-a11y` | Encodings and safe saving, PDF export, spell check, input methods, screen readers |
| `packaging` | The Store package, its icons, the listing draft and the privacy policy |
| `docs` | The plan, the design spec and the list of differences from the Mac app |

## Licence

MIT. See `LICENSE`. `packaging/NOTICES.txt` lists the outside crates inside the exe.
