//! The Store package's files: the manifest, the icons, the exe's own manifest and a real pack.
//! No test here installs anything or shows a window.
use std::collections::BTreeSet;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const NO_WINDOW: u32 = 0x0800_0000;
const SCALES: [u32; 5] = [100, 125, 150, 200, 400];
const TARGET_SIZES: [u32; 14] = [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256];
const ALT_FORMS: [&str; 3] = ["", "_altform-unplated", "_altform-lightunplated"];

fn packaging() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging")
}

fn read_manifest() -> Vec<Element> {
    let text = std::fs::read_to_string(packaging().join("AppxManifest.xml")).unwrap();
    parse_xml(&text).unwrap()
}

/// One element, with its depth, its name, its attributes and the text just inside it.
#[derive(Debug)]
struct Element {
    name: String,
    attributes: Vec<(String, String)>,
    text: String,
}

impl Element {
    fn attr(&self, name: &str) -> Option<&str> {
        let found = self.attributes.iter().find(|(key, _)| key == name);
        found.map(|(_, value)| value.as_str())
    }
}

fn find<'a>(elements: &'a [Element], name: &str) -> &'a Element {
    let mut found = elements.iter().filter(|element| element.name == name);
    let first = found.next().unwrap_or_else(|| panic!("no <{name}>"));
    assert!(found.next().is_none(), "more than one <{name}>");
    first
}

/// A small parser that checks well-formedness: tags balance, attributes are quoted and not
/// repeated, one root element, and no text outside it. It does not read a DTD, which a package
/// manifest must not have.
fn parse_xml(text: &str) -> Result<Vec<Element>, String> {
    let mut rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut open: Vec<String> = Vec::new();
    let mut elements: Vec<Element> = Vec::new();
    let mut roots = 0;
    while !rest.is_empty() {
        let Some(start) = rest.find('<') else {
            if rest.trim().is_empty() {
                break;
            }
            return Err(format!("text after the root: {rest:.40}"));
        };
        let between = &rest[..start];
        if !between.trim().is_empty() {
            if open.is_empty() {
                return Err(format!("text outside the root: {between:.40}"));
            }
            if between.contains('&') && !known_entities(between) {
                return Err(format!("a bad entity in: {between:.40}"));
            }
            if let Some(last) = elements.last_mut() {
                last.text.push_str(between.trim());
            }
        }
        rest = &rest[start..];
        if let Some(after) = rest.strip_prefix("<!--") {
            let end = after.find("-->").ok_or("a comment does not end")?;
            rest = &after[end + 3..];
        } else if let Some(after) = rest.strip_prefix("<?") {
            let end = after.find("?>").ok_or("a declaration does not end")?;
            rest = &after[end + 2..];
        } else if rest.starts_with("<!") {
            return Err("a DTD or CDATA section".into());
        } else if let Some(after) = rest.strip_prefix("</") {
            let end = after.find('>').ok_or("a closing tag does not end")?;
            let name = after[..end].trim();
            match open.pop() {
                Some(expected) if expected == name => {}
                other => return Err(format!("</{name}> closes {other:?}")),
            }
            rest = &after[end + 1..];
        } else {
            let end = tag_end(rest).ok_or("a tag does not end")?;
            let tag = &rest[1..end];
            let (tag, empty) = match tag.strip_suffix('/') {
                Some(inner) => (inner, true),
                None => (tag, false),
            };
            let element = parse_tag(tag)?;
            if open.is_empty() {
                roots += 1;
                if roots > 1 {
                    return Err("two root elements".into());
                }
            }
            if !empty {
                open.push(element.name.clone());
            }
            elements.push(element);
            rest = &rest[end + 1..];
        }
    }
    if !open.is_empty() {
        return Err(format!("not closed: {open:?}"));
    }
    if roots != 1 {
        return Err("no root element".into());
    }
    Ok(elements)
}

/// The `>` that ends a tag, past any `>` inside a quoted value.
fn tag_end(text: &str) -> Option<usize> {
    let mut quote = None;
    for (at, c) in text.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if q == c => quote = None,
            (None, '>') => return Some(at),
            _ => {}
        }
    }
    None
}

fn known_entities(text: &str) -> bool {
    text.split('&').skip(1).all(|part| {
        ["amp;", "lt;", "gt;", "quot;", "apos;"]
            .iter()
            .any(|entity| part.starts_with(entity))
    })
}

