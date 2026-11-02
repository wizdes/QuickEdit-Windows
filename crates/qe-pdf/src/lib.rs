//! A small, worker-only PDF writer. It currently supports ASCII text.
use qe_api::{Dip, Layout, LineIndex, TextSnapshot};
use qe_layout::DirectWriteLayout;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

pub const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;
static TEMP_NUMBER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct PdfOptions {
    pub paper_width: f32,
    pub paper_height: f32,
    pub margin: f32,
    pub font_family: String,
    pub font_points: f32,
    pub header_and_footer: bool,
    pub rewrap: bool,
    pub window_width: f32,
    pub title: String,
}
impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            paper_width: 612.0,
            paper_height: 792.0,
            margin: 72.0,
            font_family: "Cascadia Mono".into(),
            font_points: 10.0,
            header_and_footer: false,
            rewrap: true,
            window_width: 468.0,
            title: String::new(),
        }
    }
}

fn escape_pdf(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}' => {}
            _ => out.push(ch),
        }
    }
    out
}
fn object(bytes: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &str) {
    let id = offsets.len() + 1;
    offsets.push(bytes.len());
    bytes.extend_from_slice(format!("{id} 0 obj\n{body}\nendobj\n").as_bytes());
}
fn stream(content: &str) -> String {
    format!(
        "<< /Length {} >>\nstream\n{}\nendstream",
        content.len() + 1,
        content
    )
}

/// Runs entirely on a worker. The 8 MiB limit bounds PDF memory and page count.
pub fn build_pdf(
    source: Arc<dyn TextSnapshot>,
    options: &PdfOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize),
) -> Result<Vec<u8>, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("PDF export was cancelled".into());
    }
    if source.len_bytes().0 > MAX_INPUT_BYTES {
        return Err("PDF export is limited to 8 MiB of text".into());
    }
    if !options.paper_width.is_finite()
        || !options.paper_height.is_finite()
        || !options.margin.is_finite()
        || options.paper_width <= 2.0 * options.margin
        || options.paper_height <= 2.0 * options.margin
    {
        return Err("PDF paper or margins are invalid".into());
    }
    let engine = DirectWriteLayout::new().map_err(|e| e.to_string())?;
    let width = if options.rewrap {
        options.paper_width - 2.0 * options.margin
    } else {
        options.window_width
    };
    let mut pages = Vec::<String>::new();
    let mut content = String::new();
    let mut y = options.paper_height - options.margin;
    let lines = source.line_count().max(1);
    if lines > 100_000 {
        return Err("PDF export is limited to 100,000 lines".into());
    }
    for line in 0..lines {
        if cancel.load(Ordering::Relaxed) {
            return Err("PDF export was cancelled".into());
        }
        let layout = engine
            .layout_line(source.as_ref(), LineIndex(line), Dip(width))
            .ok_or_else(|| format!("Line {} could not be laid out", line + 1))?;
        if layout.partial {
            return Err(format!("Line {} is too long for PDF export", line + 1));
        }
        for row in &layout.rows {
            let row_height = row.height.0 * 72.0 / 96.0;
            if y - row_height < options.margin && !content.is_empty() {
                pages.push(std::mem::take(&mut content));
                y = options.paper_height - options.margin;
            }
            let mut text = String::new();
            source.visit_chunks(row.range.clone(), &mut |part| text.push_str(part));
            if !text.is_ascii()
                || text
                    .bytes()
                    .any(|b| b == b'\t' || (b < 32 && b != b'\r' && b != b'\n'))
            {
                return Err(format!(
                    "Line {} uses a character this PDF writer cannot embed yet",
                    line + 1
                ));
            }
            let at = y - row_height;
            content.push_str(&format!(
                "BT /F1 {} Tf 1 0 0 1 {} {} Tm ({}) Tj ET\n",
                options.font_points,
                options.margin,
                at,
                escape_pdf(&text)
            ));
            y = at;
        }
        progress(line + 1);
    }
    pages.push(content);
    if cancel.load(Ordering::Relaxed) {
        return Err("PDF export was cancelled".into());
    }
    let mut bytes = b"%PDF-1.7\n%QuickEdit\n".to_vec();
    let mut offsets = Vec::new();
    object(
        &mut bytes,
        &mut offsets,
        "<< /Type /Catalog /Pages 2 0 R >>",
    );
    let kids = (0..pages.len())
        .map(|n| format!("{} 0 R", 5 + n * 2))
        .collect::<Vec<_>>()
        .join(" ");
    object(
        &mut bytes,
        &mut offsets,
        &format!("<< /Type /Pages /Count {} /Kids [{}] >>", pages.len(), kids),
    );
    // ASCII CMap makes extracted text searchable and selectable.
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /QuickEdit-ASCII def\n/CMapType 2 def\n1 begincodespacerange\n<00> <7F>\nendcodespacerange\n95 beginbfchar\n",
    );
    for c in 32..=126 {
        cmap.push_str(&format!("<{c:02X}> <{c:04X}>\n"));
    }
    cmap.push_str("endbfchar\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend");
    object(
        &mut bytes,
        &mut offsets,
        &format!(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding /ToUnicode 4 0 R >>"
        ),
    );
    object(&mut bytes, &mut offsets, &stream(&cmap));
    for (n, page) in pages.iter().enumerate() {
        let page_id = 5 + n * 2;
        let stream_id = page_id + 1;
        object(
            &mut bytes,
            &mut offsets,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 3 0 R >> >> /Contents {stream_id} 0 R >>",
                options.paper_width, options.paper_height
            ),
        );
        object(&mut bytes, &mut offsets, &stream(page));
    }
    let xref = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
    );
    for off in &offsets {
        bytes.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    check_pdf(&bytes)?;
    Ok(bytes)
}

