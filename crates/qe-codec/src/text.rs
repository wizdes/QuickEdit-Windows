//! Bytes to text and text to bytes. Follows PlainTextIO.swift on the Mac.
use crate::save::{WatchedSource, with_leftover};
use crate::{CodecError, Decoded, Endian, Format, LineEnding};
use qe_api::{EncodingChoice, TextEncoding};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use windows::Win32::Foundation::{ERROR_INVALID_FLAGS, GetLastError};
use windows::Win32::Globalization::{
    GetACP, IsDBCSLeadByteEx, IsValidCodePage, MB_ERR_INVALID_CHARS, MultiByteToWideChar,
    WC_ERR_INVALID_CHARS, WideCharToMultiByte,
};
use windows::core::{BOOL, PCSTR};

/// Every conversion runs on pieces of this size, so that no Windows call nears
/// its 2 GiB limit and the working buffers stay small.
pub(crate) const CHUNK: usize = 1024 * 1024;
const CP_UTF8: u32 = 65001;
const CP_GB18030: u32 = 54936;
const CP_EUC_JP: u32 = 20932;

pub(crate) fn unicode_endian(e: TextEncoding) -> Option<Endian> {
    match e {
        TextEncoding::Utf16
        | TextEncoding::Utf32
        | TextEncoding::Utf16Le
        | TextEncoding::Utf32Le => Some(Endian::Little),
        TextEncoding::Utf16Be | TextEncoding::Utf32Be => Some(Endian::Big),
        _ => None,
    }
}
fn is_utf16(e: TextEncoding) -> bool {
    matches!(
        e,
        TextEncoding::Utf16 | TextEncoding::Utf16Le | TextEncoding::Utf16Be
    )
}
fn is_utf32(e: TextEncoding) -> bool {
    matches!(
        e,
        TextEncoding::Utf32 | TextEncoding::Utf32Le | TextEncoding::Utf32Be
    )
}
fn code_page(e: TextEncoding) -> Option<u32> {
    use TextEncoding::*;
    Some(match e {
        Windows874 => 874,
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
        Gb18030 => CP_GB18030,
        Big5 => 950,
        Korean949 => 949,
        EucJp => CP_EUC_JP,
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
/// The entry for a Windows system code page. `None` for a page with no entry;
/// Automatic then goes straight to its last step.
fn encoding_for_code_page(cp: u32) -> Option<TextEncoding> {
    if cp == CP_UTF8 {
        return Some(TextEncoding::Utf8);
    }
    TextEncoding::ALL
        .iter()
        .copied()
        .find(|e| code_page(*e) == Some(cp))
}
pub fn is_available(e: TextEncoding) -> bool {
    // SAFETY: the call takes a plain number.
    code_page(e).is_none_or(|cp| unsafe { IsValidCodePage(cp) }.as_bool())
}

/// A count from a Windows conversion call. Zero and below mean failure, with
/// the reason in the thread's last error.
fn conversion_count(count: i32) -> Result<usize, u32> {
    if count <= 0 {
        // SAFETY: the call takes no arguments.
        Err(unsafe { GetLastError() }.0)
    } else {
        Ok(count as usize)
    }
}
/// Strict conversion: bytes with no mapping in this code page fail.
fn bytes_to_wide(cp: u32, bytes: &[u8], wide: &mut Vec<u16>) -> Result<(), u32> {
    wide.clear();
    if bytes.is_empty() {
        return Ok(());
    }
    // SAFETY: each slice is valid for its length, and lengths are at most a few MiB.
    let count =
        conversion_count(unsafe { MultiByteToWideChar(cp, MB_ERR_INVALID_CHARS, bytes, None) })?;
    wide.resize(count, 0);
    // SAFETY: as above; `wide` holds the `count` units the first call asked for.
    let written = conversion_count(unsafe {
        MultiByteToWideChar(cp, MB_ERR_INVALID_CHARS, bytes, Some(wide))
    })?;
    wide.truncate(written);
    Ok(())
}
/// One call to WideCharToMultiByte. Returns the count and whether Windows had to
/// use its default character.
fn wide_call(cp: u32, wide: &[u16], out: Option<&mut [u8]>) -> Result<(usize, bool), u32> {
    // These two pages refuse the "used default" output and take a strict flag.
    let special = cp == CP_GB18030 || cp == CP_UTF8;
    let flags = if special { WC_ERR_INVALID_CHARS } else { 0 };
    let mut used = BOOL(0);
    let used_arg = (!special).then_some(&raw mut used);
    // SAFETY: each slice is valid for its length, lengths are at most a few MiB, and
    // `used` outlives the call.
    let count = conversion_count(unsafe {
        WideCharToMultiByte(cp, flags, wide, out, PCSTR::null(), used_arg)
    })?;
    Ok((count, used.as_bool()))
}
/// Standard EUC-JP: ASCII, 8E plus a half-width kana, two high bytes, or 8F plus
/// two high bytes. Code page 20932 writes JIS X 0212 characters in another form
/// that only Windows reads, so those count as not writable.
fn is_standard_euc_jp(bytes: &[u8]) -> bool {
    let high = |b: Option<&u8>| b.is_some_and(|b| (0xa1..=0xfe).contains(b));
    let mut i = 0;
    while i < bytes.len() {
        let width = match bytes[i] {
            0..=0x7f => 1,
            0x8e if bytes.get(i + 1).is_some_and(|b| (0xa1..=0xdf).contains(b)) => 2,
            0x8f if high(bytes.get(i + 1)) && high(bytes.get(i + 2)) => 3,
            0xa1..=0xfe if high(bytes.get(i + 1)) => 2,
            _ => return false,
        };
        i += width;
    }
    true
}
/// Turns standard EUC-JP into the form code page 20932 reads. The page reads JIS X 0212
/// as two bytes with the last one's high bit clear, and reads a bare 8F as a control
/// character followed by the wrong letter. Any byte outside the standard form gives
/// `None`, so the caller refuses the file and never shows a wrong character.
fn euc_jp_for_code_page(bytes: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let high = |b: Option<&u8>| b.copied().filter(|b| (0xa1..=0xfe).contains(b));
    out.clear();
    out.reserve(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            a @ 0..=0x7f => {
                out.push(a);
                i += 1;
            }
            0x8e if bytes.get(i + 1).is_some_and(|b| (0xa1..=0xdf).contains(b)) => {
                out.extend_from_slice(&bytes[i..i + 2]);
                i += 2;
            }
            0x8f => {
                let (a, b) = (high(bytes.get(i + 1))?, high(bytes.get(i + 2))?);
                out.extend_from_slice(&[a, b & 0x7f]);
                i += 3;
            }
            a @ 0xa1..=0xfe => {
                out.extend_from_slice(&[a, high(bytes.get(i + 1))?]);
                i += 2;
            }
            _ => return None,
        }
    }
    Some(())
}
/// Encodes `text` in pieces and hands each piece to `sink`. No allocation grows
/// with the text, and any output size works.
fn encode_legacy(
    text: &str,
    e: TextEncoding,
    cp: u32,
    verify: bool,
    mut sink: impl FnMut(&[u8]),
) -> Result<(), CodecError> {
    let unencodable = |_| CodecError::Unencodable(e);
    let (mut wide, mut back, mut bytes) = (Vec::new(), Vec::new(), Vec::new());
    let mut rest = text;
    while !rest.is_empty() {
        let (piece, tail) = rest.split_at(rest.floor_char_boundary(CHUNK));
        rest = tail;
        wide.clear();
        wide.extend(piece.encode_utf16());
        let (count, _) = wide_call(cp, &wide, None).map_err(unencodable)?;
        bytes.resize(count, 0);
        let (written, used_default) =
            wide_call(cp, &wide, Some(&mut bytes)).map_err(unencodable)?;
        if used_default {
            return Err(CodecError::Unencodable(e));
        }
        bytes.truncate(written);
        if verify {
            if cp == CP_EUC_JP && !is_standard_euc_jp(&bytes) {
                return Err(CodecError::Unencodable(e));
            }
            // Windows best-fit mappings can change a character without setting the
            // "used default" output (for example é to e in CP1251).
            bytes_to_wide(cp, &bytes, &mut back).map_err(unencodable)?;
            if back != wide {
                return Err(CodecError::Unencodable(e));
            }
        }
        sink(&bytes);
    }
    Ok(())
}

/// Counts of LF, CRLF and lone CR over any number of pieces. Ties go to LF, then
/// CRLF, then CR, as on the Mac.
#[derive(Default)]
struct LineCounts {
    lf: usize,
    crlf: usize,
    cr: usize,
    previous_cr: bool,
}
impl LineCounts {
    fn add(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if b == b'\n' {
                if self.previous_cr {
                    self.cr -= 1;
                    self.crlf += 1;
                } else {
                    self.lf += 1;
                }
            } else if b == b'\r' {
                self.cr += 1;
            }
            self.previous_cr = b == b'\r';
        }
    }
    fn main(&self) -> LineEnding {
        if self.lf >= self.crlf && self.lf >= self.cr {
            LineEnding::Lf
        } else if self.crlf >= self.cr {
            LineEnding::CrLf
        } else {
            LineEnding::Cr
        }
    }
}

/// Strict UTF-8 check over pieces of any size. A character cut by a piece end
/// waits in `pending`; the next piece completes only that character.
#[derive(Default)]
struct Utf8Scanner {
    pending: [u8; 4],
    held: usize,
}
impl Utf8Scanner {
    /// False as soon as the bytes cannot be UTF-8.
    fn feed(&mut self, mut piece: &[u8]) -> bool {
        while self.held > 0 {
            let Some((&byte, rest)) = piece.split_first() else {
                return true;
            };
            piece = rest;
            self.pending[self.held] = byte;
            self.held += 1;
            match std::str::from_utf8(&self.pending[..self.held]) {
                Ok(_) => self.held = 0,
                Err(e) if e.error_len().is_none() => {}
                Err(_) => return false,
            }
        }
        match std::str::from_utf8(piece) {
            Ok(_) => true,
            Err(e) if e.error_len().is_none() => {
                let tail = &piece[e.valid_up_to()..];
                self.pending[..tail.len()].copy_from_slice(tail);
                self.held = tail.len();
                true
            }
            Err(_) => false,
        }
    }
    /// True when no character was left unfinished.
    fn finish(&self) -> bool {
        self.held == 0
    }
}

/// What to read: which encoding is recorded, how long the BOM is, the byte order,
/// and which encoding an error names.
#[derive(Clone, Copy)]
struct Plan {
    encoding: TextEncoding,
    bom: usize,
    endian: Option<Endian>,
    fault: TextEncoding,
}
const UTF32_LE_BOM: [u8; 4] = [0xff, 0xfe, 0, 0];
const UTF32_BE_BOM: [u8; 4] = [0, 0, 0xfe, 0xff];
const UTF16_LE_BOM: [u8; 2] = [0xff, 0xfe];
const UTF16_BE_BOM: [u8; 2] = [0xfe, 0xff];
const UTF8_BOM: [u8; 3] = [0xef, 0xbb, 0xbf];

/// The Mac's `decode(_:as:)`: for plain UTF-16 and UTF-32 the BOM picks the byte
/// order, and a big-endian BOM is recorded as the big-endian encoding so that a
/// save writes the file back as found. No BOM means big-endian.
fn specific_plan(head: &[u8], e: TextEncoding) -> Plan {
    use TextEncoding::*;
    let plan = |encoding, bom, endian| Plan {
        encoding,
        bom,
        endian,
        fault: e,
    };
    let (little, big) = (Some(Endian::Little), Some(Endian::Big));
    match e {
        Utf8 => plan(Utf8, usize::from(head.starts_with(&UTF8_BOM)) * 3, None),
        Utf16 if head.starts_with(&UTF16_LE_BOM) => plan(Utf16, 2, little),
        Utf16 if head.starts_with(&UTF16_BE_BOM) => plan(Utf16Be, 2, big),
        Utf16 => plan(Utf16, 0, big),
        Utf32 if head.starts_with(&UTF32_LE_BOM) => plan(Utf32, 4, little),
        Utf32 if head.starts_with(&UTF32_BE_BOM) => plan(Utf32Be, 4, big),
        Utf32 => plan(Utf32, 0, big),
        Utf16Le => plan(e, usize::from(head.starts_with(&UTF16_LE_BOM)) * 2, little),
        Utf16Be => plan(e, usize::from(head.starts_with(&UTF16_BE_BOM)) * 2, big),
        Utf32Le => plan(e, usize::from(head.starts_with(&UTF32_LE_BOM)) * 4, little),
        Utf32Be => plan(e, usize::from(head.starts_with(&UTF32_BE_BOM)) * 4, big),
        _ => plan(e, 0, None),
    }
}
/// The BOM step of Automatic. UTF-32 first, because its little-endian BOM starts
/// with the UTF-16 one. A bad UTF-32 body names plain UTF-32 in the error, as on the
/// Mac. UTF-8 BOMs wait for the UTF-8 step.
fn automatic_bom_plan(head: &[u8]) -> Option<Plan> {
    if head.starts_with(&UTF32_LE_BOM) || head.starts_with(&UTF32_BE_BOM) {
        Some(specific_plan(head, TextEncoding::Utf32))
    } else if head.starts_with(&UTF16_LE_BOM) || head.starts_with(&UTF16_BE_BOM) {
        Some(specific_plan(head, TextEncoding::Utf16))
    } else {
        None
    }
}
fn legacy_plan(e: TextEncoding) -> Plan {
    Plan {
        encoding: e,
        bom: 0,
        endian: None,
        fault: e,
    }
}
/// The system code page as a step of Automatic, or `None` if it cannot be one:
/// no entry for that page, or UTF-8, which the step before already refused.
fn system_plan(system_cp: u32) -> Option<Plan> {
    encoding_for_code_page(system_cp)
        .filter(|e| *e != TextEncoding::Utf8 && is_available(*e))
        .map(legacy_plan)
}
fn lossy_utf8_format(main_line_ending: LineEnding) -> Format {
    Format {
        lossy: true,
        main_line_ending,
        ..Format::new_document()
    }
}

/// Converts one non-UTF-8 encoding, a piece at a time.
struct Converter {
    plan: Plan,
    code_page: Option<u32>,
    lead: [bool; 256],
    wide: Vec<u16>,
    scratch: Vec<u8>,
    euc_jp: Vec<u8>,
}
impl Converter {
    fn new(plan: Plan) -> Result<Self, CodecError> {
        let e = plan.encoding;
        if !is_available(e) {
            return Err(CodecError::UnavailableEncoding(e));
        }
        let cp = code_page(e);
        let mut lead = [false; 256];
        if let Some(cp) = cp {
            for (byte, slot) in lead.iter_mut().enumerate() {
                // SAFETY: the call takes plain numbers. For a page with no lead bytes
                // it reports false.
                *slot = unsafe { IsDBCSLeadByteEx(cp, byte as u8) }.is_ok();
            }
        }
        Ok(Self {
            plan,
            code_page: cp,
            lead,
            wide: Vec::new(),
            scratch: Vec::new(),
            euc_jp: Vec::new(),
        })
    }
    /// The length of the longest start of `bytes` that does not cut a character.
    /// `bytes` must start on a character start.
    fn cut(&self, bytes: &[u8]) -> usize {
        let e = self.plan.encoding;
        if is_utf16(e) {
            let mut end = bytes.len() & !1;
            if end >= 2 {
                let unit = [bytes[end - 2], bytes[end - 1]];
                let unit = if self.plan.endian == Some(Endian::Little) {
                    u16::from_le_bytes(unit)
                } else {
                    u16::from_be_bytes(unit)
                };
                if (0xd800..=0xdbff).contains(&unit) {
                    end -= 2;
                }
            }
            return end;
        }
        if is_utf32(e) {
            return bytes.len() & !3;
        }
        let cp = self.code_page.unwrap_or(0);
        let mut i = 0;
        while i < bytes.len() {
            let width = self.character_bytes(cp, bytes, i);
            if i + width > bytes.len() {
                return i;
            }
            i += width;
        }
        i
    }
    fn character_bytes(&self, cp: u32, bytes: &[u8], i: usize) -> usize {
        let a = bytes[i];
        match cp {
            CP_GB18030 if (0x81..=0xfe).contains(&a) => {
                if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    4
                } else {
                    2
                }
            }
            CP_EUC_JP if a == 0x8f => 3,
            CP_EUC_JP if a == 0x8e || (0xa1..=0xfe).contains(&a) => 2,
            CP_EUC_JP => 1,
            _ if self.lead[a as usize] => 2,
            _ => 1,
        }
    }
    /// Appends the text of one piece to `out`.
    fn append_text(&mut self, bytes: &[u8], out: &mut String) -> Result<(), CodecError> {
        let e = self.plan.encoding;
        let order = self.plan.endian;
        if is_utf16(e) {
            let units = bytes.as_chunks::<2>().0.iter().map(|b| {
                if order == Some(Endian::Little) {
                    u16::from_le_bytes(*b)
                } else {
                    u16::from_be_bytes(*b)
                }
            });
            out.extend(std::char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')));
            Ok(())
        } else if is_utf32(e) {
            for b in bytes.as_chunks::<4>().0 {
                let n = if order == Some(Endian::Little) {
                    u32::from_le_bytes(*b)
                } else {
                    u32::from_be_bytes(*b)
                };
                out.push(
                    char::from_u32(n).ok_or(CodecError::InapplicableEncoding(self.plan.fault))?,
                );
            }
            Ok(())
        } else {
            let cp = self.code_page.ok_or(CodecError::UnavailableEncoding(e))?;
            let bytes = if cp == CP_EUC_JP {
                euc_jp_for_code_page(bytes, &mut self.euc_jp)
                    .ok_or(CodecError::InapplicableEncoding(self.plan.fault))?;
                &self.euc_jp[..]
            } else {
                bytes
            };
            bytes_to_wide(cp, bytes, &mut self.wide).map_err(|code| {
                if code == ERROR_INVALID_FLAGS.0 {
                    CodecError::UnavailableEncoding(e)
                } else {
                    CodecError::InapplicableEncoding(self.plan.fault)
                }
            })?;
            out.extend(
                std::char::decode_utf16(self.wide.iter().copied()).map(|c| c.unwrap_or('\u{fffd}')),
            );
            Ok(())
        }
    }
    /// True when `text`, saved again, gives exactly `bytes`.
    fn round_trips(&mut self, text: &str, bytes: &[u8]) -> bool {
        self.scratch.clear();
        let e = self.plan.encoding;
        if let Some(cp) = self.code_page {
            let scratch = &mut self.scratch;
            let done = encode_legacy(text, e, cp, true, |piece| scratch.extend_from_slice(piece));
            done.is_ok() && self.scratch == bytes
        } else {
            write_unicode(
                text,
                is_utf16(e),
                self.plan.endian == Some(Endian::Little),
                &mut self.scratch,
            );
            self.scratch == bytes
        }
    }
}
fn reserve_text(text: &mut String, size: usize) -> Result<(), CodecError> {
    text.try_reserve_exact(size)
        .map_err(|_| CodecError::TooLarge {
            what: "there is not enough memory for its text",
        })
}
/// Calls `each` on successive pieces of `body`, each ending on a character end.
fn for_each_piece(
    body: &[u8],
    converter: &mut Converter,
    mut each: impl FnMut(&mut Converter, &[u8]) -> Result<(), CodecError>,
) -> Result<(), CodecError> {
    let mut rest = body;
    while !rest.is_empty() {
        let window = &rest[..rest.len().min(CHUNK)];
        let cut = if window.len() == rest.len() {
            rest.len()
        } else {
            converter.cut(window)
        };
        each(converter, &rest[..cut])?;
        rest = &rest[cut..];
    }
    Ok(())
}
/// Decodes a non-UTF-8 body. The first pass checks every piece, measures the text
/// and finds any loss, with no large buffer. The second pass fills one exact
/// allocation, so memory is the input plus the text and a few MiB.
fn decode_body(bytes: &[u8], plan: Plan) -> Result<Decoded, CodecError> {
    let mut converter = Converter::new(plan)?;
    let mut lines = LineCounts::default();
    let (mut size, mut lossy) = (0usize, false);
    let mut piece_text = String::new();
    let body = &bytes[plan.bom..];
    for_each_piece(body, &mut converter, |converter, piece| {
        piece_text.clear();
        converter.append_text(piece, &mut piece_text)?;
        size += piece_text.len();
        lines.add(piece_text.as_bytes());
        lossy |= !converter.round_trips(&piece_text, piece);
        Ok(())
    })?;
    let mut text = String::new();
    reserve_text(&mut text, size)?;
    for_each_piece(body, &mut converter, |converter, piece| {
        converter.append_text(piece, &mut text)
    })?;
    Ok(Decoded {
        text,
        format: Format {
            encoding: plan.encoding,
            has_bom: plan.bom != 0,
            endian: plan.endian,
            main_line_ending: lines.main(),
            lossy,
        },
    })
}
fn utf8_decoded(mut text: String, has_bom: bool) -> Decoded {
    if has_bom {
        text.drain(..UTF8_BOM.len());
    }
    let mut lines = LineCounts::default();
    lines.add(text.as_bytes());
    Decoded {
        text,
        format: Format {
            has_bom,
            main_line_ending: lines.main(),
            ..Format::new_document()
        },
    }
}
/// Decodes file bytes. A chosen encoding wins and refuses bytes that do not fit.
/// Automatic reads a BOM, then strict UTF-8, then the Windows system code page,
/// and at last lossy UTF-8. As on the Mac, a UTF-16 or UTF-32 BOM ends the search: a
/// UTF-32 body with a value that is not a character gives `InapplicableEncoding`.
pub fn decode(bytes: impl Into<Vec<u8>>, choice: EncodingChoice) -> Result<Decoded, CodecError> {
    // SAFETY: the call takes no arguments.
    decode_with_system_page(bytes.into(), choice, unsafe { GetACP() })
}
fn decode_with_system_page(
    bytes: Vec<u8>,
    choice: EncodingChoice,
    system_cp: u32,
) -> Result<Decoded, CodecError> {
    let plan = match choice {
        EncodingChoice::Specific(e) => Some(specific_plan(&bytes, e)),
        EncodingChoice::Automatic => automatic_bom_plan(&bytes),
    };
    if let Some(plan) = plan {
        if plan.encoding != TextEncoding::Utf8 {
            return decode_body(&bytes, plan);
        }
        return match String::from_utf8(bytes) {
            Ok(text) => Ok(utf8_decoded(text, plan.bom != 0)),
            Err(_) => Err(CodecError::InapplicableEncoding(plan.fault)),
        };
    }
    let bytes = match String::from_utf8(bytes) {
        Ok(text) => {
            let has_bom = text.as_bytes().starts_with(&UTF8_BOM);
            return Ok(utf8_decoded(text, has_bom));
        }
        Err(e) => e.into_bytes(),
    };
    if let Some(plan) = system_plan(system_cp) {
        match decode_body(&bytes, plan) {
            Err(CodecError::InapplicableEncoding(_)) => {}
            finished => return finished,
        }
    }
    // The Mac's last step: the bytes fit no encoding it may use.
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let mut lines = LineCounts::default();
    lines.add(text.as_bytes());
    Ok(Decoded {
        text,
        format: lossy_utf8_format(lines.main()),
    })
}

fn write_unicode(text: &str, utf16: bool, little: bool, out: &mut Vec<u8>) {
    if utf16 {
        for u in text.encode_utf16() {
            out.extend_from_slice(&if little {
                u.to_le_bytes()
            } else {
                u.to_be_bytes()
            });
        }
    } else {
        for c in text.chars() {
            let n = c as u32;
            out.extend_from_slice(&if little {
                n.to_le_bytes()
            } else {
                n.to_be_bytes()
            });
        }
    }
}
fn reserve_output(out: &mut Vec<u8>, size: usize) -> Result<(), CodecError> {
    out.try_reserve_exact(size)
        .map_err(|_| CodecError::TooLarge {
            what: "there is not enough memory for its bytes",
        })
}
/// Writes `text` in `format`. Never converts line endings. Plain UTF-16 and UTF-32
/// with a BOM write little-endian, as on the Mac; read without a BOM they write
/// big-endian with no BOM. A format whose fields disagree gives `InvalidFormat`.
pub fn encode(text: &str, format: &Format) -> Result<Vec<u8>, CodecError> {
    format.check()?;
    let e = format.encoding;
    let mut out = Vec::new();
    if e == TextEncoding::Utf8 {
        reserve_output(&mut out, text.len() + if format.has_bom { 3 } else { 0 })?;
        if format.has_bom {
            out.extend_from_slice(&UTF8_BOM);
        }
        out.extend_from_slice(text.as_bytes());
        return Ok(out);
    }
    if let Some(mut endian) = format.endian.or_else(|| unicode_endian(e)) {
        if matches!(e, TextEncoding::Utf16 | TextEncoding::Utf32) && format.has_bom {
            endian = Endian::Little;
        }
        let little = endian == Endian::Little;
        let utf16 = is_utf16(e);
        let body = if utf16 {
            text.encode_utf16().count() * 2
        } else {
            text.chars().count() * 4
        };
        reserve_output(&mut out, body + 4)?;
        if format.has_bom {
            out.extend_from_slice(match (utf16, little) {
                (true, true) => &UTF16_LE_BOM,
                (true, false) => &UTF16_BE_BOM,
                (false, true) => &UTF32_LE_BOM,
                (false, false) => &UTF32_BE_BOM,
            });
        }
        write_unicode(text, utf16, little, &mut out);
        return Ok(out);
    }
    let cp = code_page(e).ok_or(CodecError::UnavailableEncoding(e))?;
    if !is_available(e) {
        return Err(CodecError::UnavailableEncoding(e));
    }
    let mut size = 0;
    encode_legacy(text, e, cp, true, |piece| size += piece.len())?;
    reserve_output(&mut out, size)?;
    encode_legacy(text, e, cp, false, |piece| out.extend_from_slice(piece))?;
    Ok(out)
}
/// True when `encoding` can hold every character of `text`. Allocates nothing
/// that grows with the text, and handles output of any size.
pub fn can_encode(text: &str, encoding: TextEncoding) -> Result<bool, CodecError> {
    if !is_available(encoding) {
        return Err(CodecError::UnavailableEncoding(encoding));
    }
    let Some(cp) = code_page(encoding) else {
        return Ok(true);
    };
    match encode_legacy(text, encoding, cp, true, |_| {}) {
        Ok(()) => Ok(true),
        Err(CodecError::Unencodable(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Sends progress upward only, and never past `total`. `base` moves the numbers past
/// an earlier phase. An empty file has a total of 0 and reports `(0, 0)` as its finish.
struct Progress<F> {
    report: F,
    total: u64,
    base: u64,
    best: u64,
}
impl<F: FnMut(u64, u64)> Progress<F> {
    fn at(&mut self, done: u64) {
        let now = (self.base + done).min(self.total);
        if now >= self.best {
            self.best = now;
            (self.report)(now, self.total);
        }
    }
}
/// The UTF-8 step of Automatic, with progress and cancel on every piece. On a bad
/// byte it stops at once, since the rest cannot change the answer.
fn stream_is_utf8<F: FnMut(u64, u64)>(
    source: &mut WatchedSource,
    progress: &mut Progress<F>,
    cancel: &mut impl FnMut() -> bool,
) -> Result<bool, CodecError> {
    source.seek_to(0)?;
    let mut chunk = vec![0u8; CHUNK];
    let mut scanner = Utf8Scanner::default();
    let mut done = 0;
    loop {
        if cancel() {
            return Err(CodecError::Cancelled);
        }
        let n = source.read(&mut chunk)?;
        if n == 0 {
            return Ok(scanner.finish());
        }
        if !scanner.feed(&chunk[..n]) {
            return Ok(false);
        }
        done += n as u64;
        progress.at(done);
    }
}
fn write_utf8_stream<F: FnMut(u64, u64)>(
    source: &mut WatchedSource,
    out: &mut impl TextSink,
    plan: Plan,
    progress: &mut Progress<F>,
    cancel: &mut impl FnMut() -> bool,
) -> Result<Format, CodecError> {
    source.seek_to(plan.bom as u64)?;
    let mut chunk = vec![0u8; CHUNK];
    let mut scanner = Utf8Scanner::default();
    let mut lines = LineCounts::default();
    let mut done = plan.bom as u64;
    loop {
        if cancel() {
            return Err(CodecError::Cancelled);
        }
        let n = source.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        if !scanner.feed(&chunk[..n]) {
            return Err(CodecError::InapplicableEncoding(plan.fault));
        }
        done += n as u64;
        out.put(&chunk[..n], done)?;
        lines.add(&chunk[..n]);
        progress.at(done);
    }
    if !scanner.finish() {
        return Err(CodecError::InapplicableEncoding(plan.fault));
    }
    Ok(Format {
        has_bom: plan.bom != 0,
        main_line_ending: lines.main(),
        ..Format::new_document()
    })
}
/// Whether a pass must find out if a save would change the bytes. That check is slow,
/// and a second pass over a file already has the answer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LossCheck {
    Run,
    Skip,
}
fn write_converted_stream<F: FnMut(u64, u64)>(
    source: &mut WatchedSource,
    out: &mut impl TextSink,
    plan: Plan,
    loss_check: LossCheck,
    progress: &mut Progress<F>,
    cancel: &mut impl FnMut() -> bool,
) -> Result<Format, CodecError> {
    let mut converter = Converter::new(plan)?;
    source.seek_to(plan.bom as u64)?;
    let mut buffer = vec![0u8; CHUNK + 8];
    let mut text = String::new();
    let mut lines = LineCounts::default();
    let (mut held, mut done, mut lossy) = (0, plan.bom as u64, false);
    loop {
        if cancel() {
            return Err(CodecError::Cancelled);
        }
        let n = source.read(&mut buffer[held..CHUNK + held])?;
        done += n as u64;
        let end = held + n;
        let last = n == 0;
        let cut = if last {
            end
        } else {
            converter.cut(&buffer[..end])
        };
        text.clear();
        converter.append_text(&buffer[..cut], &mut text)?;
        if loss_check == LossCheck::Run {
            lossy |= !converter.round_trips(&text, &buffer[..cut]);
        }
        out.put(text.as_bytes(), done)?;
        lines.add(text.as_bytes());
        if last {
            break;
        }
        held = end - cut;
        buffer.copy_within(cut..end, 0);
        progress.at(done);
    }
    Ok(Format {
        encoding: plan.encoding,
        has_bom: plan.bom != 0,
        endian: plan.endian,
        main_line_ending: lines.main(),
        lossy,
    })
}
/// The last step of Automatic, in pieces: bad bytes become U+FFFD exactly as
/// `String::from_utf8_lossy` does it.
fn write_lossy_stream<F: FnMut(u64, u64)>(
    source: &mut WatchedSource,
    out: &mut impl TextSink,
    progress: &mut Progress<F>,
    cancel: &mut impl FnMut() -> bool,
) -> Result<Format, CodecError> {
    source.seek_to(0)?;
    let mut buffer = vec![0u8; CHUNK + 4];
    let mut lines = LineCounts::default();
    let (mut held, mut done) = (0, 0u64);
    let mut emit = |bytes: &[u8], done: u64| -> Result<(), CodecError> {
        out.put(bytes, done)?;
        lines.add(bytes);
        Ok(())
    };
    loop {
        if cancel() {
            return Err(CodecError::Cancelled);
        }
        let n = source.read(&mut buffer[held..CHUNK + held])?;
        done += n as u64;
        let end = held + n;
        let last = n == 0;
        let mut start = 0;
        while start < end {
            match std::str::from_utf8(&buffer[start..end]) {
                Ok(_) => {
                    emit(&buffer[start..end], done)?;
                    start = end;
                }
                Err(e) => {
                    let good = start + e.valid_up_to();
                    emit(&buffer[start..good], done)?;
                    start = good;
                    match e.error_len() {
                        Some(bad) => {
                            emit("\u{fffd}".as_bytes(), done)?;
                            start += bad;
                        }
                        None if last => {
                            emit("\u{fffd}".as_bytes(), done)?;
                            start = end;
                        }
                        None => break,
                    }
                }
            }
        }
        if last {
            break;
        }
        held = end - start;
        buffer.copy_within(start..end, 0);
        progress.at(done);
    }
    Ok(lossy_utf8_format(lines.main()))
}
/// Where streamed text goes. `input_done` is how many file bytes have been read so far.
trait TextSink {
    fn put(&mut self, text: &[u8], input_done: u64) -> Result<(), CodecError>;
}
impl TextSink for File {
    fn put(&mut self, text: &[u8], _input_done: u64) -> Result<(), CodecError> {
        Ok(self.write_all(text)?)
    }
}
/// Counts bytes, for a file whose text did not fit the first estimate.
#[derive(Default)]
struct SizeCounter(usize);
impl TextSink for SizeCounter {
    fn put(&mut self, text: &[u8], _input_done: u64) -> Result<(), CodecError> {
        self.0 += text.len();
        Ok(())
    }
}
/// Holds the text in one block that never moves. Growing a block would hold the old
/// and new blocks at once, so text that does not fit sets `overflowed` and stops, and
/// the caller measures the file and starts again. No write can end the process, since
/// memory is only asked for through `try_reserve_exact`.
struct FilledBytes {
    bytes: Vec<u8>,
    /// The file's size, until the first piece has set the size of the block.
    estimating_from: Option<u64>,
    overflowed: bool,
}
impl FilledBytes {
    fn exact(size: usize) -> Result<Self, CodecError> {
        let mut bytes = Vec::new();
        reserve_output(&mut bytes, size)?;
        Ok(Self {
            bytes,
            estimating_from: None,
            overflowed: false,
        })
    }
    fn estimating(file_size: u64) -> Self {
        Self {
            bytes: Vec::new(),
            estimating_from: Some(file_size),
            overflowed: false,
        }
    }
    /// The block for a whole file, from what the first piece turned into, plus about
    /// 3%. A file that is not uniform either starts again or gets a block that is too
    /// large, which `into_bytes` gives back.
    fn estimate(piece_text: usize, input_done: u64, file_size: u64) -> u128 {
        let whole = piece_text as u128 * u128::from(file_size) / u128::from(input_done.max(1));
        whole + whole / 32 + 64
    }
    fn into_bytes(mut self) -> Vec<u8> {
        let spare = self.bytes.capacity() - self.bytes.len();
        if spare > self.bytes.len() / 8 + CHUNK {
            self.bytes.shrink_to_fit();
        }
        self.bytes
    }
}
impl TextSink for FilledBytes {
    fn put(&mut self, text: &[u8], input_done: u64) -> Result<(), CodecError> {
        if text.is_empty() {
            return Ok(());
        }
        if let Some(file_size) = self.estimating_from.take() {
            let wanted = Self::estimate(text.len(), input_done, file_size);
            let reserved = usize::try_from(wanted)
                .ok()
                .is_some_and(|wanted| self.bytes.try_reserve_exact(wanted).is_ok());
            // Too large for the estimate says nothing about the real size, which the
            // measuring pass finds. Only that pass may give `TooLarge` to the caller.
            self.overflowed = !reserved;
        }
        if self.overflowed || self.bytes.capacity() - self.bytes.len() < text.len() {
            self.overflowed = true;
            return Err(CodecError::TooLarge {
                what: "its text is larger than the memory set aside for it",
            });
        }
        self.bytes.extend_from_slice(text);
        Ok(())
    }
}
/// How a file turned out to need reading.
#[derive(Clone, Copy)]
enum Route {
    Utf8(Plan),
    Converted(Plan),
    LossyUtf8,
}
/// One pass over the file. A refusal is only trusted if the file is still the one that
/// was opened; otherwise the bad byte may be the other program's, and it says so.
fn run_route<F: FnMut(u64, u64)>(
    route: Route,
    source: &mut WatchedSource,
    sink: &mut impl TextSink,
    loss_check: LossCheck,
    progress: &mut Progress<F>,
    cancel: &mut impl FnMut() -> bool,
) -> Result<Format, CodecError> {
    let result = match route {
        Route::Utf8(plan) => write_utf8_stream(source, sink, plan, progress, cancel),
        Route::Converted(plan) => {
            write_converted_stream(source, sink, plan, loss_check, progress, cancel)
        }
        Route::LossyUtf8 => write_lossy_stream(source, sink, progress, cancel),
    };
    if matches!(result, Err(CodecError::InapplicableEncoding(_))) {
        source.confirm_unchanged()?;
    }
    result
}
/// The start of a streaming read: the file and its first bytes.
struct Opened {
    source: WatchedSource,
    head: [u8; 4],
    head_len: usize,
}
fn open_for_stream(input: &Path) -> Result<Opened, CodecError> {
    let mut source = WatchedSource::open(input)?;
    let mut head = [0u8; 4];
    let mut head_len = 0;
    while head_len < head.len() {
        let n = source.read(&mut head[head_len..])?;
        if n == 0 {
            break;
        }
        head_len += n;
    }
    Ok(Opened {
        source,
        head,
        head_len,
    })
}
fn utf8_route(opened: &Opened) -> Route {
    let has_bom = opened.head[..opened.head_len].starts_with(&UTF8_BOM);
    Route::Utf8(Plan {
        bom: usize::from(has_bom) * UTF8_BOM.len(),
        ..legacy_plan(TextEncoding::Utf8)
    })
}
fn planned_route(plan: Plan) -> Route {
    if plan.encoding == TextEncoding::Utf8 {
        Route::Utf8(plan)
    } else {
        Route::Converted(plan)
    }
}
/// Picks the route the same way `decode` does, and writes the text to `out`. A scan
/// comes first under Automatic, so that a wrong route rarely writes anything.
fn stream_choosing_route<F: FnMut(u64, u64)>(
    opened: &mut Opened,
    plan: Option<Plan>,
    system_cp: u32,
    out: &mut File,
    progress: &mut Progress<F>,
    cancel: &mut impl FnMut() -> bool,
) -> Result<Format, CodecError> {
    let check = LossCheck::Run;
    if let Some(plan) = plan {
        return run_route(
            planned_route(plan),
            &mut opened.source,
            out,
            check,
            progress,
            cancel,
        );
    }
    let is_utf8 = stream_is_utf8(&mut opened.source, progress, cancel)?;
    progress.base += opened.source.size();
    if is_utf8 {
        let route = utf8_route(opened);
        return run_route(route, &mut opened.source, out, check, progress, cancel);
    }
    let source = &mut opened.source;
    if let Some(plan) = system_plan(system_cp) {
        match run_route(Route::Converted(plan), source, out, check, progress, cancel) {
            Err(CodecError::InapplicableEncoding(_)) => {
                out.set_len(0)?;
                out.seek(SeekFrom::Start(0))?;
            }
            finished => return finished,
        }
    }
    run_route(Route::LossyUtf8, source, out, check, progress, cancel)
}
fn choice_plan(choice: EncodingChoice, head: &[u8]) -> Result<Option<Plan>, CodecError> {
    let plan = match choice {
        EncodingChoice::Specific(e) => Some(specific_plan(head, e)),
        EncodingChoice::Automatic => automatic_bom_plan(head),
    };
    if let Some(plan) = plan
        && !is_available(plan.encoding)
    {
        return Err(CodecError::UnavailableEncoding(plan.encoding));
    }
    Ok(plan)
}
/// Streaming `decode`: writes the text as UTF-8 to a new file at `output` and
/// returns the same format `decode` would. Memory stays at a few MiB.
///
/// `progress` gets `(done, total)` and only rises. `total` is the file size, or
/// twice that when Automatic must scan the file before it converts. `done` never
/// passes `total`. An empty file reports `(0, 0)` once, as its finish, so a caller
/// that shows a percentage must handle a total of zero. `cancel` is asked before
/// every 1 MiB of work, scan included.
///
/// After any failure `output` is gone. If it could not be deleted, the error is
/// `CleanupFailed`, which names it, or `SourceChanged` with it in `leftover`. If
/// `input` changes during the read, the error is `SourceChanged`.
pub fn transcode_to_utf8_file(
    input: &Path,
    output: &Path,
    choice: EncodingChoice,
    progress: impl FnMut(u64, u64),
    cancel: impl FnMut() -> bool,
) -> Result<Format, CodecError> {
    // SAFETY: the call takes no arguments.
    let system_cp = unsafe { GetACP() };
    transcode_with_system_page(input, output, choice, system_cp, progress, cancel)
}
fn transcode_with_system_page(
    input: &Path,
    output: &Path,
    choice: EncodingChoice,
    system_cp: u32,
    progress: impl FnMut(u64, u64),
    mut cancel: impl FnMut() -> bool,
) -> Result<Format, CodecError> {
    let mut opened = open_for_stream(input)?;
    let plan = choice_plan(choice, &opened.head[..opened.head_len])?;
    let size = opened.source.size();
    let mut progress = Progress {
        report: progress,
        total: size * if plan.is_none() { 2 } else { 1 },
        base: 0,
        best: 0,
    };
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    let result = stream_choosing_route(
        &mut opened,
        plan,
        system_cp,
        &mut out,
        &mut progress,
        &mut cancel,
    )
    .and_then(|format| {
        out.sync_all()?;
        progress.at(size);
        Ok(format)
    });
    match result {
        Ok(format) => Ok(format),
        Err(error) => {
            drop(out);
            // `output` is the caller's own scratch file. The first failure is what the
            // caller needs most, and this one is added to it.
            let stuck = match std::fs::remove_file(output) {
                Ok(()) => None,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => Some(output.to_path_buf()),
            };
            Err(with_leftover(error, stuck))
        }
    }
}
/// One route over a whole file: the text bytes and the format.
///
/// Most files take one pass. The first piece sets the size of one block, and the text
/// fills it. If the text does not fit, the block is freed before anything else happens,
/// the file is measured, and a second pass fills a block of the exact size. So peak
/// memory stays at the larger of that first block and the text, plus a few MiB.
fn read_route<F: FnMut(u64, u64)>(
    route: Route,
    source: &mut WatchedSource,
    progress: &mut Progress<F>,
    cancel: &mut impl FnMut() -> bool,
) -> Result<(Vec<u8>, Format), CodecError> {
    let too_large = || CodecError::TooLarge {
        what: "there is not enough memory for its text",
    };
    let size = source.size();
    let first = match route {
        // UTF-8 text is the file itself, so the size is already known.
        Route::Utf8(_) => Some(FilledBytes::exact(
            usize::try_from(size).map_err(|_| too_large())?,
        )?),
        Route::Converted(_) => Some(FilledBytes::estimating(size)),
        // Rare, and its first piece says little: it can end at the first bad byte.
        Route::LossyUtf8 => None,
    };
    if let Some(mut sink) = first {
        match run_route(route, source, &mut sink, LossCheck::Run, progress, cancel) {
            Ok(format) => return Ok((sink.into_bytes(), format)),
            Err(_) if sink.overflowed => {}
            Err(error) => return Err(error),
        }
    }
    let mut counter = SizeCounter::default();
    let format = run_route(
        route,
        source,
        &mut counter,
        LossCheck::Run,
        progress,
        cancel,
    )?;
    let mut sink = FilledBytes::exact(counter.0)?;
    match run_route(route, source, &mut sink, LossCheck::Skip, progress, cancel) {
        Ok(_) => Ok((sink.into_bytes(), format)),
        // The measure was exact, so more text means that the file changed.
        Err(_) if sink.overflowed => Err(CodecError::SourceChanged {
            path: source.shown_path(),
            leftover: None,
        }),
        Err(error) => Err(error),
    }
}
/// `decode` for a file on disk. It never holds the file's bytes in memory. Its peak
/// is the text plus about 3% and a few MiB for a file whose start is like the rest.
/// A file whose first 1 MiB gives more text per byte than the rest, such as Japanese
/// and then ASCII, peaks at that first rate instead; the extra memory is reserved
/// but not touched. A file whose first 1 MiB gives less takes a second pass, at the
/// same peak.
///
/// `progress` gets `(done, total)` and only rises. `total` is the file size, and `done`
/// never passes it. An empty file reports `(0, 0)` once, as its finish, so a caller that
/// shows a percentage must handle a total of zero. A second pass holds the number still
/// until it catches up. `cancel` is asked before every 1 MiB of work.
///
/// If the file changes during the read, the error is `SourceChanged`: a longer or
/// shorter file, a new write time, or another file under that name.
pub fn decode_file(
    input: &Path,
    choice: EncodingChoice,
    progress: impl FnMut(u64, u64),
    cancel: impl FnMut() -> bool,
) -> Result<Decoded, CodecError> {
    // SAFETY: the call takes no arguments.
    let system_cp = unsafe { GetACP() };
    decode_file_with_system_page(input, choice, system_cp, progress, cancel)
}
fn decode_file_with_system_page(
    input: &Path,
    choice: EncodingChoice,
    system_cp: u32,
    progress: impl FnMut(u64, u64),
    mut cancel: impl FnMut() -> bool,
) -> Result<Decoded, CodecError> {
    let mut opened = open_for_stream(input)?;
    let plan = choice_plan(choice, &opened.head[..opened.head_len])?;
    let size = opened.source.size();
    let mut progress = Progress {
        report: progress,
        total: size,
        base: 0,
        best: 0,
    };
    // Each route but the last may refuse the file, which moves on to the next. The first
    // route needs no scan: a bad byte stops it at that byte.
    let (tries, last) = match plan {
        Some(plan) => (Vec::new(), planned_route(plan)),
        None => {
            let mut tries = vec![utf8_route(&opened)];
            tries.extend(system_plan(system_cp).map(Route::Converted));
            (tries, Route::LossyUtf8)
        }
    };
    let finish = |(bytes, format): (Vec<u8>, Format)| {
        // No copy is made. The bytes were checked or built as text piece by piece, so
        // this cannot fail unless this crate has a bug. Then it is an error, not a panic.
        String::from_utf8(bytes)
            .map(|text| Decoded { text, format })
            .map_err(|_| CodecError::InapplicableEncoding(TextEncoding::Utf8))
    };
    let source = &mut opened.source;
    for route in tries {
        match read_route(route, source, &mut progress, &mut cancel) {
            Err(CodecError::InapplicableEncoding(_)) => {}
            finished => {
                let decoded = finish(finished?)?;
                progress.at(size);
                return Ok(decoded);
            }
        }
    }
    let decoded = finish(read_route(last, source, &mut progress, &mut cancel)?)?;
    progress.at(size);
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Item 7: nothing here can end the process by growing a block.
    #[test]
    fn filled_bytes_never_grows_its_block() {
        let mut sink = FilledBytes::exact(4).unwrap();
        sink.put(b"abc", 3).unwrap();
        let block = sink.bytes.as_ptr();
        let error = sink.put(b"de", 5).unwrap_err();
        assert!(matches!(error, CodecError::TooLarge { .. }), "{error}");
        assert!(sink.overflowed);
        assert_eq!(sink.bytes, b"abc");
        assert_eq!(sink.bytes.as_ptr(), block);
        assert!(matches!(
            FilledBytes::exact(usize::MAX / 2),
            Err(CodecError::TooLarge { .. })
        ));
    }

    #[test]
    fn filled_bytes_estimates_from_the_first_piece() {
        // 1 MiB became 1.5 MiB, and the file is 100 MiB.
        let wanted = FilledBytes::estimate(1_572_864, 1 << 20, 100 << 20);
        assert_eq!(wanted, 157_286_400 + 157_286_400 / 32 + 64);
        // An estimate too large for memory is not an error yet: the measuring pass decides.
        let mut sink = FilledBytes::estimating(u64::MAX);
        assert!(sink.put(b"text", 1).is_err());
        assert!(sink.overflowed);
        // Uniform text fits the estimate, whatever the first read's size.
        let mut sink = FilledBytes::estimating(30);
        for done in [10, 20, 30] {
            sink.put(&[b'a'; 15], done).unwrap();
        }
        assert!(!sink.overflowed);
        assert_eq!(sink.into_bytes().len(), 45);
    }

    #[test]
    fn filled_bytes_gives_back_a_block_that_was_far_too_large() {
        let mut sink = FilledBytes::exact(50 << 20).unwrap();
        sink.put(b"small", 5).unwrap();
        assert!(sink.into_bytes().capacity() < CHUNK);
        // A small spare part is kept: giving it back can copy the whole block.
        let mut sink = FilledBytes::exact(8 << 20).unwrap();
        sink.put(&vec![b'a'; 7 << 20], 1).unwrap();
        assert!(sink.into_bytes().capacity() == 8 << 20);
    }

    #[test]
    fn conversion_count_refuses_zero_and_negative() {
        assert!(conversion_count(0).is_err());
        assert!(conversion_count(-1).is_err());
        assert!(conversion_count(i32::MIN).is_err());
        assert_eq!(conversion_count(7), Ok(7));
    }

    #[test]
    fn reserve_failure_says_too_large_not_encoding() {
        let mut text = String::new();
        let error = reserve_text(&mut text, usize::MAX / 2).unwrap_err();
        assert!(error.to_string().contains("too large"), "{error}");
        let mut out = Vec::new();
        let error = reserve_output(&mut out, usize::MAX / 2).unwrap_err();
        assert!(matches!(error, CodecError::TooLarge { .. }), "{error}");
        assert!(error.to_string().contains("too large"), "{error}");
        assert!(!error.to_string().contains("encoding"), "{error}");
    }

    #[test]
    fn every_windows_system_code_page_has_an_entry() {
        for (cp, encoding) in [
            (874, TextEncoding::Windows874),
            (932, TextEncoding::ShiftJis),
            (936, TextEncoding::Gbk),
            (949, TextEncoding::Korean949),
            (950, TextEncoding::Big5),
            (1250, TextEncoding::Windows1250),
            (1251, TextEncoding::Windows1251),
            (1252, TextEncoding::Windows1252),
            (1253, TextEncoding::Windows1253),
            (1254, TextEncoding::Windows1254),
            (1255, TextEncoding::Windows1255),
            (1256, TextEncoding::Windows1256),
            (1257, TextEncoding::Windows1257),
            (1258, TextEncoding::Windows1258),
            (65001, TextEncoding::Utf8),
        ] {
            assert_eq!(encoding_for_code_page(cp), Some(encoding), "{cp}");
        }
    }

    fn automatic(bytes: &[u8], cp: u32) -> Decoded {
        decode_with_system_page(bytes.to_vec(), EncodingChoice::Automatic, cp).unwrap()
    }

    #[test]
    fn automatic_last_step_is_lossy_utf8_as_on_the_mac() {
        // 81 20 is not valid in Shift JIS, and FF 41 is not valid UTF-8.
        let bytes = [0x81, 0x20, b'\r', b'\n', 0xff, b'A', b'\r', b'\n'];
        for cp in [932, 65001, 37] {
            let d = automatic(&bytes[..], cp);
            assert_eq!(d.text, "\u{fffd} \r\n\u{fffd}A\r\n", "code page {cp}");
            assert_eq!(d.format.encoding, TextEncoding::Utf8);
            assert!(d.format.lossy);
            assert!(!d.format.has_bom);
            assert_eq!(d.format.main_line_ending, LineEnding::CrLf);
        }
    }

    #[test]
    fn automatic_system_page_that_fits_is_used_and_marks_duplicate_mappings_lossy() {
        // 8A BF is 漢 in Shift JIS and is not valid UTF-8.
        let d = automatic(&[0x8a, 0xbf], 932);
        assert_eq!(d.text, "\u{6f22}");
        assert_eq!(d.format.encoding, TextEncoding::ShiftJis);
        assert!(!d.format.lossy);
        // EE EF and FA 40 both read as U+2170; Windows writes FA 40.
        let d = automatic(&[0xee, 0xef], 932);
        assert_eq!(d.text, "\u{2170}");
        assert_eq!(d.format.encoding, TextEncoding::ShiftJis);
        assert!(d.format.lossy);
        // A Thai system: A1 is ก.
        let d = automatic(&[0xa1, 0xa2], 874);
        assert_eq!(d.format.encoding, TextEncoding::Windows874);
        assert_eq!(d.text.chars().next(), Some('\u{0e01}'));
    }

    /// The same bytes through `decode` and `transcode_to_utf8_file` give the same
    /// result, for files far larger than the 1 MiB piece and for every last step.
    #[test]
    fn stream_and_memory_agree_on_automatic_with_each_system_page() {
        let dir = std::env::temp_dir().join(format!("qe-codec-agree-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut invalid_everywhere = vec![0xe6, 0xbc];
        invalid_everywhere.extend(std::iter::repeat_n(b'a', CHUNK - 3));
        // A cut character, then a good one that crosses the piece end.
        invalid_everywhere.extend([0xe6, 0xbc, 0xa2]);
        invalid_everywhere.extend([0xff, b'\r', b'\n', 0xe6]);
        for (index, cp) in [932u32, 65001, 37, 1252].into_iter().enumerate() {
            let input = dir.join(format!("in{index}"));
            let output = dir.join(format!("out{index}"));
            std::fs::write(&input, &invalid_everywhere).unwrap();
            let memory = automatic(&invalid_everywhere, cp);
            let mut last = (0, 0);
            let format = transcode_with_system_page(
                &input,
                &output,
                EncodingChoice::Automatic,
                cp,
                |done, total| {
                    assert!(done >= last.0, "progress fell");
                    last = (done, total);
                },
                || false,
            )
            .unwrap();
            assert_eq!(format, memory.format, "code page {cp}");
            assert_eq!(
                std::fs::read_to_string(&output).unwrap(),
                memory.text,
                "code page {cp}"
            );
            assert_eq!(last.0, last.1, "progress did not finish");
            let mut last = (0, 0);
            let whole = decode_file_with_system_page(
                &input,
                EncodingChoice::Automatic,
                cp,
                |done, total| {
                    assert!(done >= last.0, "progress fell");
                    last = (done, total);
                },
                || false,
            )
            .unwrap();
            assert_eq!(whole, memory, "decode_file, code page {cp}");
            assert_eq!(last.0, last.1, "decode_file progress did not finish");
            std::fs::remove_file(input).unwrap();
            std::fs::remove_file(output).unwrap();
        }
        std::fs::remove_dir(dir).unwrap();
    }

    /// The whole of Automatic on a chosen system page, against a hand-made answer.
    #[test]
    fn automatic_reads_every_byte_value_as_the_system_page_says() {
        let bytes: Vec<u8> = (0..=255).collect();
        let d = automatic(&bytes, 1252);
        assert_eq!(d.format.encoding, TextEncoding::Windows1252);
        assert!(!d.format.lossy);
        let chars: Vec<char> = d.text.chars().collect();
        assert_eq!(chars.len(), 256);
        for (byte, c) in chars.iter().enumerate().take(0x80) {
            assert_eq!(*c as usize, byte);
        }
        // From the published CP1252 table, including the bytes with no letter.
        for (byte, wanted) in [
            (0x80, '\u{20ac}'),
            (0x81, '\u{81}'),
            (0x8d, '\u{8d}'),
            (0x8f, '\u{8f}'),
            (0x90, '\u{90}'),
            (0x99, '\u{2122}'),
            (0x9d, '\u{9d}'),
            (0x9f, '\u{178}'),
            (0xa0, '\u{a0}'),
            (0xe9, '\u{e9}'),
            (0xff, '\u{ff}'),
        ] {
            assert_eq!(chars[byte], wanted, "{byte:#x}");
        }
        // A page with no entry goes straight to the last step.
        let d = automatic(&bytes, 37);
        assert_eq!(d.text, String::from_utf8_lossy(&bytes));
        assert!(d.format.lossy);
    }

    #[test]
    fn scanner_completes_only_the_pending_character() {
        // Fed in every split, including one byte at a time.
        let text = "a\u{e9}\u{6f22}\u{1f600}b".repeat(3);
        let bytes = text.as_bytes();
        for size in 1..bytes.len() {
            let mut scanner = Utf8Scanner::default();
            for piece in bytes.chunks(size) {
                assert!(scanner.feed(piece), "piece size {size}");
            }
            assert!(scanner.finish(), "piece size {size}");
        }
        let mut scanner = Utf8Scanner::default();
        assert!(scanner.feed(&[0xe6, 0xbc]));
        assert!(!scanner.finish());
        assert!(!scanner.feed(b"a"));
    }

    #[test]
    fn standard_euc_jp_form() {
        assert!(is_standard_euc_jp(&[b'a', 0xb4, 0xc1, 0x8e, 0xb1]));
        assert!(is_standard_euc_jp(&[0x8f, 0xb0, 0xa1]));
        assert!(!is_standard_euc_jp(&[0xb0, 0x21]));
        assert!(!is_standard_euc_jp(&[0x8f, 0xb0]));
        assert!(!is_standard_euc_jp(&[0x80]));
    }
}