fn parse_tag(tag: &str) -> Result<Element, String> {
    let tag = tag.trim();
    let name_end = tag.find(char::is_whitespace).unwrap_or(tag.len());
    let name = &tag[..name_end];
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(format!("a bad tag name in <{tag:.40}>"));
    }
    let mut attributes: Vec<(String, String)> = Vec::new();
    let mut rest = tag[name_end..].trim_start();
    while !rest.is_empty() {
        let equal = rest
            .find('=')
            .ok_or(format!("an attribute with no value: {rest:.40}"))?;
        let key = rest[..equal].trim().to_string();
        let after = rest[equal + 1..].trim_start();
        let quote = after.chars().next().filter(|c| matches!(c, '"' | '\''));
        let quote = quote.ok_or(format!("{key} has no quotes"))?;
        let close = after[1..]
            .find(quote)
            .ok_or(format!("{key} does not end"))?;
        let value = &after[1..close + 1];
        if value.contains('<') || (value.contains('&') && !known_entities(value)) {
            return Err(format!("{key} holds a bad character"));
        }
        if attributes.iter().any(|(seen, _)| *seen == key) {
            return Err(format!("{key} comes twice"));
        }
        attributes.push((key, value.to_string()));
        rest = after[close + 2..].trim_start();
    }
    Ok(Element {
        name: name.to_string(),
        attributes,
        text: String::new(),
    })
}

/// The width and height in a PNG's first chunk.
fn png_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "{}", path.display());
    assert_eq!(&bytes[12..16], b"IHDR", "{}", path.display());
    let number = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (number(16), number(20))
}

/// The base size that the manifest's logo name carries.
fn base_size(file: &str) -> (u32, u32) {
    match file {
        "Square44x44Logo" => (44, 44),
        "Square71x71Logo" => (71, 71),
        "Square150x150Logo" => (150, 150),
        "Wide310x150Logo" => (310, 150),
        "Square310x310Logo" => (310, 310),
        "StoreLogo" => (50, 50),
        other => panic!("the manifest names a logo this test does not know: {other}"),
    }
}

/// Each logo the manifest names, as a file stem under `Assets`.
fn logo_names(elements: &[Element]) -> BTreeSet<String> {
    let mut paths = Vec::new();
    for element in elements {
        for (key, value) in &element.attributes {
            if key.ends_with("Logo") {
                paths.push(value.clone());
            }
        }
        if matches!(element.name.as_str(), "Logo" | "uap:Logo") {
            paths.push(element.text.clone());
        }
    }
    assert!(paths.len() >= 6, "{paths:?}");
    paths
        .iter()
        .map(|path| {
            let stem = path
                .strip_prefix("Assets\\")
                .unwrap_or_else(|| panic!("{path}"));
            stem.strip_suffix(".png")
                .unwrap_or_else(|| panic!("{path}"))
                .to_string()
        })
        .collect()
}

#[test]
fn the_small_parser_refuses_broken_xml() {
    for bad in [
        "<a><b></a></b>",
        "<a>",
        "<a/><b/>",
        "text<a/>",
        "<a x=1/>",
        "<a x='1' x='2'/>",
        "<a x='1/>",
        "<a>&nope;</a>",
        "<!DOCTYPE a><a/>",
        "",
    ] {
        assert!(parse_xml(bad).is_err(), "{bad}");
    }
    let good = parse_xml("<?xml version='1.0'?><!-- c --><a x='>'><b y=\"&amp;\"/>t</a>").unwrap();
    assert_eq!(good.len(), 2);
    assert_eq!(good[0].attr("x"), Some(">"));
}

#[test]
fn the_identity_is_all_placeholders_or_all_filled_in() {
    let elements = read_manifest();
    let identity = find(&elements, "Identity");
    let name = identity.attr("Name").unwrap();
    let publisher = identity.attr("Publisher").unwrap();
    let display = &find(&elements, "PublisherDisplayName").text;
    let values = [name, publisher, display.as_str()];
    if values.iter().any(|value| value.contains("PLACEHOLDER")) {
        assert_eq!(
            values,
            ["PLACEHOLDER.QuickEdit", "CN=PLACEHOLDER", "PLACEHOLDER"],
            "a person filled in only some of the 3 values"
        );
    } else {
        assert!(
            name.ends_with(".QuickEdit") || name.contains("QuickEdit"),
            "{name}"
        );
        assert!(publisher.starts_with("CN="), "{publisher}");
        assert!(!display.is_empty());
    }
    // The Store takes only a zero in the last place of a version.
    let version: Vec<u32> = identity
        .attr("Version")
        .unwrap()
        .split('.')
        .map(|part| part.parse().unwrap())
        .collect();
    assert_eq!(version.len(), 4);
    assert_eq!(version[3], 0);
    assert_eq!(identity.attr("ProcessorArchitecture"), Some("x64"));
    let family = find(&elements, "TargetDeviceFamily");
    assert_eq!(family.attr("Name"), Some("Windows.Desktop"));
    assert_eq!(family.attr("MinVersion"), Some("10.0.22000.0"));
}