/// A small structure check; a reader is still needed to inspect appearance.
pub fn check_pdf(bytes: &[u8]) -> Result<usize, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "PDF has non-UTF-8 bytes".to_string())?;
    if !text.starts_with("%PDF-") || !text.ends_with("%%EOF\n") {
        return Err("PDF header or end is missing".into());
    }
    let xref = text
        .rfind("\nxref\n")
        .ok_or("PDF cross-reference table is missing")?
        + 1;
    let start = text
        .rfind("startxref\n")
        .ok_or("PDF startxref is missing")?;
    let value = text
        .get(start + 10..)
        .and_then(|s| s.lines().next())
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or("PDF startxref is invalid")?;
    if value != xref {
        return Err("PDF cross-reference offset is wrong".into());
    }
    let table = text
        .get(xref + 5..)
        .ok_or("PDF cross-reference table is cut off")?;
    let mut lines = table.lines();
    let count_line = lines.next().ok_or("PDF cross-reference count is missing")?;
    let count = count_line
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse::<usize>().ok())
        .ok_or("PDF cross-reference count is invalid")?;
    if count < 6 || lines.next() != Some("0000000000 65535 f ") {
        return Err("PDF free object is invalid".into());
    }
    for id in 1..count {
        let entry = lines.next().ok_or("PDF cross-reference entry is missing")?;
        let offset = entry
            .get(..10)
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or("PDF cross-reference entry is invalid")?;
        let expected = format!("{id} 0 obj\n");
        if !bytes
            .get(offset..)
            .is_some_and(|tail| tail.starts_with(expected.as_bytes()))
        {
            return Err(format!("PDF object {id} has a wrong offset"));
        }
    }
    let pages = text.matches("/Type /Page /Parent").count();
    if pages == 0 || !text.contains("/ToUnicode 4 0 R") {
        return Err("PDF page or ToUnicode map is missing".into());
    }
    Ok(pages)
}

/// Spawn this from the UI thread with `worker_snapshot()`. Progress runs on the worker.
pub fn export_worker(
    source: Arc<dyn TextSnapshot>,
    options: PdfOptions,
    path: PathBuf,
    cancel: Arc<AtomicBool>,
    progress: impl Fn(usize) + Send + 'static,
) -> std::thread::JoinHandle<Result<usize, String>> {
    std::thread::spawn(move || {
        let bytes = build_pdf(source, &options, &cancel, progress)?;
        if cancel.load(Ordering::Relaxed) {
            return Err("PDF export was cancelled".into());
        }
        write_atomic(&path, &bytes, &cancel)?;
        check_pdf(&bytes)
    })
}
fn write_atomic(path: &Path, bytes: &[u8], cancel: &AtomicBool) -> Result<(), String> {
    let parent = path.parent().ok_or("PDF path has no parent folder")?;
    let name = path
        .file_name()
        .ok_or("PDF path has no file name")?
        .to_string_lossy();
    let id = TEMP_NUMBER.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".{name}.qe-{}-{id}.tmp", std::process::id()));
    let mut created = false;
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        created = true;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        if cancel.load(Ordering::Relaxed) {
            return Err("PDF export was cancelled".into());
        }
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(&temp);
    }
    result
}
