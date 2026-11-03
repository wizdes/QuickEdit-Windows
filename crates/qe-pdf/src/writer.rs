//! Writes PDF objects to the temp file as they are made, so that memory does not grow with
//! the document, and checks the finished file's structure from disk.
use crate::deflate::Compressor;
use crate::{PdfError, PdfOptions};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_PATH_NOT_FOUND,
    GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetFileSecurityW, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
    SECURITY_ATTRIBUTES, SetSecurityDescriptorControl,
};
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE,
};
use windows::core::PCWSTR;

pub const CATALOG_ID: usize = 1;
pub const PAGES_ID: usize = 2;
pub const RESOURCES_ID: usize = 3;
pub const INFO_ID: usize = 4;
const FIRST_FREE_ID: usize = 5;

fn problem(error: std::io::Error) -> PdfError {
    PdfError::Write(format!("The PDF could not be written: {error}"))
}

/// The `\\?\` form, which raw calls need for paths over 260 characters.
fn raw_call_path(path: &Path) -> Result<Vec<u16>, PdfError> {
    let full = std::path::absolute(path).map_err(problem)?;
    let units: Vec<u16> = full.as_os_str().encode_wide().collect();
    let text = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let mut out = if units.starts_with(&text(r"\\?\")) {
        units
    } else if units.starts_with(&text(r"\\")) {
        let mut out = text(r"\\?\UNC\");
        out.extend_from_slice(&units[2..]);
        out
    } else {
        let mut out = text(r"\\?\");
        out.extend_from_slice(&units);
        out
    };
    out.push(0);
    Ok(out)
}

/// A file's access rules, ready to give to a new one, as `qe-codec`'s save does. The rules are
/// marked protected, so that Windows keeps them as they are and adds none from the folder.
pub struct AccessRules {
    // u64 gives the alignment a security descriptor needs.
    descriptor: Vec<u64>,
}

/// The rules of the file this PDF will replace. `None` when there is no such file, or on a file
/// system with no rules, such as exFAT. Rules that cannot be read stop the export, since a new
/// file could then be open to more people than the old one.
pub fn read_access_rules(path: &Path) -> Result<Option<AccessRules>, PdfError> {
    let name = raw_call_path(path)?;
    let refuse = |code: u32| {
        PdfError::Write(format!(
            "The PDF was not saved, because the access rules of {} could not be read: {}",
            path.display(),
            std::io::Error::from_raw_os_error(code as i32)
        ))
    };
    let mut needed = 0u32;
    // SAFETY: the path ends in a zero and outlives the call. With no buffer, the call only
    // reports the size.
    let done = unsafe {
        GetFileSecurityW(
            PCWSTR(name.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            None,
            0,
            &mut needed,
        )
    };
    if !done.as_bool() && needed == 0 {
        // SAFETY: the call takes no arguments.
        let error = unsafe { GetLastError() };
        return match error {
            ERROR_FILE_NOT_FOUND
            | ERROR_PATH_NOT_FOUND
            | ERROR_NOT_SUPPORTED
            | ERROR_INVALID_FUNCTION => Ok(None),
            other => Err(refuse(other.0)),
        };
    }
    let mut rules = AccessRules {
        descriptor: vec![0u64; (needed as usize).div_ceil(8).max(1)],
    };
    let pointer = PSECURITY_DESCRIPTOR(rules.descriptor.as_mut_ptr().cast());
    // SAFETY: as above, and `descriptor` holds at least `needed` bytes.
    let done = unsafe {
        GetFileSecurityW(
            PCWSTR(name.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            Some(pointer),
            needed,
            &mut needed,
        )
    };
    if !done.as_bool() {
        // SAFETY: the call takes no arguments.
        return Err(refuse(unsafe { GetLastError() }.0));
    }
    // SAFETY: `pointer` holds the valid descriptor that the call just wrote.
    unsafe { SetSecurityDescriptorControl(pointer, SE_DACL_PROTECTED, SE_DACL_PROTECTED) }
        .map_err(|e| refuse((e.code().0 as u32) & 0xffff))?;
    Ok(Some(rules))
}

/// A new file with the given rules in place from its first moment. It must not exist yet.
fn create_new_with_rules(path: &Path, rules: Option<&mut AccessRules>) -> Result<File, PdfError> {
    let name = raw_call_path(path)?;
    let attributes = rules.map(|rules| SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: rules.descriptor.as_mut_ptr().cast(),
        bInheritHandle: false.into(),
    });
    // SAFETY: the path ends in a zero, and `attributes` and its descriptor outlive the call.
    let handle: HANDLE = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            // The same sharing as `std::fs`, which this call replaced.
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            attributes.as_ref().map(|a| a as *const SECURITY_ATTRIBUTES),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|e| problem(std::io::Error::from_raw_os_error(e.code().0 & 0xffff)))?;
    // SAFETY: the handle was just opened for this file, and nothing else owns it.
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

pub struct PdfFile {
    out: BufWriter<File>,
    at: u64,
    /// Where each object starts, by id minus one. Zero means not written yet.
    offsets: Vec<u64>,
    compressor: Compressor,
}

impl PdfFile {
    pub fn create(path: &Path, rules: Option<&mut AccessRules>) -> Result<Self, PdfError> {
        let file = create_new_with_rules(path, rules)?;
        let mut pdf = Self {
            out: BufWriter::with_capacity(1 << 20, file),
            at: 0,
            offsets: vec![0; FIRST_FREE_ID - 1],
            compressor: Compressor::default(),
        };
        // The comment's high bytes tell a reader that this file holds binary data.
        pdf.raw(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n")?;
        Ok(pdf)
    }
    fn raw(&mut self, bytes: &[u8]) -> Result<(), PdfError> {
        self.out.write_all(bytes).map_err(problem)?;
        self.at += bytes.len() as u64;
        Ok(())
    }
    pub fn new_id(&mut self) -> usize {
        self.offsets.push(0);
        self.offsets.len()
    }
    pub fn object_count(&self) -> usize {
        self.offsets.len()
    }
    /// The bytes written so far.
    pub fn bytes_written(&self) -> u64 {
        self.at
    }
    pub fn object(&mut self, id: usize, body: &[u8]) -> Result<(), PdfError> {
        self.mark(id)?;
        self.raw(format!("{id} 0 obj\n").as_bytes())?;
        self.raw(body)?;
        self.raw(b"\nendobj\n")
    }
    fn mark(&mut self, id: usize) -> Result<(), PdfError> {
        let slot = id
            .checked_sub(1)
            .and_then(|index| self.offsets.get_mut(index))
            .ok_or_else(|| PdfError::Check(format!("object {id} was never made")))?;
        *slot = self.at;
        Ok(())
    }
    /// A compressed stream object. `dict` holds any keys other than the length and filter.
    pub fn stream(&mut self, id: usize, dict: &str, data: &[u8]) -> Result<(), PdfError> {
        let packed = self.compressor.compress(data);
        self.mark(id)?;
        self.raw(
            format!(
                "{id} 0 obj\n<< {dict} /Filter /FlateDecode /Length {} >>\nstream\n",
                packed.len()
            )
            .as_bytes(),
        )?;
        self.raw(&packed)?;
        self.raw(b"\nendstream\nendobj\n")
    }
    /// Writes the file's end and flushes it to disk. Returns its size.
    pub fn finish(&mut self, options: &PdfOptions, _pages: usize) -> Result<u64, PdfError> {
        let mut info = String::from("<< /Creator (QuickEdit)");
        // The Mac names the file's title only when it prints the header.
        if options.header_and_footer {
            info.push_str(" /Title <FEFF");
            for unit in options.title.encode_utf16() {
                info.push_str(&format!("{unit:04X}"));
            }
            info.push('>');
        }
        info.push_str(" >>");
        self.object(INFO_ID, info.as_bytes())?;
        if let Some(missing) = self.offsets.iter().position(|offset| *offset == 0) {
            return Err(PdfError::Check(format!(
                "object {} was never written",
                missing + 1
            )));
        }
        let xref = self.at;
        let count = self.offsets.len() + 1;
        self.raw(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes())?;
        let offsets = std::mem::take(&mut self.offsets);
        for offset in &offsets {
            self.raw(format!("{offset:010} 00000 n \n").as_bytes())?;
        }
        self.raw(
            format!(
                "trailer\n<< /Size {count} /Root {CATALOG_ID} 0 R /Info {INFO_ID} 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        )?;
        self.out.flush().map_err(problem)?;
        self.out.get_ref().sync_all().map_err(problem)?;
        Ok(self.at)
    }
}

fn read_at(file: &mut File, at: u64, buffer: &mut [u8]) -> Result<usize, PdfError> {
    file.seek(SeekFrom::Start(at)).map_err(problem)?;
    let mut filled = 0;
    while filled < buffer.len() {
        let got = file.read(&mut buffer[filled..]).map_err(problem)?;
        if got == 0 {
            break;
        }
        filled += got;
    }
    Ok(filled)
}

/// The check before a temp file becomes the PDF: it ends properly, the table of objects
/// points at every object, and the page tree has the pages that were written. It reads
/// a small piece per object, so it does not hold the file in memory.
pub fn verify_on_disk(path: &Path, pages: usize, cancel: &AtomicBool) -> Result<(), PdfError> {
    let mut file = File::open(path).map_err(problem)?;
    let size = file.metadata().map_err(problem)?.len();
    let bad = |what: &str| PdfError::Check(what.to_string());
    let mut tail = [0u8; 128];
    let start = size.saturating_sub(tail.len() as u64);
    let got = read_at(&mut file, start, &mut tail)?;
    let tail = String::from_utf8_lossy(&tail[..got]).into_owned();
    if !tail.ends_with("%%EOF\n") {
        return Err(bad("its end is missing"));
    }
    let xref: u64 = tail
        .rsplit("startxref\n")
        .next()
        .and_then(|rest| rest.lines().next())
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| bad("its table of objects cannot be found"))?;
    let mut head = [0u8; 32];
    let got = read_at(&mut file, xref, &mut head)?;
    let head_text = String::from_utf8_lossy(&head[..got]).into_owned();
    let mut lines = head_text.lines();
    if lines.next() != Some("xref") {
        return Err(bad("its table of objects is in the wrong place"));
    }
    let count_line = lines.next().ok_or_else(|| bad("its table has no size"))?;
    let count: usize = count_line
        .split(' ')
        .nth(1)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| bad("its table has no size"))?;
    let table = xref + 5 + count_line.len() as u64 + 1;
    for id in 1..count {
        // Each entry is two disk reads, and a big file has millions.
        if id.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
            return Err(PdfError::Cancelled);
        }
        let mut entry = [0u8; 20];
        read_at(&mut file, table + id as u64 * 20, &mut entry)?;
        let offset: u64 = std::str::from_utf8(&entry[..10])
            .ok()
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| bad("an entry of its table is bad"))?;
        let want = format!("{id} 0 obj\n");
        let mut found = vec![0u8; want.len()];
        read_at(&mut file, offset, &mut found)?;
        if found != want.as_bytes() {
            return Err(PdfError::Check(format!(
                "object {id} is not where it should be"
            )));
        }
    }
    // Object 2 is the page tree, and /Count comes first in it.
    let mut entry = [0u8; 10];
    read_at(&mut file, table + PAGES_ID as u64 * 20, &mut entry)?;
    let offset: u64 = std::str::from_utf8(&entry)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| bad("an entry of its table is bad"))?;
    let mut tree = [0u8; 64];
    let got = read_at(&mut file, offset, &mut tree)?;
    let want = format!("/Count {pages} ");
    if !String::from_utf8_lossy(&tree[..got]).contains(&want) {
        return Err(bad("it does not have the pages that were written"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small whole file, as `verify_on_disk` expects, with one page.
    fn good_file(path: &Path) {
        let mut file = PdfFile::create(path, None).unwrap();
        file.object(RESOURCES_ID, b"<< >>").unwrap();
        file.object(CATALOG_ID, b"<< >>").unwrap();
        file.object(PAGES_ID, b"<< /Count 1 /Type /Pages /Kids [] >>")
            .unwrap();
        file.finish(&PdfOptions::default(), 1).unwrap();
    }

    #[test]
    fn the_check_refuses_each_kind_of_damage() {
        let path = std::env::temp_dir().join(format!("qe-pdf-verify-{}.tmp", std::process::id()));
        let cancel = AtomicBool::new(false);
        good_file(&path);
        verify_on_disk(&path, 1, &cancel).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let find = |what: &[u8]| {
            bytes
                .windows(what.len())
                .rposition(|window| window == what)
                .unwrap()
        };
        let table = find(b"\nxref\n") + 1;
        let mut moved_entry = bytes.clone();
        moved_entry[table + 5 + 4 + 20 + 9] ^= 1;
        let mut moved_object = bytes.clone();
        moved_object[find(b"\n1 0 obj") + 1] = b'9';
        let mut no_table = bytes.clone();
        no_table[table] = b'y';
        let cut_end = bytes[..bytes.len() - 2].to_vec();
        for (what, damaged, pages) in [
            ("an entry that points elsewhere", moved_entry, 1),
            ("an object that is not its number", moved_object, 1),
            ("a table that has moved", no_table, 1),
            ("a cut end", cut_end, 1),
            ("the wrong page count", bytes.clone(), 2),
        ] {
            std::fs::write(&path, &damaged).unwrap();
            let result = verify_on_disk(&path, pages, &cancel);
            assert!(
                matches!(result, Err(PdfError::Check(_))),
                "{what}: {result:?}"
            );
        }
        std::fs::remove_file(&path).unwrap();
    }
}