#[test]
fn the_app_entry_runs_quickedit_exe_with_full_trust_and_each_launch_its_own_process() {
    let elements = read_manifest();
    let application = find(&elements, "Application");
    assert_eq!(application.attr("Executable"), Some("QuickEdit.exe"));
    assert_eq!(
        application.attr("EntryPoint"),
        Some("Windows.FullTrustApplication")
    );
    assert_eq!(
        application.attr("uap10:SupportsMultipleInstances"),
        Some("true")
    );
    // The name cargo builds is the name the manifest runs.
    let exe = Path::new(env!("CARGO_BIN_EXE_QuickEdit"));
    assert_eq!(exe.file_name().unwrap(), "QuickEdit.exe");
    let alias = find(&elements, "uap3:Extension");
    assert_eq!(alias.attr("Category"), Some("windows.appExecutionAlias"));
    assert_eq!(alias.attr("Executable"), Some("QuickEdit.exe"));
    assert_eq!(
        find(&elements, "desktop:ExecutionAlias").attr("Alias"),
        Some("quickedit.exe")
    );
    // runFullTrust is the only restricted capability, and nothing else is asked for.
    let capabilities: Vec<_> = elements
        .iter()
        .filter(|element| element.name.ends_with("Capability"))
        .map(|element| element.attr("Name").unwrap())
        .collect();
    assert_eq!(capabilities, ["runFullTrust"]);
    // Each file is one launch with one quoted argument, so a path with a space stays whole. The
    // parser here does not expand entities, so this is the text as it stands in the file.
    let association = find(&elements, "uap3:FileTypeAssociation");
    assert_eq!(association.attr("MultiSelectModel"), Some("Document"));
    assert_eq!(association.attr("Parameters"), Some("&quot;%1&quot;"));
}

/// The list between the two marks in `packaging/README.md`.
fn readme_extensions() -> BTreeSet<String> {
    let text = std::fs::read_to_string(packaging().join("README.md")).unwrap();
    let start = text
        .find("<!-- extensions start -->")
        .expect("the start mark");
    let end = text.find("<!-- extensions end -->").expect("the end mark");
    let list: BTreeSet<String> = text[start..end]
        .split('`')
        .filter(|part| part.starts_with('.'))
        .map(str::to_string)
        .collect();
    assert!(list.len() > 4);
    list
}

#[test]
fn the_associations_match_the_readme_and_avoid_reserved_types() {
    let elements = read_manifest();
    let in_manifest: Vec<String> = elements
        .iter()
        .filter(|element| element.name == "uap:FileType")
        .map(|element| element.text.clone())
        .collect();
    let unique: BTreeSet<String> = in_manifest.iter().cloned().collect();
    assert_eq!(unique.len(), in_manifest.len(), "a type comes twice");
    assert_eq!(unique, readme_extensions());
    for required in [".txt", ".text", ".log", ".md"] {
        assert!(unique.contains(required), "{required}");
    }
    // From Microsoft's list of types an app's registration for is ignored. The list also names
    // `.txt`, and Microsoft's own samples register it, so packaging/README.md covers that one.
    for reserved in [
        ".bat", ".cmd", ".com", ".exe", ".js", ".lnk", ".ps1", ".py", ".pl", ".rb", ".reg", ".vb",
        ".vbs", ".xaml", ".tmp", ".inf", ".htm", ".html", ".rtf", ".url", ".xml",
    ] {
        assert!(!unique.contains(reserved), "{reserved} is reserved");
    }
    // Nothing in the manifest asks to take over as the default.
    assert!(
        elements
            .iter()
            .all(|e| e.attr("desktop2:AllowSilentDefaultTakeOver").is_none())
    );
}

