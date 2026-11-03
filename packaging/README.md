# Store package

These files make the Microsoft Store package for QuickEdit, for Windows 11 x64. An MSIX file is the package format that the Store takes.

| File | What it does |
| --- | --- |
| `AppxManifest.xml` | Says who made the package, what it runs and which file types it opens |
| `Assets\` | The 72 icon files that the manifest names, drawn by `make-icons.ps1` |
| `QuickEdit.ico`, `QuickEdit.exe.manifest` | The icon and the settings that `crates/qe-app/build.rs` puts inside the exe |
| `build.ps1` | Builds the exe and the package |
| `listing.md` | A draft of the Store page text |
| `PRIVACY.md` | The privacy policy. You must publish it at a web address |
| `NOTICES.txt`, `make-notices.ps1` | The outside crates in the exe and their licences, and the command that lists them from `Cargo.lock` |
| `crates/qe-app/tests/package.rs` | Checks all of this, and packs a real package |

## What a person does

### 1. Fill in the name

Reserve the name in Partner Center. Then open Product identity on that app's page, and copy 3 values into `AppxManifest.xml`. They appear nowhere else.

| In the manifest now | Copy from Product identity |
| --- | --- |
| `Identity Name="PLACEHOLDER.QuickEdit"` | Package/Identity/Name |
| `Identity Publisher="CN=PLACEHOLDER"` | Package/Identity/Publisher |
| `<PublisherDisplayName>PLACEHOLDER` | Package/Properties/PublisherDisplayName |

The test `the_identity_is_all_placeholders_or_all_filled_in` fails if only some of them change. Until all 3 change, `build.ps1` still builds, and it prints a warning that Partner Center will refuse the package.

### 2. Build the upload

```powershell
pwsh -NoProfile -File packaging\build.ps1
```

It writes `target\QuickEdit-<version>-x64.msix` and prints the size. The version comes from `Version` in `AppxManifest.xml`, and `crates/qe-app/build.rs` puts the same number in the exe's version record. Raise it for each upload. The Store needs the last of the 4 numbers to stay 0.

The script does three things that a plain `cargo build --release` does not:

- It passes `--remap-path-prefix` for your home, Cargo, Rust and repo folders, so that your account name is not in the exe. These paths differ on each PC, so `.cargo/config.toml` cannot hold them.
- It stops if the exe imports a C runtime DLL or holds `\Users\`.
- It stops if `NOTICES.txt` no longer matches `Cargo.lock`. Run `pwsh -NoProfile -File packaging\make-notices.ps1` to write it again. The list is made by a script and is not legal advice, so read it before a release.

The package has no signature, and the Store adds its own after review. So this file cannot be installed by double-click. To try the app on a PC with Developer Mode on, use the loose form:

```powershell
pwsh -NoProfile -File packaging\build.ps1 -LooseLayout
Add-AppxPackage -Register target\package\AppxManifest.xml
```

To remove it again, use `Get-AppxPackage *QuickEdit* | Remove-AppxPackage`. This also deletes the app's settings and autosaved text, as "Where the app's data goes" says below.

### 3. Publish the privacy policy

You must publish `PRIVACY.md` at a web address that anyone can open, such as a page on your own site or the repo's file view. First fill in its 2 `PLACEHOLDER` lines. Then put that URL in Partner Center under Privacy policy.

[Store policy 10.5.1](https://learn.microsoft.com/en-us/windows/apps/publish/store-policies) says that "Product types that inherently have access to Personal Information must always have privacy policies. These include, but are not limited to, Desktop Bridge and Win32 products." QuickEdit is a Win32 product in a package, so it needs one even though it collects nothing. [Partner Center](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/support-info) may also mark the question Yes by itself from the package's capabilities.

### 4. Test the install

These steps need a real install, so no automated test here covers them. On a PC with Developer Mode on, run the loose form from step 2, then:

1. **Paths with spaces.** Make a folder named `My Docs`. Select 15 `.log` files in it, and press Enter, with QuickEdit closed. All 15 must open as tabs in one window. Repeat with QuickEdit running.
2. **Open with.** Right-click a `.txt` file and pick Open with, with QuickEdit running and with it closed. See "Choices to check" for what `.txt` may do.
3. **Package rule for flags.** From a terminal in a folder with a file named `--hidden`, run `quickedit.exe --hidden`. A visible window must open with that file. The same line from an unpackaged exe would run with hidden windows.
4. **New file inside AppData.** In the app, use Save As to save `%LOCALAPPDATA%\qe-test\new.txt`, where `qe-test` is a real folder you made first. Then, from another terminal, run `Get-Content "$env:LOCALAPPDATA\qe-test\new.txt"`. If this says the file is missing, Windows sent the file to the app's private folder, and uninstalling will delete it.
5. **Existing file inside AppData.** Make `%LOCALAPPDATA%\qe-test\old.txt` from a terminal with the text `before`. Open it in the app, type, and save. Run the same `Get-Content` on it from another terminal. It must show the new text. Then run `Get-ChildItem "$env:LOCALAPPDATA\Packages\*QuickEdit*\LocalCache\Local" -Recurse` and check that no copy of `old.txt` or a temp file is there.
6. **Roaming AppData.** Repeat steps 4 and 5 under `%APPDATA%\qe-test`, with `LocalCache\Roaming` in the last command.
7. **Saving over the top folder.** Repeat step 5 with a file directly in `%LOCALAPPDATA%`, since Microsoft's table treats some top folders differently.

**If step 4, 5, 6 or 7 fails:** a saved file is a person's document, so it must not vanish. The options are `uap10:RuntimeBehavior="win32App"` or the `unvirtualizedResources` capability with `virtualization:ExcludedDirectory`. Each one changes where the app's own data goes, so "Where the app's data goes" and `PRIVACY.md` then need rewriting. Sources: [Flexible virtualization](https://learn.microsoft.com/en-us/windows/msix/desktop/flexible-virtualization) and [How packaged desktop apps run on Windows](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-behind-the-scenes). Do not add either one before the test shows the need.

To remove the test folders afterward, delete `qe-test` from both places.

### 5. Run Microsoft's checks

Run the Windows App Certification Kit before the upload, from an elevated prompt in a signed-in session. These commands have not been run here, because the kit starts the app and may show windows. The kit is at `C:\Program Files (x86)\Windows Kits\10\App Certification Kit\appcert.exe`.

```powershell
appcert.exe reset
appcert.exe test -appxpackagepath <full path to the .msix> -reportoutputpath <full path to report.xml>
```

An [unsigned package needs a special `OID` value in its `Publisher` to install](https://learn.microsoft.com/en-us/windows/msix/package/unsigned-package), and this manifest has none. So the kit may fail at its install step on the `.msix`. In that case, register the loose form from step 2, and pass `-packagefullname` from `Get-AppxPackage *QuickEdit*` in place of `-appxpackagepath`.

### 6. Fill in the Store page

| Item | What to give |
| --- | --- |
| Description and features | Start from `listing.md` |
| Screenshots | At least 1, and 4 are suggested. PNG, at least 1366 by 768 pixels. Take them from the real app |
| Category | Productivity. This matches the Mac app's category |
| Privacy | Give the URL from step 3. Answer Yes to whether the product accesses personal information, because it reads whatever files a person opens. `listing.md` has text for this |
| Age rating | Answer the IARC questionnaire (the age rating form) in Partner Center. A person must answer it. The answers should say that the app shows only the text of files the person opens |
| `runFullTrust` | Partner Center asks why on the Submission options page. Say: "QuickEdit is a Win32 desktop app written in Rust. It runs as a normal desktop process, so the package must declare runFullTrust. It uses no other restricted capability." |

### 7. What the Store's review looks for

- **The kit's checks pass.** For a full-trust app these cover a valid package and manifest, the file and capability rules, and that the app starts and stops cleanly.
- **The app does what the listing says.** The reviewer runs it, so every feature in the listing must be there.
- **A reason for `runFullTrust`.** See the table above. A reviewer reads it. Microsoft says this can add time to the first review.
- **Clean install and uninstall.** The package holds one exe and needs no install script. Nothing runs at startup, and there is no service.
- **No taking over.** The app must not set itself as the default for a file type. This manifest asks for no such thing, and Windows asks the person.
- **An accurate age rating and privacy answer**, as above.

## Choices in the manifest

### File types

The Mac app opens anything macOS calls plain text or text, and shows other data as a viewer. Windows has no such group, so this is a fixed list. It starts with the 4 the plan names, then adds the Mac README's "Markdown, JSON, source code", plus common config and data formats. Being on the list means the app shows in Open with. It never makes the app the default.

<!-- extensions start -->
`.txt` `.text` `.log` `.md` `.markdown` `.json` `.jsonc` `.csv` `.tsv` `.yaml` `.yml` `.toml` `.ini` `.cfg` `.conf` `.diff` `.patch` `.rst` `.tex` `.c` `.h` `.cc` `.cpp` `.hpp` `.cs` `.css` `.go` `.java` `.jsx` `.kt` `.lua` `.rs` `.sh` `.sql` `.swift` `.tsx`
<!-- extensions end -->

These are left out:

- `.js`, `.py`, `.rb`, `.pl`, `.ps1`, `.bat`, `.cmd`, `.reg`, `.vb` and others, because Microsoft's reserved list says Windows ignores a registration for them.
- `.xml`, `.htm`, `.html` and `.rtf`, for the same reason.
- `.ts`, because it is also a video type.
- The Mac's "any data" viewer row. It has no match in this list.

The reserved list also names `.txt`. Microsoft's own sample in the packaging page registers `.txt`, and Microsoft's File Explorer page says a file type "already taken" appears once the person picks the app as the default. So `.txt` stays on the list. This has not been checked on a real install. A file from the Open dialog, or one passed on the command line, opens whatever its type.

### Several files at once

The association sets `MultiSelectModel="Document"` and `Parameters="&quot;%1&quot;"`, which is `"%1"` with its quotes.

- **15 files selected:** Windows starts `QuickEdit.exe` 15 times, each with one quoted path. The first to take the pipe is the app. The others pass their one file through the pipe and exit. The tabs join the last used window in the order the pipe takes them, which may differ from the order in Explorer.
- **Why not `Player`:** `Player` passes all the paths on one line, and a [report in Microsoft's docs repository](https://github.com/MicrosoftDocs/winrt-related/issues/124) says they arrive "separated by spaces", with no word on quotes. A path such as `C:\Users\First Last\a.txt` would then split in two. Nothing in Microsoft's docs settles it, and `Document` with a quoted `%1` avoids the question.
- **Where the attributes go:** [uap3:FileTypeAssociation](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-uap3-filetypeassociation) lists both `Parameters` and `MultiSelectModel` on the association itself, and [Microsoft's File Explorer page](https://github.com/MicrosoftDocs/windows-dev-docs/blob/docs/hub/apps/desktop/modernize/integrate-packaged-app-with-file-explorer.md) says `Parameters` there is what a double-click passes. So no separate `uap3:Verb` is needed, and none adds a menu line.
- **Measured here:** the test `fifteen_real_launches_at_once_with_spaces_land_in_one_window` starts 15 real exes at once, with quoted paths in a folder with spaces. All arrive in one window.
- **Over 15 files:** [Microsoft's Win32 page](https://learn.microsoft.com/en-us/windows/win32/shell/how-to-employ-the-verb-selection-model) says a `Document` verb stops appearing past 15 selected items. That page is about the registry form, so the limit for a package has not been checked. Step 4 of "What a person does" covers the real install.

### Second launches

The handoff in `crates/qe-docs/README.md` needs each launch to run its own process, so that the process can find the pipe, send its files and exit.

- `uap10:SupportsMultipleInstances="true"` asks for this. Microsoft's page prefers the `uap10` form over `desktop4` for apps on build 19041 or later, and this package needs 22000.
- The pipe's name is built from the user, the session and the integrity level. A full-trust package runs at medium integrity with no object isolation, as `uap16:BaseNamedObjectsIsolation` is not set. So the name does not change inside the package. A copy run from a folder and a packaged copy use the same pipe, and so they hand off to each other.

### Alias

`quickedit.exe` starts the app from a terminal, by way of the `uap3:AppExecutionAlias` extension. A person can turn it off in Settings, under App execution aliases.

### Where the app's data goes

The app keeps its settings, session and autosave under `%LOCALAPPDATA%\QuickEdit`. Inside a package, Windows sends new files there to a private folder for this package, `%LOCALAPPDATA%\Packages\<package family name>\LocalCache\Local\QuickEdit`, and shows them to the app at the normal path.

- **The app reads what it wrote.** It needs no code change.
- **A person's own files in `AppData`:** Save As or a save inside `%APPDATA%` or `%LOCALAPPDATA%` may be redirected in the same way. Step 4 of "What a person does" tests it.
- **No `FileSystemWriteVirtualization` setting.** Turning this off needs the `unvirtualizedResources` restricted capability, which Microsoft says is not meant for apps like this, and nothing outside the app needs these files.
- **Uninstalling deletes this data.** That includes autosaved text that was never saved to a file. The listing and the app's help should say so.
- **Other programs cannot see the folder at the normal path.** To find a `.damaged` copy, a person must look under `Packages`.

**A trap on a PC that ran a loose exe.** Microsoft's page says the app opens the private folder first, then the real one, and that a change to a file in the real folder is not redirected. So on a PC that already has a real `%LOCALAPPDATA%\QuickEdit`, some files could be in one place and some in the other. A Store customer has no such folder. This PC may have one. Test the package with that folder both present and renamed away. This has not been tested.

## The exe's own manifest

`crates/qe-app/build.rs` compiles `QuickEdit.exe.manifest`, `QuickEdit.ico` and a version record with `rc.exe`, and adds them to the bin only.

| Setting | Why |
| --- | --- |
| `dpiAwareness` `PerMonitorV2` | The process starts in this mode. `qe-shell` still makes its run-time check, finds the mode already set, and does nothing |
| No `activeCodePage` | `qe-codec` reads the system code page through `GetACP` as a step of Automatic. Setting UTF-8 would change how old files decode |
| `longPathAware` | Paths over 260 characters work where Windows has them turned on |
| `supportedOS` | One id means Windows 10 and 11 |
| `asInvoker` | The app never asks for admin rights |

| Build | With no `rc.exe` |
| --- | --- |
| Release | Stops with `QE_RC_MISSING` |
| Debug | Prints a cargo warning and builds with no manifest or icon |

`QE_RC` names an `rc.exe` to use, in place of the search.

## How the exe is built for the Store

- **C runtime inside the exe.** `.cargo/config.toml` sets `+crt-static`, so the exe needs no `VCRUNTIME140.dll` or `api-ms-win-crt-*` DLL. [Microsoft says](https://learn.microsoft.com/en-us/troubleshoot/developer/visualstudio/cpp/libraries/c-runtime-packages-desktop-bridge) a packaged app that uses the shared runtime must name its framework package. This one uses none, so its manifest names none. The test `the_exe_imports_no_c_runtime` reads the import table.
- **No flags in a package.** When `GetCurrentPackageFullName` says the process has a package, every argument is a file. So a file named `--hidden` opens, and the test flags cannot run. The test `a_packaged_run_reads_every_flag_as_a_file` asks the exe how a package would read a line.
- **Where DLLs load from.** `/DEPENDENTLOADFLAG:0x800` keeps the exe's own imports to System32, and `main` calls `SetDefaultDllDirectories` first, so later loads come from System32 alone. The app ships no DLLs, so its own folder is out of the search too. This matters only for a loose copy of the exe, as a package's folder is locked. A shell extension in the Open dialog that loads a helper DLL from `PATH` would fail to load it.
- **The pipe.** See `crates/qe-docs/src/handoff.rs`. A launch sends nothing until the server has the same user and level, and the pipe has one rule, for its own user.

## Icons

`make-icons.ps1` draws all of them with `System.Drawing`, from numbers copied from the Mac app's `QuickEdit.icon` files. Run it again after any change, and commit the results.

- **Under 64 px:** a simpler layout on a 16 by 16 grid, with thicker rows and no ruled lines, so that a 16 px icon can still be read.
- **Not copied:** Icon Composer's shadow under the caret, its glass effects and the Mac's dark look.
- **Tiles:** Windows 11 shows no live tiles, but the Store still asks for the files.

## Sources

| Decision | Source |
| --- | --- |
| Quoted `Parameters` on the association | [Integrate a packaged desktop app with File Explorer](https://github.com/MicrosoftDocs/windows-dev-docs/blob/docs/hub/apps/desktop/modernize/integrate-packaged-app-with-file-explorer.md), [uap3:FileTypeAssociation](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-uap3-filetypeassociation) |
| Alias, file types, `MultiSelectModel`, the default at 15 files | [Integrate your desktop app with Windows using packaging extensions](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/desktop-to-uwp-extensions) |
| `MultiSelectModel` as an attribute | [uap3:FileTypeAssociation](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-uap3-filetypeassociation) |
| Limits of 15 and 100 items | [How to employ the verb selection model](https://learn.microsoft.com/en-us/windows/win32/shell/how-to-employ-the-verb-selection-model) |
| What `Player` does in a package, as measured by someone else, and why it is not used | [winrt-related issue 124](https://github.com/MicrosoftDocs/winrt-related/issues/124) |
| "Already taken" types such as `.txt` | [Integrate a packaged desktop app with File Explorer](https://github.com/MicrosoftDocs/windows-dev-docs/blob/docs/hub/apps/desktop/modernize/integrate-packaged-app-with-file-explorer.md) |
| Reserved file types | [reserved-uri-scheme-names.md](https://github.com/MicrosoftDocs/windows-dev-docs/blob/docs/hub/apps/develop/launch/reserved-uri-scheme-names.md) in Microsoft's docs repository. The learn.microsoft.com address now redirects to "Handle URI activation" |
| `SupportsMultipleInstances`, `uap10` over `desktop4` | [Create a multi-instance app](https://learn.microsoft.com/en-us/windows/uwp/launch-resume/multi-instance-uwp), [Application](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-application) |
| Alias elements | [uap3:AppExecutionAlias](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-uap3-appexecutionalias), [desktop:ExecutionAlias](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-desktop-executionalias) |
| `AppData` redirection, removal on uninstall | [How packaged desktop apps run on Windows](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-behind-the-scenes), [Flexible virtualization](https://learn.microsoft.com/en-us/windows/msix/desktop/flexible-virtualization) |
| `FileSystemWriteVirtualization` and its capability | [virtualization:FileSystemWriteVirtualization](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-virtualization-filesystemwritevirtualization), [App capability declarations](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/app-capability-declarations) |
| `runFullTrust` | [Generating MSIX package components](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-manual-conversion), [App capability declarations](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/app-capability-declarations) |
| Icon sizes, `altform-unplated`, `altform-lightunplated` | [App icon construction](https://learn.microsoft.com/en-us/windows/apps/design/iconography/app-icon-construction) |
| Exe manifest settings | [Application manifests](https://learn.microsoft.com/en-us/windows/win32/sbscs/application-manifests) |
| Privacy and age rating rules | [Microsoft Store Policies](https://learn.microsoft.com/en-us/windows/apps/publish/store-policies), [Age ratings](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/age-ratings) |
| Required Store page fields, screenshots | [Create an app submission](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/create-app-submission), [Screenshots and images](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msix/screenshots-and-images) |
| The certification kit | [Windows App Certification Kit](https://learn.microsoft.com/en-us/windows/uwp/debug-test-perf/windows-app-certification-kit), [its tests](https://learn.microsoft.com/en-us/windows/uwp/debug-test-perf/windows-app-certification-kit-tests) |

## Choices to check

1. **`.txt`.** Windows' list says a registration for a reserved type is ignored, and it names `.txt`. Microsoft's sample registers `.txt` too. Step 4 shows which is true here. If `.txt` is ignored, change the listing's File Explorer line to say which types it covers.
2. **File list.** It is short on purpose. Adding a type is one line here and one in the manifest.
3. **App version.** The package and the exe's version record say `1.0.0.0`, from the manifest. The workspace's `Cargo.toml` says `0.1.0`, and nothing shows it.
4. **The name.** [QuickEdit Text Editor](https://play.google.com/store/apps/details?hl=en&id=com.rhmsoft.edit) is on Google Play. Store policy 10.1.1 bars a name "the same as that of other products" unless you publish them. Name reservation may still pass, but a person decides this.
