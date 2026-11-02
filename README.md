# QuickEdit for Windows

A fast plain-text editor for Windows 11. It is a new app written in Rust on the Windows API. It looks and behaves like [QuickEdit for Mac](https://github.com/wizdes/QuickEdit), except that its top row works like Windows Terminal.

It is under construction. Read `docs/STATE.md` for where it stands.

## Build and test

You need Rust 1.99.1 and the Visual Studio C++ build tools.

```powershell
cargo build --release
cargo test --workspace
```