#[test]
fn every_logo_has_every_scale_at_the_right_pixel_size() {
    let elements = read_manifest();
    let assets = packaging().join("Assets");
    let mut expected = BTreeSet::new();
    for name in logo_names(&elements) {
        let (width, height) = base_size(&name);
        for scale in SCALES {
            let file = format!("{name}.scale-{scale}.png");
            let want = (scaled(width, scale), scaled(height, scale));
            assert_eq!(png_size(&assets.join(&file)), want, "{file}");
            expected.insert(file);
        }
    }
    for size in TARGET_SIZES {
        for form in ALT_FORMS {
            let file = format!("Square44x44Logo.targetsize-{size}{form}.png");
            assert_eq!(png_size(&assets.join(&file)), (size, size), "{file}");
            expected.insert(file);
        }
    }
    // A stray file would go into the package and into its index.
    let found: BTreeSet<String> = std::fs::read_dir(&assets)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(found, expected);
}

fn scaled(base: u32, scale: u32) -> u32 {
    (base * scale + 50) / 100
}

#[test]
fn the_exe_icon_holds_the_sizes_windows_asks_for() {
    let bytes = std::fs::read(packaging().join("QuickEdit.ico")).unwrap();
    assert_eq!(&bytes[..4], &[0, 0, 1, 0]);
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    let mut sizes = Vec::new();
    for entry in bytes[6..6 + 16 * count].chunks(16) {
        let side = if entry[0] == 0 {
            256
        } else {
            u32::from(entry[0])
        };
        let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
        let length = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        assert!(offset + length <= bytes.len());
        assert_eq!(&bytes[offset..offset + 4], b"\x89PNG");
        let number = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!((number(offset + 16), number(offset + 20)), (side, side));
        sizes.push(side);
    }
    for required in [16, 24, 32, 48, 256] {
        assert!(sizes.contains(&required), "{required}");
    }
}

#[test]
fn the_exe_manifest_keeps_the_system_code_page_and_asks_for_the_rest() {
    let text = std::fs::read_to_string(packaging().join("QuickEdit.exe.manifest")).unwrap();
    parse_xml(&text).unwrap();
    let elements = parse_xml(&text).unwrap();
    // qe-codec reads the system code page as a step of Automatic. Setting this would change
    // how old files decode.
    assert!(
        elements
            .iter()
            .all(|element| element.name != "activeCodePage")
    );
    assert_eq!(find(&elements, "dpiAwareness").text, "PerMonitorV2");
    assert_eq!(find(&elements, "longPathAware").text, "true");
    assert_eq!(
        find(&elements, "supportedOS").attr("Id"),
        Some("{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}")
    );
    assert_eq!(
        find(&elements, "requestedExecutionLevel").attr("level"),
        Some("asInvoker")
    );
}

fn sdk_tool(name: &str) -> Option<PathBuf> {
    let bin = PathBuf::from(std::env::var_os("ProgramFiles(x86)")?).join("Windows Kits/10/bin");
    let mut found: Vec<(Vec<u32>, PathBuf)> = std::fs::read_dir(bin)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let folder = entry.file_name().into_string().ok()?;
            let version = folder
                .split('.')
                .map(|part| part.parse().ok())
                .collect::<Option<Vec<u32>>>()?;
            let file = entry.path().join("x64").join(name);
            file.is_file().then_some((version, file))
        })
        .collect();
    found.sort();
    found.pop().map(|(_, file)| file)
}

/// Whether the exe holds this text as bytes, as an embedded manifest does.
fn holds(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|part| part == needle)
}

#[test]
fn the_built_exe_holds_the_manifest_and_icon_and_still_starts() {
    let exe = env!("CARGO_BIN_EXE_QuickEdit");
    if sdk_tool("rc.exe").is_none() && std::env::var_os("QE_RC").is_none() {
        println!("skipped the resource check: rc.exe is missing, so this debug exe has none");
    } else {
        let bytes = std::fs::read(exe).unwrap();
        assert!(holds(&bytes, b"PerMonitorV2"), "no manifest in the exe");
        assert!(holds(&bytes, b"longPathAware"), "no manifest in the exe");
        assert!(!holds(&bytes, b"activeCodePage"));
        // The PNG header of the 256 px entry, inside an RT_ICON resource.
        assert!(holds(&bytes, b"\x89PNG\r\n\x1a\n"), "no icon in the exe");
    }
    // With the manifest, the process starts as per-monitor v2 before qe-shell's call to set it.
    let folder = temp_folder("smoke");
    let output = Command::new(exe)
        .arg("--hidden-smoke")
        .arg("--data-root")
        .arg(folder.join("data"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&folder);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("hidden smoke passed"));
}

