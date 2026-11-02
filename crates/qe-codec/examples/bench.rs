//! Release bench: speed and peak memory of `decode` and `encode` on non-ASCII text.
//!
//! Run with `cargo run --release -p qe-codec --locked --example bench`, or add a
//! size in MiB (default 512). A process's peak memory never falls, so each case
//! runs in its own child process. The fixtures are hand-written byte patterns;
//! they do not come from the crate's own `encode`.
use qe_api::{EncodingChoice, TextEncoding};
use qe_codec::{decode, decode_file, encode};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::time::Instant;

#[repr(C)]
#[derive(Default)]
struct MemoryCounters {
    size: u32,
    page_fault_count: u32,
    peak_working_set: usize,
    working_set: usize,
    quota_peak_paged: usize,
    quota_paged: usize,
    quota_peak_non_paged: usize,
    quota_non_paged: usize,
    pagefile: usize,
    peak_pagefile: usize,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut core::ffi::c_void;
    fn K32GetProcessMemoryInfo(
        process: *mut core::ffi::c_void,
        counters: *mut MemoryCounters,
        size: u32,
    ) -> i32;
}
/// Peak private memory and peak working set of this process, in bytes.
fn peaks() -> (usize, usize) {
    let mut m = MemoryCounters {
        size: size_of::<MemoryCounters>() as u32,
        ..Default::default()
    };
    // SAFETY: `m` is a live struct of the size the call is told.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut m, m.size) };
    assert_ne!(ok, 0, "K32GetProcessMemoryInfo failed");
    (m.peak_pagefile, m.peak_working_set)
}

/// The file's shape. `decode_file` sets its memory from the first 1 MiB, so a file
/// whose start is unlike the rest is its hardest case.
#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Uniform,
    AsciiThenPattern,
    PatternThenAscii,
}
struct Case {
    label: &'static str,
    encoding: TextEncoding,
    head: &'static [u8],
    pattern: &'static [u8],
    shape: Shape,
    /// Automatic in place of the chosen encoding, and `decode_file` only.
    automatic: bool,
    file_only: bool,
}
const UNIFORM: Case = Case {
    label: "",
    encoding: TextEncoding::Utf8,
    head: &[],
    pattern: &[],
    shape: Shape::Uniform,
    automatic: false,
    file_only: false,
};
const CASES: &[Case] = &[
    // 漢字é CR LF
    Case {
        label: "UTF-8 (CJK mix)",
        encoding: TextEncoding::Utf8,
        head: &[],
        pattern: &[0xE6, 0xBC, 0xA2, 0xE5, 0xAD, 0x97, 0xC3, 0xA9, 0x0D, 0x0A],
        ..UNIFORM
    },
    // Same text, UTF-16 LE, with a BOM.
    Case {
        label: "UTF-16 (CJK mix)",
        encoding: TextEncoding::Utf16,
        head: &[0xFF, 0xFE],
        pattern: &[0x22, 0x6F, 0x57, 0x5B, 0xE9, 0x00, 0x0D, 0x00, 0x0A, 0x00],
        ..UNIFORM
    },
    // éàfox CR LF
    Case {
        label: "Windows-1252",
        encoding: TextEncoding::Windows1252,
        head: &[],
        pattern: &[0xE9, 0xE0, 0x66, 0x6F, 0x78, 0x0D, 0x0A],
        ..UNIFORM
    },
    // 漢 only: text is 1.5 times the file, the worst common case.
    Case {
        label: "Shift JIS (only 漢)",
        encoding: TextEncoding::ShiftJis,
        head: &[],
        pattern: &[0x8A, 0xBF],
        ..UNIFORM
    },
    // 漢あab CR LF: a more usual Japanese document.
    Case {
        label: "Shift JIS (mixed)",
        encoding: TextEncoding::ShiftJis,
        head: &[],
        pattern: &[0x8A, 0xBF, 0x82, 0xA0, 0x61, 0x62, 0x0D, 0x0A],
        ..UNIFORM
    },
    Case {
        label: "UTF-8, Automatic",
        encoding: TextEncoding::Utf8,
        pattern: &[0xE6, 0xBC, 0xA2, 0xE5, 0xAD, 0x97, 0xC3, 0xA9, 0x0D, 0x0A],
        automatic: true,
        file_only: true,
        ..UNIFORM
    },
    // Thai: 1 byte becomes 3, the largest growth of any code page here.
    Case {
        label: "Windows-874 (Thai)",
        encoding: TextEncoding::Windows874,
        pattern: &[0xA1, 0xA2, 0xA3, 0xA4, 0xA5],
        file_only: true,
        ..UNIFORM
    },
    Case {
        label: "SJIS 8 MiB a, then 漢",
        encoding: TextEncoding::ShiftJis,
        pattern: &[0x8A, 0xBF],
        shape: Shape::AsciiThenPattern,
        file_only: true,
        ..UNIFORM
    },
    Case {
        label: "SJIS 8 MiB 漢, then a",
        encoding: TextEncoding::ShiftJis,
        pattern: &[0x8A, 0xBF],
        shape: Shape::PatternThenAscii,
        file_only: true,
        ..UNIFORM
    },
];
const MIB: usize = 1024 * 1024;

