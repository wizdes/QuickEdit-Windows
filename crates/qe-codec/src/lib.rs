//! Plain-text byte conversion and safe file writes for QuickEdit.
use qe_api::{EncodingChoice, TextEncoding};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetACP() -> u32;
    fn IsValidCodePage(code_page: u32) -> i32;
    fn MultiByteToWideChar(
        code_page: u32,
        flags: u32,
        input: *const u8,
        length: i32,
        output: *mut u16,
        capacity: i32,
    ) -> i32;
    fn WideCharToMultiByte(
        code_page: u32,
        flags: u32,
        input: *const u16,
        length: i32,
        output: *mut u8,
        capacity: i32,
        default_char: *const u8,
        used_default: *mut i32,
    ) -> i32;
    fn ReplaceFileW(
        replaced: *const u16,
        replacement: *const u16,
        backup: *const u16,
        flags: u32,
        exclude: *const u8,
        reserved: *const u8,
    ) -> i32;
}
const MB_ERR_INVALID_CHARS: u32 = 8;
const WC_ERR_INVALID_CHARS: u32 = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Format {
    pub encoding: TextEncoding,
    pub has_bom: bool,
    pub endian: Option<Endian>,
    pub main_line_ending: LineEnding,
    pub lossy: bool,
}
impl Format {
    pub fn new_document() -> Self {
        Self {
            encoding: TextEncoding::Utf8,
            has_bom: false,
            endian: None,
            main_line_ending: LineEnding::Lf,
            lossy: false,
        }
    }
    pub fn changed_to(&self, encoding: TextEncoding) -> Self {
        if encoding == self.encoding {
            return self.clone();
        }
        Self {
            encoding,
            has_bom: matches!(encoding, TextEncoding::Utf16 | TextEncoding::Utf32),
            endian: unicode_endian(encoding),
            main_line_ending: self.main_line_ending,
            lossy: false,
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct Decoded {
    pub text: String,
    pub format: Format,
}
#[derive(Debug)]
pub enum CodecError {
    InapplicableEncoding(TextEncoding),
    Unencodable(TextEncoding),
    UnavailableEncoding(TextEncoding),
    UnsupportedSystemCodePage(u32),
    ReadOnly(PathBuf),
    RecoveryRequired { original: PathBuf, backup: PathBuf },
    Io(std::io::Error),
    Cancelled,
}
impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InapplicableEncoding(e) => {
                write!(f, "could not be opened using text encoding {}", e.name())
            }
            Self::Unencodable(e) => write!(f, "text cannot be saved using {}", e.name()),
            Self::UnavailableEncoding(e) => write!(
                f,
                "text encoding {} is not available on this Windows system",
                e.name()
            ),
            Self::UnsupportedSystemCodePage(cp) => {
                write!(f, "Windows system code page {cp} has no TextEncoding entry")
            }
            Self::ReadOnly(p) => write!(f, "read-only target: {}", p.display()),
            Self::RecoveryRequired { original, backup } => write!(
                f,
                "could not restore {} after replace failure; original bytes are at {}",
                original.display(),
                backup.display()
            ),
            Self::Io(e) => e.fmt(f),
            Self::Cancelled => write!(f, "operation cancelled"),
        }
    }
}
impl std::error::Error for CodecError {}
impl From<std::io::Error> for CodecError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