// ---- What the built exe holds ----

/// The parts of a PE file these tests read.
struct Pe {
    bytes: Vec<u8>,
    /// (virtual address, size in the file's image, offset in the file)
    sections: Vec<(u32, u32, u32)>,
    data_directories: usize,
}

impl Pe {
    fn read(path: &Path) -> Self {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let u16_at = |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let pe = u32_at(0x3c) as usize;
        assert_eq!(&bytes[pe..pe + 4], b"PE\0\0", "{}", path.display());
        let count = u16_at(pe + 6) as usize;
        let optional = pe + 24;
        assert_eq!(u16_at(optional), 0x20b, "a 64-bit exe");
        let table = optional + u16_at(pe + 20) as usize;
        let sections = (0..count)
            .map(|index| {
                let at = table + 40 * index;
                let size = u32_at(at + 8).max(u32_at(at + 16));
                (u32_at(at + 12), size, u32_at(at + 20))
            })
            .collect();
        Self {
            data_directories: optional + 112,
            bytes,
            sections,
        }
    }
    fn u16_at(&self, at: usize) -> u16 {
        u16::from_le_bytes(self.bytes[at..at + 2].try_into().unwrap())
    }
    fn u32_at(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap())
    }
    /// The file offset of an address in the loaded image.
    fn offset(&self, address: u32) -> usize {
        let (start, _, raw) = self
            .sections
            .iter()
            .find(|(start, size, _)| address >= *start && address < start + size)
            .unwrap_or_else(|| panic!("the address {address:#x} is in no section"));
        (address - start + raw) as usize
    }
    /// The address of one data directory, or `None` if the exe has none.
    fn directory(&self, index: usize) -> Option<u32> {
        let address = self.u32_at(self.data_directories + 8 * index);
        (address != 0).then_some(address)
    }
    fn name_at(&self, address: u32) -> String {
        let at = self.offset(address);
        let end = at + self.bytes[at..].iter().position(|b| *b == 0).unwrap();
        String::from_utf8_lossy(&self.bytes[at..end]).into_owned()
    }
    /// Every DLL in the import table, normal and delay-loaded.
    fn imports(&self) -> Vec<String> {
        let mut names = Vec::new();
        // The name's place is 12 bytes into a normal entry of 20, and 4 into a delay entry of 32.
        for (directory, name_field, step) in [(1, 12, 20), (13, 4, 32)] {
            let Some(address) = self.directory(directory) else {
                continue;
            };
            let mut at = self.offset(address);
            loop {
                let name = self.u32_at(at + name_field);
                if name == 0 {
                    break;
                }
                names.push(self.name_at(name));
                at += step;
            }
        }
        assert!(!names.is_empty(), "no imports were read");
        names
    }
    /// `DependentLoadFlags`, 78 bytes into a 64-bit load configuration.
    fn dependent_load_flags(&self) -> u16 {
        let address = self
            .directory(10)
            .expect("the exe has a load configuration");
        self.u16_at(self.offset(address) + 78)
    }
    /// (file version, product version) from `VS_FIXEDFILEINFO`, whose first word is its mark.
    fn fixed_versions(&self) -> Option<([u16; 4], [u16; 4])> {
        let mark = 0xFEEF_04BDu32.to_le_bytes();
        let at = self.bytes.windows(4).position(|part| part == mark)?;
        let read = |at: usize| {
            let (ms, ls) = (self.u32_at(at), self.u32_at(at + 4));
            [(ms >> 16) as u16, ms as u16, (ls >> 16) as u16, ls as u16]
        };
        Some((read(at + 8), read(at + 16)))
    }
}

fn debug_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_QuickEdit"))
}

/// The release exe beside this build, or the one `QE_RELEASE_EXE` names. Tests that need it say
/// so and pass when there is none, and `packaging/build.ps1` makes the same checks on its own.
fn release_exe() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("QE_RELEASE_EXE") {
        return Some(PathBuf::from(named));
    }
    let debug = debug_exe();
    let beside = debug
        .parent()?
        .parent()?
        .join("release")
        .join("QuickEdit.exe");
    if beside.is_file() {
        return Some(beside);
    }
    println!(
        "skipped for the release exe: there is none at {}",
        beside.display()
    );
    None
}