fn fixture(case: &Case, mib: usize) -> Vec<u8> {
    let size = case.head.len() + (mib * MIB / case.pattern.len()) * case.pattern.len();
    let mut bytes = Vec::with_capacity(size);
    bytes.extend_from_slice(case.head);
    let first = 8 * MIB / case.pattern.len() * case.pattern.len();
    match case.shape {
        Shape::Uniform => {}
        Shape::AsciiThenPattern => bytes.resize(first, b'a'),
        Shape::PatternThenAscii => {
            while bytes.len() < first {
                bytes.extend_from_slice(case.pattern);
            }
            bytes.resize(size, b'a');
        }
    }
    while bytes.len() < size {
        bytes.extend_from_slice(case.pattern);
    }
    bytes
}
/// The file route: no bytes of the file are ever held, so only the text counts.
fn child_file(case: &Case, path: &str) {
    let size = std::fs::metadata(path).expect("fixture file").len() as f64;
    let start = Instant::now();
    let choice = if case.automatic {
        EncodingChoice::Automatic
    } else {
        EncodingChoice::Specific(case.encoding)
    };
    let decoded = decode_file(path.as_ref(), choice, |_, _| {}, || false).expect("decode_file");
    let seconds = start.elapsed().as_secs_f64();
    let (private, set) = peaks();
    assert!(!decoded.format.lossy, "{}: lossy", case.label);
    println!(
        "{:<22} {:>5} MiB | decode_file {:>7.1} MB/s, peak {:>5.2} GiB private ({:.2} x file), {:>5.2} GiB working set",
        case.label,
        size as usize / MIB,
        size / 1_000_000.0 / seconds,
        private as f64 / (1024 * MIB) as f64,
        private as f64 / size,
        set as f64 / (1024 * MIB) as f64,
    );
}
fn child(case: &Case, mib: usize) {
    let bytes = fixture(case, mib);
    let size = bytes.len();
    let start = Instant::now();
    let decoded = decode(bytes, EncodingChoice::Specific(case.encoding)).expect("decode");
    let decode_s = start.elapsed().as_secs_f64();
    let (decode_private, decode_set) = peaks();
    assert!(!decoded.format.lossy, "{}: lossy", case.label);
    let start = Instant::now();
    let encoded = encode(&decoded.text, &decoded.format).expect("encode");
    let encode_s = start.elapsed().as_secs_f64();
    let (end_private, _) = peaks();
    assert_eq!(encoded.len(), size, "{}: size changed", case.label);
    assert_eq!(&encoded[..case.head.len()], case.head);
    assert!(
        encoded[case.head.len()..]
            .chunks(case.pattern.len())
            .all(|c| c == case.pattern),
        "{}: bytes changed",
        case.label
    );
    let mb = size as f64 / 1_000_000.0;
    let times = size as f64;
    println!(
        "{:<22} {:>5} MiB | decode {:>7.1} MB/s, peak {:>5.2} GiB private ({:.2} x file), {:>5.2} GiB working set | encode {:>7.1} MB/s | peak after encode {:>5.2} GiB ({:.2} x file)",
        case.label,
        size / MIB,
        mb / decode_s,
        decode_private as f64 / (1024 * MIB) as f64,
        decode_private as f64 / times,
        decode_set as f64 / (1024 * MIB) as f64,
        mb / encode_s,
        end_private as f64 / (1024 * MIB) as f64,
        end_private as f64 / times,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("child") {
        let index: usize = args[1].parse().expect("case index");
        let mib: usize = args[2].parse().expect("size");
        child(&CASES[index], mib);
        return;
    }
    if args.first().map(String::as_str) == Some("child-file") {
        let index: usize = args[1].parse().expect("case index");
        child_file(&CASES[index], &args[2]);
        return;
    }
    let mib = args
        .first()
        .map_or(512, |a| a.parse().expect("size in MiB"));
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe().expect("own path");
    for (index, case) in CASES.iter().enumerate() {
        let run = |args: &[&str]| {
            let output = Command::new(&exe)
                .args(args)
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .expect("start child");
            print!("{}", String::from_utf8_lossy(&output.stdout));
            assert!(
                output.status.success(),
                "case {}: {}",
                case.label,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let number = index.to_string();
        if !case.file_only {
            run(&["child", &number, &mib.to_string()]);
        }
        let path = std::env::temp_dir().join(format!("qe-bench-{}.bin", std::process::id()));
        std::fs::write(&path, fixture(case, mib)).expect("write fixture file");
        run(&["child-file", &number, path.to_str().expect("plain path")]);
        std::fs::remove_file(&path).expect("remove fixture file");
    }
}
