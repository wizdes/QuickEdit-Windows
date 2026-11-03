# Store package

These files make the Microsoft Store package for QuickEdit, for Windows 11 x64. An MSIX file is the package format that the Store takes.

| File | What it does |
| --- | --- |
| `AppxManifest.xml` | Says who made the package, what it runs and which file types it opens |
| `Assets\` | The 72 icon files that the manifest names, drawn by `make-icons.ps1` |
| `QuickEdit.ico`, `QuickEdit.exe.manifest` | The icon and the settings that `crates/qe-app/build.rs` puts inside the exe |
| `build.ps1` | Builds the exe and the package |
| `listing.md` | A draft of the Store page text |
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

It writes `target\QuickEdit-<version>-x64.msix` and prints the size. The version comes from `Version` in `AppxManifest.xml`. Raise it for each upload. The Store needs the last of the 4 numbers to stay 0.

The package has no signature, and the Store adds its own after review. So this file cannot be installed by double-click. To try the app on a PC with Developer Mode on, use the loose form:

```powershell
pwsh -NoProfile -File packaging\build.ps1 -LooseLayout
Add-AppxPackage -Register target\package\AppxManifest.xml
```

To remove it again, use `Get-AppxPackage *QuickEdit* | Remove-AppxPackage`. This also deletes the app's settings and autosaved text, as "Where the app's data goes" says below.

### 3. Run Microsoft's checks

Run the Windows App Certification Kit before the upload, from an elevated prompt in a signed-in session. These commands have not been run here, because the kit starts the app and may show windows. The kit is at `C:\Program Files (x86)\Windows Kits\10\App Certification Kit\appcert.exe`.

```powershell
appcert.exe reset
appcert.exe test -appxpackagepath <full path to the .msix> -reportoutputpath <full path to report.xml>
```

### 4. Fill in the Store page

| Item | What to give |
| --- | --- |
| Description and features | Start from `listing.md` |
| Screenshots | At least 1, and 4 are suggested. PNG, at least 1366 by 768 pixels. Take them from the real app |
| Category | Productivity. This matches the Mac app's category |
| Privacy | The app collects nothing, stores nothing away from this PC and has no network code. Answer that it does not access, collect or send personal information. Store policy 10.5.1 asks for a privacy policy web address only for an app that does, or where a law asks. See "Choices to check" |
| Age rating | Answer the IARC questionnaire (the age rating form) in Partner Center. A person must answer it. The answers should say that the app shows only the text of files the person opens |
| `runFullTrust` | Partner Center asks why on the Submission options page. Say: "QuickEdit is a Win32 desktop app written in Rust. It runs as a normal desktop process, so the package must declare runFullTrust. It uses no other restricted capability." |

### 5. What the Store's review looks for

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

The association sets `MultiSelectModel="Player"`.

- **15 files selected:** Windows starts `QuickEdit.exe` one time, with all 15 paths on its command line. With no running copy, that launch is the app. With a running copy, it passes all 15 through the pipe, in one message, and exits. So they arrive as tabs in the last used window.
- **Why it is set here:** Microsoft's page says that with no setting the default is `Player` for 15 files or fewer, and `Document` above that, which starts one process for each file. Setting it keeps one rule for all counts.
- **Over 100 files:** Microsoft's Win32 page says a verb of this kind stops appearing past 100 items for `Player`, and past 15 for `Document`. This page is about the registry form, so the limit for a package has not been checked.
- **The fallback:** `Document` also works with the handoff code, as 15 launches of one path each, with more processes.

**Not yet checked on an install.** A report in Microsoft's docs repository says the paths arrive "separated by spaces", and it does not say whether a path with a space in it is quoted. `crates/qe-app/src/args.rs` takes each argument as one file, so an unquoted path would break. Test these after registering:

1. Select 15 files in a folder whose name has a space, and press Enter.
2. Open `.txt` from Open with, with the app running and with it closed.

If the paths split at spaces, change `Player` to `Document`.

### Second launches

The handoff in `crates/qe-docs/README.md` needs each launch to run its own process, so that the process can find the pipe, send its files and exit.

- `uap10:SupportsMultipleInstances="true"` asks for this. Microsoft's page prefers the `uap10` form over `desktop4` for apps on build 19041 or later, and this package needs 22000.
- The pipe's name is built from the user, the session and the integrity level. A full-trust package runs at medium integrity with no object isolation, as `uap16:BaseNamedObjectsIsolation` is not set. So the name does not change inside the package. A copy run from a folder and a packaged copy use the same pipe, and so they hand off to each other.

### Alias

`quickedit.exe` starts the app from a terminal, by way of the `uap3:AppExecutionAlias` extension. A person can turn it off in Settings, under App execution aliases.

### Where the app's data goes

The app keeps its settings, session and autosave under `%LOCALAPPDATA%\QuickEdit`. Inside a package, Windows sends new files there to a private folder for this package, `%LOCALAPPDATA%\Packages\<package family name>\LocalCache\Local\QuickEdit`, and shows them to the app at the normal path.

- **The app reads what it wrote.** It needs no code change.
- **No `FileSystemWriteVirtualization` setting.** Turning this off needs the `unvirtualizedResources` restricted capability, which Microsoft says is not meant for apps like this, and nothing outside the app needs these files.
- **Uninstalling deletes this data.** That includes autosaved text that was never saved to a file. The listing and the app's help should say so.
- **Other programs cannot see the folder at the normal path.** To find a `.damaged` copy, a person must look under `Packages`.

**A trap on a PC that ran a loose exe.** Microsoft's page says the app opens the private folder first, then the real one, and that a change to a file in the real folder is not redirected. So on a PC that already has a real `%LOCALAPPDATA%\QuickEdit`, some files could be in one place and some in the other. A Store customer has no such folder. This PC may have one. Test the package with that folder both present and renamed away. This has not been tested.

## The exe's own manifest

`crates/qe-app/build.rs` compiles `QuickEdit.exe.manifest` and `QuickEdit.ico` with `rc.exe`, and adds them to the bin only.

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

## Icons

`make-icons.ps1` draws all of them with `System.Drawing`, from numbers copied from the Mac app's `QuickEdit.icon` files. Run it again after any change, and commit the results.

- **Under 64 px:** a simpler layout on a 16 by 16 grid, with thicker rows and no ruled lines, so that a 16 px icon can still be read.
- **Not copied:** Icon Composer's shadow under the caret, its glass effects and the Mac's dark look.
- **Tiles:** Windows 11 shows no live tiles, but the Store still asks for the files.

## Sources

| Decision | Source |
| --- | --- |
| Alias, file types, `MultiSelectModel`, the default at 15 files | [Integrate your desktop app with Windows using packaging extensions](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/desktop-to-uwp-extensions) |
| `MultiSelectModel` as an attribute | [uap3:FileTypeAssociation](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-uap3-filetypeassociation) |
| Limits of 15 and 100 items | [How to employ the verb selection model](https://learn.microsoft.com/en-us/windows/win32/shell/how-to-employ-the-verb-selection-model) |
| What `Player` does in a package, as measured by someone else | [winrt-related issue 124](https://github.com/MicrosoftDocs/winrt-related/issues/124) |
| "Already taken" types such as `.txt` | [Integrate a packaged desktop app with File Explorer](https://github.com/MicrosoftDocs/windows-dev-docs/blob/docs/hub/apps/desktop/modernize/integrate-packaged-app-with-file-explorer.md) |
| Reserved file types | [Reserved file and URI scheme names](https://learn.microsoft.com/en-us/windows/apps/develop/launch/reserved-uri-scheme-names) |
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

1. **Privacy address.** The rule asks for an address only for an app that collects personal information. A short page saying "QuickEdit collects nothing" costs little, and it answers a reviewer who asks.
2. **File list.** It is short on purpose. Adding a type is one line here and one in the manifest.
3. **App version.** The package says `1.0.0.0`, and the workspace's `Cargo.toml` says `0.1.0`. The exe shows neither at present.