fn check_no_c_runtime_import(exe: &Path) {
    let imports = Pe::read(exe).imports();
    println!("{} imports {imports:?}", exe.display());
    for name in &imports {
        let lower = name.to_ascii_lowercase();
        assert!(
            !lower.starts_with("vcruntime")
                && !lower.starts_with("msvcp")
                && !lower.starts_with("api-ms-win-crt-")
                && !lower.starts_with("ucrtbase"),
            "{} needs {name}, which a clean PC may not have",
            exe.display()
        );
    }
}

/// With no C runtime in the package, an import of one fails before any Rust code runs.
#[test]
fn the_exe_imports_no_c_runtime() {
    check_no_c_runtime_import(&debug_exe());
    if let Some(exe) = release_exe() {
        check_no_c_runtime_import(&exe);
    }
}

fn find_case_blind(haystack: &[u8], needle: &str) -> Option<usize> {
    let needle: Vec<u8> = needle.bytes().map(|b| b.to_ascii_lowercase()).collect();
    haystack.windows(needle.len()).position(|part| {
        part.iter()
            .map(u8::to_ascii_lowercase)
            .eq(needle.iter().copied())
    })
}

/// The folder an exe was built in holds the builder's account name. Store customers get the exe.
/// A release exe from plain `cargo build --release` fails here: `packaging/build.ps1` adds the
/// `--remap-path-prefix` flags that this needs, as they differ from one PC to the next.
#[test]
fn the_release_exe_names_no_user_folder() {
    let Some(exe) = release_exe() else {
        return;
    };
    let bytes = std::fs::read(&exe).unwrap();
    for needle in [r"\Users\", "/Users/", ".cargo", ".rustup"] {
        if let Some(at) = find_case_blind(&bytes, needle) {
            let end = (at + 80).min(bytes.len());
            panic!(
                "{} holds {needle} at {at}: {}. Build it with packaging\\build.ps1.",
                exe.display(),
                String::from_utf8_lossy(&bytes[at..end])
            );
        }
        let wide: Vec<u8> = needle.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert!(
            !holds(&bytes, &wide),
            "{} holds {needle} in UTF-16",
            exe.display()
        );
    }
}