fn unicode_endian(e: TextEncoding) -> Option<Endian> {
    match e {
        TextEncoding::Utf16
        | TextEncoding::Utf32
        | TextEncoding::Utf16Le
        | TextEncoding::Utf32Le => Some(Endian::Little),
        TextEncoding::Utf16Be | TextEncoding::Utf32Be => Some(Endian::Big),
        _ => None,
    }
}
fn code_page(e: TextEncoding) -> Option<u32> {
    use TextEncoding::*;
    Some(match e {
        Windows1250 => 1250,
        Windows1251 => 1251,
        Windows1252 => 1252,
        Windows1253 => 1253,
        Windows1254 => 1254,
        Windows1255 => 1255,
        Windows1256 => 1256,
        Windows1257 => 1257,
        Windows1258 => 1258,
        ShiftJis => 932,
        Gbk => 936,
        Gb18030 => 54936,
        Big5 => 950,
        Korean949 => 949,
        EucJp => 20932,
        Iso8859_1 => 28591,
        Iso8859_2 => 28592,
        Iso8859_3 => 28593,
        Iso8859_4 => 28594,
        Iso8859_5 => 28595,
        Iso8859_6 => 28596,
        Iso8859_7 => 28597,
        Iso8859_8 => 28598,
        Iso8859_9 => 28599,
        Iso8859_10 => 28600,
        Iso8859_11 => 28601,
        Iso8859_13 => 28603,
        Iso8859_14 => 28604,
        Iso8859_15 => 28605,
        Iso8859_16 => 28606,
        Koi8R => 20866,
        MacRoman => 10000,
        MacJapanese => 10001,
        MacChineseTraditional => 10002,
        MacKorean => 10003,
        MacChineseSimplified => 10008,
        _ => return None,
    })
}
pub fn is_available(e: TextEncoding) -> bool {
    code_page(e).is_none_or(|cp| unsafe { IsValidCodePage(cp) != 0 })
}
fn cp_decode(bytes: &[u8], e: TextEncoding) -> Result<String, CodecError> {
    let cp = code_page(e).ok_or(CodecError::UnavailableEncoding(e))?;
    if !is_available(e) {
        return Err(CodecError::UnavailableEncoding(e));
    }
    if bytes.is_empty() {
        return Ok(String::new());
    }
    let len = i32::try_from(bytes.len()).map_err(|_| CodecError::InapplicableEncoding(e))?;
    // Windows supports the strict flag for UTF-8 and GB18030 only. For other pages,
    // a byte round trip detects repairs and duplicate mappings.
    let flags = if cp == 54936 { MB_ERR_INVALID_CHARS } else { 0 };
    let count =
        unsafe { MultiByteToWideChar(cp, flags, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
    if count == 0 {
        return Err(CodecError::InapplicableEncoding(e));
    }
    let mut wide = vec![0u16; count as usize];
    if unsafe { MultiByteToWideChar(cp, flags, bytes.as_ptr(), len, wide.as_mut_ptr(), count) } == 0
    {
        return Err(CodecError::InapplicableEncoding(e));
    }
    Ok(String::from_utf16_lossy(&wide))
}
fn cp_encode(text: &str, e: TextEncoding) -> Result<Vec<u8>, CodecError> {
    let cp = code_page(e).ok_or(CodecError::UnavailableEncoding(e))?;
    if !is_available(e) {
        return Err(CodecError::UnavailableEncoding(e));
    }
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    let len = i32::try_from(wide.len()).map_err(|_| CodecError::Unencodable(e))?;
    let flags = if cp == 54936 { WC_ERR_INVALID_CHARS } else { 0 };
    let mut used = 0;
    let used_ptr = if cp == 54936 {
        std::ptr::null_mut()
    } else {
        &mut used
    };
    let count = unsafe {
        WideCharToMultiByte(
            cp,
            flags,
            wide.as_ptr(),
            len,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            used_ptr,
        )
    };
    if count == 0 || used != 0 {
        return Err(CodecError::Unencodable(e));
    }
    let mut bytes = vec![0u8; count as usize];
    used = 0;
    if unsafe {
        WideCharToMultiByte(
            cp,
            flags,
            wide.as_ptr(),
            len,
            bytes.as_mut_ptr(),
            count,
            std::ptr::null(),
            used_ptr,
        )
    } == 0
        || used != 0
    {
        return Err(CodecError::Unencodable(e));
    }
    // Windows best-fit mappings can change a character without setting
    // lpUsedDefaultChar (for example é to e in CP1251).
    if cp_decode(&bytes, e)? != text {
        return Err(CodecError::Unencodable(e));
    }
    Ok(bytes)
}
fn main_line_ending(text: &str) -> LineEnding {
    let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
    let mut it = text.bytes().peekable();
    while let Some(b) = it.next() {
        match b {
            b'\r' if it.peek() == Some(&b'\n') => {
                it.next();
                crlf += 1;
            }
            b'\r' => cr += 1,
            b'\n' => lf += 1,
            _ => {}
        }
    }
    if lf >= crlf && lf >= cr {
        LineEnding::Lf
    } else if crlf >= cr {
        LineEnding::CrLf
    } else {
        LineEnding::Cr
    }
}
fn decode_unicode(bytes: &[u8], e: TextEncoding, endian: Endian) -> Result<String, CodecError> {
    if matches!(
        e,
        TextEncoding::Utf16 | TextEncoding::Utf16Le | TextEncoding::Utf16Be
    ) {
        let units = bytes.as_chunks::<2>().0.iter().map(|b| {
            if endian == Endian::Little {
                u16::from_le_bytes(*b)
            } else {
                u16::from_be_bytes(*b)
            }
        });
        Ok(std::char::decode_utf16(units)
            .map(|c| c.unwrap_or('\u{fffd}'))
            .collect())
    } else {
        let mut s = String::new();
        for b in bytes.as_chunks::<4>().0 {
            let n = if endian == Endian::Little {
                u32::from_le_bytes(*b)
            } else {
                u32::from_be_bytes(*b)
            };
            s.push(char::from_u32(n).ok_or(CodecError::InapplicableEncoding(e))?);
        }
        Ok(s)
    }
}
fn valid_utf8_chunks(bytes: &[u8]) -> bool {
    let mut carry = Vec::with_capacity(4);
    for chunk in bytes.chunks(1024 * 1024) {
        let mut body = chunk;
        if !carry.is_empty() {
            let take = (4 - carry.len()).min(body.len());
            carry.extend_from_slice(&body[..take]);
            match std::str::from_utf8(&carry) {
                Ok(_) => {
                    carry.clear();
                    body = &body[take..];
                }
                Err(e) if e.error_len().is_none() => continue,
                Err(_) => return false,
            }
        }
        match std::str::from_utf8(body) {
            Ok(_) => {}
            Err(e) if e.error_len().is_none() => carry.extend_from_slice(&body[e.valid_up_to()..]),
            Err(_) => return false,
        }
    }
    carry.is_empty()
}
pub fn decode(bytes: impl Into<Vec<u8>>, choice: EncodingChoice) -> Result<Decoded, CodecError> {
    let mut bytes = bytes.into();
    let (e, bom, endian) = match choice {
        EncodingChoice::Specific(e) => {
            let (bom, endian) = bom_for_choice(&bytes, e);
            (e, bom, endian)
        }
        EncodingChoice::Automatic => {
            if bytes.starts_with(&[0xff, 0xfe, 0, 0]) {
                (TextEncoding::Utf32, 4, Some(Endian::Little))
            } else if bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
                (TextEncoding::Utf32Be, 4, Some(Endian::Big))
            } else if bytes.starts_with(&[0xff, 0xfe]) {
                (TextEncoding::Utf16, 2, Some(Endian::Little))
            } else if bytes.starts_with(&[0xfe, 0xff]) {
                (TextEncoding::Utf16Be, 2, Some(Endian::Big))
            } else if bytes.starts_with(&[0xef, 0xbb, 0xbf]) && valid_utf8_chunks(&bytes[3..]) {
                (TextEncoding::Utf8, 3, None)
            } else if valid_utf8_chunks(&bytes) {
                (TextEncoding::Utf8, 0, None)
            } else {
                let cp = unsafe { GetACP() };
                let e = TextEncoding::ALL
                    .iter()
                    .copied()
                    .find(|e| code_page(*e) == Some(cp))
                    .ok_or(CodecError::UnsupportedSystemCodePage(cp))?;
                (e, 0, None)
            }
        }
    };
    if e == TextEncoding::Utf8 {
        if bom != 0 {
            bytes.drain(..bom);
        }
        if !valid_utf8_chunks(&bytes) {
            return Err(CodecError::InapplicableEncoding(e));
        }
        // SAFETY: every byte was checked above, including sequences spanning chunks.
        let text = unsafe { String::from_utf8_unchecked(bytes) };
        let main_line_ending = main_line_ending(&text);
        return Ok(Decoded {
            text,
            format: Format {
                encoding: e,
                has_bom: bom != 0,
                endian: None,
                main_line_ending,
                lossy: false,
            },
        });
    }
    let body = &bytes[bom..];
    let text = if let Some(endian) = endian {
        decode_unicode(body, e, endian).map_err(|error| {
            if matches!(choice, EncodingChoice::Automatic) && e == TextEncoding::Utf32Be {
                CodecError::InapplicableEncoding(TextEncoding::Utf32)
            } else {
                error
            }
        })?
    } else {
        cp_decode(body, e)?
    };
    let format = Format {
        encoding: e,
        has_bom: bom != 0,
        endian,
        main_line_ending: main_line_ending(&text),
        lossy: false,
    };
    let roundtrip = encode(&text, &format).ok();
    let lossy = roundtrip.as_deref() != Some(bytes.as_slice());
    Ok(Decoded {
        text,
        format: Format { lossy, ..format },
    })
}
fn bom_for_choice(bytes: &[u8], e: TextEncoding) -> (usize, Option<Endian>) {
    use TextEncoding::*;
    match e {
        Utf8 => (
            usize::from(bytes.starts_with(&[0xef, 0xbb, 0xbf])) * 3,
            None,
        ),
        Utf16 => {
            if bytes.starts_with(&[0xff, 0xfe]) {
                (2, Some(Endian::Little))
            } else if bytes.starts_with(&[0xfe, 0xff]) {
                (2, Some(Endian::Big))
            } else {
                (0, Some(Endian::Big))
            }
        }
        Utf32 => {
            if bytes.starts_with(&[0xff, 0xfe, 0, 0]) {
                (4, Some(Endian::Little))
            } else if bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
                (4, Some(Endian::Big))
            } else {
                (0, Some(Endian::Big))
            }
        }
        Utf16Le => (
            usize::from(bytes.starts_with(&[0xff, 0xfe])) * 2,
            Some(Endian::Little),
        ),
        Utf16Be => (
            usize::from(bytes.starts_with(&[0xfe, 0xff])) * 2,
            Some(Endian::Big),
        ),
        Utf32Le => (
            usize::from(bytes.starts_with(&[0xff, 0xfe, 0, 0])) * 4,
            Some(Endian::Little),
        ),
        Utf32Be => (
            usize::from(bytes.starts_with(&[0, 0, 0xfe, 0xff])) * 4,
            Some(Endian::Big),
        ),
        _ => (0, None),
    }
}
pub fn encode(text: &str, format: &Format) -> Result<Vec<u8>, CodecError> {
    let e = format.encoding;
    if e == TextEncoding::Utf8 {
        let mut out = Vec::with_capacity(text.len() + if format.has_bom { 3 } else { 0 });
        if format.has_bom {
            out.extend_from_slice(&[0xef, 0xbb, 0xbf]);
        }
        out.extend_from_slice(text.as_bytes());
        return Ok(out);
    }
    if let Some(mut endian) = format.endian.or_else(|| unicode_endian(e)) {
        if matches!(e, TextEncoding::Utf16 | TextEncoding::Utf32) && format.has_bom {
            endian = Endian::Little;
        }
        let mut out = Vec::with_capacity(text.len() * 4 + 4);
        let is16 = matches!(
            e,
            TextEncoding::Utf16 | TextEncoding::Utf16Le | TextEncoding::Utf16Be
        );
        let bom = match (is16, endian) {
            (true, Endian::Little) => &[0xff, 0xfe][..],
            (true, Endian::Big) => &[0xfe, 0xff],
            (false, Endian::Little) => &[0xff, 0xfe, 0, 0],
            (false, Endian::Big) => &[0, 0, 0xfe, 0xff],
        };
        if format.has_bom {
            out.extend_from_slice(bom);
        }
        if is16 {
            for u in text.encode_utf16() {
                out.extend_from_slice(&if endian == Endian::Little {
                    u.to_le_bytes()
                } else {
                    u.to_be_bytes()
                });
            }
        } else {
            for c in text.chars() {
                out.extend_from_slice(&if endian == Endian::Little {
                    (c as u32).to_le_bytes()
                } else {
                    (c as u32).to_be_bytes()
                });
            }
        }
        return Ok(out);
    }
    cp_encode(text, e)
}
pub fn can_encode(text: &str, encoding: TextEncoding) -> Result<bool, CodecError> {
    if !is_available(encoding) {
        return Err(CodecError::UnavailableEncoding(encoding));
    }
    let format = Format::new_document().changed_to(encoding);
    match encode(text, &format) {
        Ok(_) => Ok(true),
        Err(CodecError::Unencodable(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

static TEMP_ID: AtomicU64 = AtomicU64::new(0);
fn wide(p: &OsStr) -> Vec<u16> {
    p.encode_wide().chain(Some(0)).collect()
}
#[derive(Debug, PartialEq, Eq)]
pub enum AtomicWriteResult {
    Replaced,
    /// The original was not changed. The temp file contains the requested bytes.
    /// Replacing it by a copy is not atomic and could lose the original on failure.
    ReplacementUnavailable {
        temp_path: PathBuf,
        limit: &'static str,
    },
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<AtomicWriteResult, CodecError> {
    let meta = match fs::metadata(path) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if meta.as_ref().is_some_and(|m| m.permissions().readonly()) {
        return Err(CodecError::ReadOnly(path.to_path_buf()));
    }
    let parent = path.parent().ok_or_else(|| {
        CodecError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no parent",
        ))
    })?;
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".qe-{}-{id}.tmp", std::process::id()));
    let backup = parent.join(format!(".qe-{}-{id}.bak", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    if let Err(e) = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Some(meta) = &meta {
            fs::set_permissions(&temp, meta.permissions())?;
        }
        Ok(())
    })() {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(e.into());
    }
    drop(file);
    if meta.is_none() {
        return match fs::rename(&temp, path) {
            Ok(()) => Ok(AtomicWriteResult::Replaced),
            Err(_) => Ok(AtomicWriteResult::ReplacementUnavailable {
                temp_path: temp,
                limit: "The target was not created; copying the temp file to it would not be atomic.",
            }),
        };
    }
    let target_w = wide(path.as_os_str());
    let temp_w = wide(temp.as_os_str());
    let backup_w = wide(backup.as_os_str());
    if unsafe {
        ReplaceFileW(
            target_w.as_ptr(),
            temp_w.as_ptr(),
            backup_w.as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    } != 0
    {
        let _ = fs::remove_file(backup);
        Ok(AtomicWriteResult::Replaced)
    } else {
        if !path.exists() && backup.exists() && fs::rename(&backup, path).is_err() {
            return Err(CodecError::RecoveryRequired {
                original: path.to_path_buf(),
                backup,
            });
        }
        Ok(AtomicWriteResult::ReplacementUnavailable {
            temp_path: temp,
            limit: "The original remains intact; copying the temp file over it would not be atomic.",
        })
    }
}
pub fn copy_large_original(
    input: &Path,
    dest: &Path,
    mut progress: impl FnMut(u64, u64),
    mut cancel: impl FnMut() -> bool,
) -> Result<(), CodecError> {
    let mut source = File::open(input)?;
    let total = source.metadata()?.len();
    let mut output = OpenOptions::new().write(true).create_new(true).open(dest)?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut done = 0;
    let result = (|| -> Result<(), CodecError> {
        loop {
            if cancel() {
                return Err(CodecError::Cancelled);
            }
            let n = source.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n])?;
            done += n as u64;
            progress(done, total);
        }
        output.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        drop(output);
        let _ = fs::remove_file(dest);
    }
    result
}
fn stream_is_utf8(file: &mut File) -> Result<bool, CodecError> {
    file.seek(SeekFrom::Start(0))?;
    let mut chunk = vec![0u8; 1024 * 1024];
    let mut carry = Vec::with_capacity(4);
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let mut body = &chunk[..n];
        if !carry.is_empty() {
            let take = (4 - carry.len()).min(body.len());
            carry.extend_from_slice(&body[..take]);
            match std::str::from_utf8(&carry) {
                Ok(_) => {
                    carry.clear();
                    body = &body[take..];
                }
                Err(e) if e.error_len().is_none() => continue,
                Err(_) => return Ok(false),
            }
        }
        match std::str::from_utf8(body) {
            Ok(_) => {}
            Err(e) if e.error_len().is_none() => carry.extend_from_slice(&body[e.valid_up_to()..]),
            Err(_) => return Ok(false),
        }
    }
    Ok(carry.is_empty())
}
fn legacy_character_bytes(cp: u32, bytes: &[u8], i: usize) -> usize {
    let a = bytes[i];
    if cp == 54936 && (0x81..=0xfe).contains(&a) {
        if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
            4
        } else {
            2
        }
    } else if cp == 20932 {
        if a == 0x8f {
            3
        } else if a == 0x8e || (0xa1..=0xfe).contains(&a) {
            2
        } else {
            1
        }
    } else if matches!(cp, 932 | 936 | 949 | 950 | 10001 | 10002 | 10003 | 10008) {
        if unsafe { IsDBCSLeadByteEx(cp, a) } != 0 {
            2
        } else {
            1
        }
    } else {
        1
    }
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn IsDBCSLeadByteEx(code_page: u32, byte: u8) -> i32;
}
fn safe_prefix(bytes: &[u8], e: TextEncoding, endian: Option<Endian>) -> usize {
    if endian.is_some() {
        let is16 = matches!(
            e,
            TextEncoding::Utf16 | TextEncoding::Utf16Le | TextEncoding::Utf16Be
        );
        let mut end = bytes.len() / if is16 { 2 } else { 4 } * if is16 { 2 } else { 4 };
        if is16 && end >= 2 {
            let b = [bytes[end - 2], bytes[end - 1]];
            let unit = if endian == Some(Endian::Little) {
                u16::from_le_bytes(b)
            } else {
                u16::from_be_bytes(b)
            };
            if (0xd800..=0xdbff).contains(&unit) {
                end -= 2;
            }
        }
        end
    } else if let Some(cp) = code_page(e) {
        let mut i = 0;
        while i < bytes.len() {
            let width = legacy_character_bytes(cp, bytes, i);
            if i + width > bytes.len() {
                break;
            }
            i += width;
        }
        i
    } else {
        bytes.len()
    }
}
fn add_line_counts(text: &str, counts: &mut [usize; 3], previous_cr: &mut bool) {
    for b in text.bytes() {
        if b == b'\n' {
            if *previous_cr {
                counts[2] -= 1;
                counts[1] += 1;
            } else {
                counts[0] += 1;
            }
        } else if b == b'\r' {
            counts[2] += 1;
        }
        *previous_cr = b == b'\r';
    }
}
pub fn transcode_to_utf8_file(
    input: &Path,
    output: &Path,
    choice: EncodingChoice,
    mut progress: impl FnMut(u64, u64),
    mut cancel: impl FnMut() -> bool,
) -> Result<Format, CodecError> {
    let mut source = File::open(input)?;
    let total = source.metadata()?.len();
    let mut head = [0u8; 4];
    let head_len = source.read(&mut head)?;
    let first = &head[..head_len];
    let (encoding, bom, endian) = match choice {
        EncodingChoice::Specific(e) => {
            let (bom, endian) = bom_for_choice(first, e);
            (e, bom, endian)
        }
        EncodingChoice::Automatic => {
            if first.starts_with(&[0xff, 0xfe, 0, 0]) {
                (TextEncoding::Utf32, 4, Some(Endian::Little))
            } else if first.starts_with(&[0, 0, 0xfe, 0xff]) {
                (TextEncoding::Utf32Be, 4, Some(Endian::Big))
            } else if first.starts_with(&[0xff, 0xfe]) {
                (TextEncoding::Utf16, 2, Some(Endian::Little))
            } else if first.starts_with(&[0xfe, 0xff]) {
                (TextEncoding::Utf16Be, 2, Some(Endian::Big))
            } else if stream_is_utf8(&mut source)? {
                (
                    TextEncoding::Utf8,
                    usize::from(first.starts_with(&[0xef, 0xbb, 0xbf])) * 3,
                    None,
                )
            } else {
                let cp = unsafe { GetACP() };
                let e = TextEncoding::ALL
                    .iter()
                    .copied()
                    .find(|e| code_page(*e) == Some(cp))
                    .ok_or(CodecError::UnsupportedSystemCodePage(cp))?;
                (e, 0, None)
            }
        }
    };
    if !is_available(encoding) {
        return Err(CodecError::UnavailableEncoding(encoding));
    }
    source.seek(SeekFrom::Start(bom as u64))?;
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    let result = (|| -> Result<Format, CodecError> {
        let mut buffer = vec![0u8; 1024 * 1024 + 8];
        let mut held = 0;
        let mut done = bom as u64;
        let mut counts = [0usize; 3];
        let mut previous_cr = false;
        let mut lossy = false;
        loop {
            if cancel() {
                return Err(CodecError::Cancelled);
            }
            let n = source.read(&mut buffer[held..1024 * 1024 + held])?;
            done += n as u64;
            let end = held + n;
            let final_chunk = n == 0;
            let cut = if final_chunk {
                end
            } else if encoding == TextEncoding::Utf8 {
                match std::str::from_utf8(&buffer[..end]) {
                    Ok(_) => end,
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    Err(_) => return Err(CodecError::InapplicableEncoding(encoding)),
                }
            } else {
                safe_prefix(&buffer[..end], encoding, endian)
            };
            if final_chunk
                && encoding == TextEncoding::Utf8
                && std::str::from_utf8(&buffer[..end]).is_err()
            {
                return Err(CodecError::InapplicableEncoding(encoding));
            }
            let bytes = &buffer[..cut];
            let text = if encoding == TextEncoding::Utf8 {
                std::str::from_utf8(bytes)
                    .map_err(|_| CodecError::InapplicableEncoding(encoding))?
                    .to_owned()
            } else if let Some(order) = endian {
                decode_unicode(bytes, encoding, order)?
            } else {
                cp_decode(bytes, encoding)?
            };
            if encoding != TextEncoding::Utf8 {
                let chunk_format = Format {
                    encoding,
                    has_bom: false,
                    endian,
                    main_line_ending: LineEnding::Lf,
                    lossy: false,
                };
                if encode(&text, &chunk_format).ok().as_deref() != Some(bytes) {
                    lossy = true;
                }
            }
            out.write_all(text.as_bytes())?;
            add_line_counts(&text, &mut counts, &mut previous_cr);
            if final_chunk {
                break;
            }
            held = end - cut;
            buffer.copy_within(cut..end, 0);
            progress(done, total);
        }
        out.sync_all()?;
        let main_line_ending = if counts[0] >= counts[1] && counts[0] >= counts[2] {
            LineEnding::Lf
        } else if counts[1] >= counts[2] {
            LineEnding::CrLf
        } else {
            LineEnding::Cr
        };
        Ok(Format {
            encoding,
            has_bom: bom > 0,
            endian,
            main_line_ending,
            lossy,
        })
    })();
    if result.is_err() {
        drop(out);
        let _ = fs::remove_file(output);
    }
    result
}