/// 0x800 is `LOAD_LIBRARY_SEARCH_SYSTEM32`: a planted DLL beside the exe must not load.
#[test]
fn the_exe_loads_its_imports_only_from_system32() {
    assert_eq!(Pe::read(&debug_exe()).dependent_load_flags(), 0x800);
    if let Some(exe) = release_exe() {
        assert_eq!(Pe::read(&exe).dependent_load_flags(), 0x800);
    }
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[test]
fn the_exe_version_record_matches_the_package_manifest() {
    if sdk_tool("rc.exe").is_none() && std::env::var_os("QE_RC").is_none() {
        println!("skipped: rc.exe is missing, so this debug exe has no resources");
        return;
    }
    let elements = read_manifest();
    let text = find(&elements, "Identity")
        .attr("Version")
        .unwrap()
        .to_string();
    let parts: Vec<u16> = text.split('.').map(|n| n.parse().unwrap()).collect();
    let want: [u16; 4] = parts.try_into().unwrap();
    let mut exes = vec![debug_exe()];
    exes.extend(release_exe());
    for exe in exes {
        let pe = Pe::read(&exe);
        let found = pe.fixed_versions().expect("a version record");
        assert_eq!(found, (want, want), "{}", exe.display());
        for field in [
            "ProductName",
            "FileDescription",
            "QuickEdit plain-text editor",
        ] {
            assert!(holds(&pe.bytes, &utf16(field)), "{field}");
        }
        assert!(holds(&pe.bytes, &utf16(&text)), "{text}");
    }
}

/// Test words must not read as test words to a customer who opens the exe in a text viewer.
#[test]
fn no_test_message_ships_in_the_exe() {
    let mut exes = vec![debug_exe()];
    exes.extend(release_exe());
    for exe in exes {
        let bytes = std::fs::read(&exe).unwrap();
        for text in [
            "A test made this raise fail.",
            "this test asked for a window that fails",
        ] {
            assert!(
                !holds(&bytes, text.as_bytes()),
                "{text} in {}",
                exe.display()
            );
        }
    }
}

// ---- The words that go with the package ----

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn the_listing_names_no_other_product_and_makes_no_network_claim_it_cannot_keep() {
    let listing = std::fs::read_to_string(packaging().join("listing.md")).unwrap();
    assert!(!listing.to_lowercase().contains("notepad"), "policy 10.1.3");
    assert!(!listing.contains("does not use the network"));
    assert!(listing.contains("has no network code and contacts no server"));
    assert!(listing.contains("network share"));
    assert!(listing.to_lowercase().contains("dictionary"));
    assert!(!listing.contains("does not access"));
}

#[test]
fn the_privacy_policy_says_what_the_app_does_and_the_readme_says_it_must_be_published() {
    let policy = std::fs::read_to_string(packaging().join("PRIVACY.md")).unwrap();
    for needed in [
        "no network code",
        "contacts no server",
        "dictionary",
        "autosave",
        r"%LOCALAPPDATA%\QuickEdit",
        r"%LOCALAPPDATA%\Packages",
    ] {
        assert!(policy.contains(needed), "{needed}");
    }
    let readme = std::fs::read_to_string(packaging().join("README.md")).unwrap();
    assert!(readme.contains("10.5.1"));
    assert!(
        readme.contains("You must publish `PRIVACY.md` at a web address"),
        "the person's step"
    );
    assert!(readme.contains("PRIVACY.md"));
    assert!(!readme.contains("does not access, collect or send"));
}

/// `LICENSE` names MIT, the build script copies the notices into the package, and the notices
/// file is what its own command makes from `Cargo.lock` now.
#[test]
fn the_licence_and_notices_are_there_and_the_notices_are_current() {
    let license = std::fs::read_to_string(repo_root().join("LICENSE")).unwrap();
    assert!(license.starts_with("MIT License"), "{license:.40}");
    let notices = std::fs::read_to_string(packaging().join("NOTICES.txt")).unwrap();
    for name in ["windows ", "windows-core ", "unicode-segmentation "] {
        assert!(notices.contains(name), "{name}");
    }
    assert!(notices.contains("not legal advice"));
    let script = std::fs::read_to_string(packaging().join("build.ps1")).unwrap();
    assert!(script.contains("NOTICES.txt") && script.contains("'LICENSE'"));
    let Some(powershell) = find_on_path("pwsh.exe") else {
        println!("skipped the notices compare: pwsh.exe is not on PATH");
        return;
    };
    let output = Command::new(powershell)
        .args(["-NoProfile", "-File"])
        .arg(packaging().join("make-notices.ps1"))
        .arg("-Check")
        .creation_flags(NO_WINDOW)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|folder| folder.join(name))
        .find(|file| file.is_file())
}

/// A public repo holds no account names, machine names or the names of the reviewers. The state
/// note is left out here because another file owner updates it. The git history still has these.
#[test]
fn tracked_text_holds_no_account_machine_or_reviewer_names() {
    // Written in two parts, so this test does not find itself.
    let names = [
        concat!("yj", "li"),
        concat!("alpha", "-wolf"),
        concat!("rog", "-wolf"),
        concat!("tail", "scale"),
        concat!("\\bAs", "tra\\b"),
        concat!("\\bOp", "us\\b"),
        concat!("\\bFa", "ble\\b"),
        concat!("\\bS", "ol\\b"),
        concat!("remote_", "machines"),
        concat!("quickedit_", "windows"),
    ];
    let mut command = Command::new("git");
    command
        .current_dir(repo_root())
        .args(["grep", "-n", "-I", "-E", "-e"])
        .arg(names.join("|"))
        .args(["--", "."])
        .creation_flags(NO_WINDOW);
    let Ok(output) = command.output() else {
        println!("skipped: git is not on PATH");
        return;
    };
    // git grep exits 1 for no match, 0 for a match, and more for a failure.
    if output.status.code() == Some(128) {
        println!("skipped: this copy is not a git checkout");
        return;
    }
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn temp_folder(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("qe-package")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn run(tool: &Path, args: &[&std::ffi::OsStr]) {
    let output = Command::new(tool)
        .args(args)
        .creation_flags(NO_WINDOW)
        .output()
        .unwrap_or_else(|e| panic!("{}: {e}", tool.display()));
    assert!(
        output.status.success(),
        "{}: {}{}",
        tool.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `makepri dump` writes UTF-8 or UTF-16 by SDK version.
fn dump_text(bytes: &[u8]) -> String {
    if let [0xff, 0xfe, rest @ ..] = bytes {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn copy_folder(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

#[test]
fn makeappx_packs_the_real_manifest_and_icons_around_a_stand_in_exe() {
    let (Some(makepri), Some(makeappx)) = (sdk_tool("makepri.exe"), sdk_tool("makeappx.exe"))
    else {
        println!("skipped: makepri.exe or makeappx.exe is not in the Windows SDK here");
        return;
    };
    let folder = temp_folder("pack");
    let layout = folder.join("layout");
    copy_folder(&packaging().join("Assets"), &layout.join("Assets"));
    std::fs::copy(
        packaging().join("AppxManifest.xml"),
        layout.join("AppxManifest.xml"),
    )
    .unwrap();
    std::fs::write(layout.join("QuickEdit.exe"), b"MZ stand-in, never run").unwrap();
    std::fs::copy(packaging().join("NOTICES.txt"), layout.join("NOTICES.txt")).unwrap();
    std::fs::copy(repo_root().join("LICENSE"), layout.join("LICENSE")).unwrap();
    let os = |text: &'static str| std::ffi::OsStr::new(text);
    let config = folder.join("priconfig.xml");
    let manifest = layout.join("AppxManifest.xml");
    let pri = layout.join("resources.pri");
    let package = folder.join("test.msix");
    run(
        &makepri,
        &[
            os("createconfig"),
            os("/cf"),
            config.as_os_str(),
            os("/dq"),
            os("en-US"),
            os("/pv"),
            os("10.0.0"),
            os("/o"),
        ],
    );
    let text = std::fs::read_to_string(&config).unwrap();
    let start = text
        .find("<packaging>")
        .expect("makepri's <packaging> part");
    let end = text.find("</packaging>").expect("its end") + "</packaging>".len();
    std::fs::write(&config, format!("{}{}", &text[..start], &text[end..])).unwrap();
    run(
        &makepri,
        &[
            os("new"),
            os("/pr"),
            layout.as_os_str(),
            os("/cf"),
            config.as_os_str(),
            os("/mn"),
            manifest.as_os_str(),
            os("/of"),
            pri.as_os_str(),
            os("/o"),
        ],
    );
    run(
        &makeappx,
        &[
            os("pack"),
            os("/o"),
            os("/d"),
            layout.as_os_str(),
            os("/p"),
            package.as_os_str(),
        ],
    );
    let unpacked = folder.join("unpacked");
    run(
        &makeappx,
        &[
            os("unpack"),
            os("/o"),
            os("/p"),
            package.as_os_str(),
            os("/d"),
            unpacked.as_os_str(),
        ],
    );
    for file in [
        "AppxManifest.xml",
        "AppxBlockMap.xml",
        "resources.pri",
        "QuickEdit.exe",
        "NOTICES.txt",
        "LICENSE",
        "Assets/Square44x44Logo.targetsize-16_altform-unplated.png",
    ] {
        assert!(unpacked.join(file).is_file(), "{file}");
    }
    // One flat package needs one index that names every file. A split index is for a bundle.
    let indexes: Vec<String> = std::fs::read_dir(&unpacked)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".pri"))
        .collect();
    assert_eq!(indexes, ["resources.pri"]);
    let dump = folder.join("dump.xml");
    run(
        &makepri,
        &[
            os("dump"),
            os("/if"),
            pri.as_os_str(),
            os("/of"),
            dump.as_os_str(),
            os("/dt"),
            os("Detailed"),
            os("/o"),
        ],
    );
    let dump = dump_text(&std::fs::read(&dump).unwrap());
    for file in [
        "Square44x44Logo.scale-125.png",
        "Square44x44Logo.targetsize-16_altform-unplated.png",
        "Square44x44Logo.targetsize-16_altform-lightunplated.png",
        "StoreLogo.scale-400.png",
        "Wide310x150Logo.scale-150.png",
    ] {
        assert!(dump.contains(file), "the index does not name {file}");
    }
    let script = std::fs::read_to_string(packaging().join("build.ps1")).unwrap();
    assert!(
        script.contains("<packaging>"),
        "build.ps1 must flatten the index too"
    );
    let _ = std::fs::remove_dir_all(&folder);
}
