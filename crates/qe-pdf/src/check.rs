//! A strict structure checker for the PDFs this crate writes. It trusts nothing it has not
//! parsed: it follows the file from `startxref`, reads every object through the table,
//! walks the page tree, parses each font's CFF program and ToUnicode map, and interprets
//! each page's text commands. Tests use it, and so can a host's diagnostics. Bad input
//! gives an error, never a panic.
use crate::deflate::inflate;
use std::collections::HashMap;

/// A parsed PDF value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    Name(String),
    Text(Vec<u8>),
    Array(Vec<Value>),
    Dict(HashMap<String, Value>),
    Ref(usize),
}
impl Value {
    fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(dict) => dict.get(key),
            _ => None,
        }
    }
    fn number(&self) -> Option<f64> {
        match self {
            Value::Number(value) => Some(*value),
            _ => None,
        }
    }
    fn name(&self) -> Option<&str> {
        match self {
            Value::Name(value) => Some(value),
            _ => None,
        }
    }
    fn reference(&self) -> Option<usize> {
        match self {
            Value::Ref(id) => Some(*id),
            _ => None,
        }
    }
}

struct Parser<'a> {
    data: &'a [u8],
    at: usize,
}
impl Parser<'_> {
    fn skip_space(&mut self) {
        while let Some(&byte) = self.data.get(self.at) {
            if matches!(byte, b' ' | b'\n' | b'\r' | b'\t' | 0x0C | 0) {
                self.at += 1;
            } else if byte == b'%' {
                while self.data.get(self.at).is_some_and(|b| *b != b'\n') {
                    self.at += 1;
                }
            } else {
                break;
            }
        }
    }
    fn word(&mut self) -> &str {
        let start = self.at;
        while self.data.get(self.at).is_some_and(|byte| {
            !matches!(
                byte,
                b' ' | b'\n' | b'\r' | b'\t' | b'/' | b'<' | b'>' | b'[' | b']' | b'(' | b')'
            )
        }) {
            self.at += 1;
        }
        std::str::from_utf8(&self.data[start..self.at]).unwrap_or("")
    }
    fn value(&mut self) -> Result<Value, String> {
        self.skip_space();
        let byte = *self
            .data
            .get(self.at)
            .ok_or("the file ended inside a value")?;
        match byte {
            b'/' => {
                self.at += 1;
                Ok(Value::Name(self.word().to_string()))
            }
            b'<' if self.data.get(self.at + 1) == Some(&b'<') => {
                self.at += 2;
                let mut dict = HashMap::new();
                loop {
                    self.skip_space();
                    if self.data.get(self.at..self.at + 2) == Some(b">>") {
                        self.at += 2;
                        return Ok(Value::Dict(dict));
                    }
                    let key = self.value()?;
                    let key = key
                        .name()
                        .ok_or("a dictionary key is not a name")?
                        .to_string();
                    let value = self.value()?;
                    dict.insert(key, value);
                }
            }
            b'<' => {
                self.at += 1;
                let mut digits = Vec::new();
                loop {
                    let byte = *self.data.get(self.at).ok_or("a hex string has no end")?;
                    self.at += 1;
                    if byte == b'>' {
                        break;
                    }
                    if !byte.is_ascii_whitespace() {
                        digits.push(byte);
                    }
                }
                if digits.len() % 2 == 1 {
                    digits.push(b'0');
                }
                let mut text = Vec::with_capacity(digits.len() / 2);
                for pair in digits.chunks(2) {
                    let part = std::str::from_utf8(pair).map_err(|_| "bad hex")?;
                    text.push(u8::from_str_radix(part, 16).map_err(|_| "bad hex")?);
                }
                Ok(Value::Text(text))
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    if self.data.get(self.at) == Some(&b']') {
                        self.at += 1;
                        return Ok(Value::Array(items));
                    }
                    items.push(self.value()?);
                }
            }
            b'(' => {
                self.at += 1;
                let mut depth = 1;
                let mut text = Vec::new();
                while depth > 0 {
                    let byte = *self.data.get(self.at).ok_or("a string has no end")?;
                    self.at += 1;
                    match byte {
                        b'\\' => {
                            text.push(*self.data.get(self.at).ok_or("a string has no end")?);
                            self.at += 1;
                        }
                        b'(' => {
                            depth += 1;
                            text.push(byte);
                        }
                        b')' => {
                            depth -= 1;
                            if depth > 0 {
                                text.push(byte);
                            }
                        }
                        _ => text.push(byte),
                    }
                }
                Ok(Value::Text(text))
            }
            _ => {
                let word = self.word().to_string();
                if word.is_empty() {
                    return Err(format!("unexpected byte {byte} at {}", self.at));
                }
                match word.as_str() {
                    "true" => return Ok(Value::Bool(true)),
                    "false" => return Ok(Value::Bool(false)),
                    "null" => return Ok(Value::Null),
                    _ => {}
                }
                let first: f64 = word
                    .parse()
                    .map_err(|_| format!("{word:?} is not a PDF value"))?;
                // "12 0 R" is a reference.
                let saved = self.at;
                self.skip_space();
                let second = self.word().to_string();
                self.skip_space();
                let third = self.word().to_string();
                if third == "R" && second.parse::<u32>().is_ok() {
                    return Ok(Value::Ref(first as usize));
                }
                self.at = saved;
                Ok(Value::Number(first))
            }
        }
    }
}

/// What the page's text commands drew: one glyph.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedGlyph {
    pub font: String,
    pub cid: u16,
    pub x: f64,
    pub y: f64,
    pub size: f64,
    pub invisible: bool,
    /// The glyph's text from the ToUnicode map, or empty when it has none.
    pub text: String,
}

/// One page as the checker read it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageReport {
    pub media_box: [f64; 4],
    pub glyphs: Vec<PlacedGlyph>,
    pub form_calls: usize,
}

/// One font as the checker read it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FontReport {
    pub base_font: String,
    /// Charstrings in the embedded program, `.notdef` included.
    pub glyph_count: usize,
    pub program_bytes: usize,
    pub charstrings: Vec<Vec<u8>>,
    pub widths: HashMap<u16, f64>,
    pub to_unicode: HashMap<u16, String>,
    /// True when the program is CFF with the Adobe Identity ROS.
    pub is_cid_cff: bool,
}

/// What the whole file holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub objects: usize,
    pub pages: Vec<PageReport>,
    pub fonts: HashMap<String, FontReport>,
    pub creator: String,
    pub title: Option<String>,
}

impl Report {
    /// Each page's text, row by row from the top. In a row, glyphs go from left to right,
    /// which for right-to-left text is the order on screen and not the order in the store.
    pub fn page_rows(&self, page: usize) -> Vec<String> {
        let Some(report) = self.pages.get(page) else {
            return Vec::new();
        };
        let mut glyphs: Vec<&PlacedGlyph> = report.glyphs.iter().collect();
        glyphs.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));
        let mut rows: Vec<(f64, String)> = Vec::new();
        for glyph in glyphs {
            match rows.last_mut() {
                Some((y, row)) if (*y - glyph.y).abs() < 0.01 => row.push_str(&glyph.text),
                _ => rows.push((glyph.y, glyph.text.clone())),
            }
        }
        rows.into_iter().map(|(_, row)| row).collect()
    }
    /// All the text in the order the file draws it, which is the store's order in each run.
    pub fn text_in_file_order(&self) -> String {
        self.pages
            .iter()
            .flat_map(|page| page.glyphs.iter().map(|glyph| glyph.text.as_str()))
            .collect()
    }
}

struct File<'a> {
    bytes: &'a [u8],
    offsets: Vec<usize>,
}

impl File<'_> {
    /// The object's value, and its stream's raw bytes when it has one.
    fn object(&self, id: usize) -> Result<(Value, Option<&[u8]>), String> {
        let offset = *self
            .offsets
            .get(id)
            .ok_or_else(|| format!("object {id} is not in the table"))?;
        let mut parser = Parser {
            data: self.bytes,
            at: offset,
        };
        let header = format!("{id} 0 obj");
        if self.bytes.get(offset..offset + header.len()) != Some(header.as_bytes()) {
            return Err(format!("object {id} is not at its offset {offset}"));
        }
        parser.at += header.len();
        let value = parser.value()?;
        parser.skip_space();
        if self.bytes.get(parser.at..parser.at + 6) == Some(b"stream") {
            let start = parser.at + 7;
            let length = value
                .get("Length")
                .and_then(Value::number)
                .ok_or_else(|| format!("stream {id} has no /Length"))?
                as usize;
            let data = self
                .bytes
                .get(start..start + length)
                .ok_or_else(|| format!("stream {id} runs past the file"))?;
            let end = start + length;
            if self.bytes.get(end..end + 10) != Some(b"\nendstream") {
                return Err(format!("stream {id} has a wrong /Length"));
            }
            Ok((value, Some(data)))
        } else {
            if self.bytes.get(parser.at..parser.at + 6) != Some(b"endobj") {
                return Err(format!("object {id} has no endobj"));
            }
            Ok((value, None))
        }
    }
    fn stream(&self, id: usize) -> Result<Vec<u8>, String> {
        let (value, data) = self.object(id)?;
        let data = data.ok_or_else(|| format!("object {id} is not a stream"))?;
        match value.get("Filter").and_then(Value::name) {
            Some("FlateDecode") => inflate(data).map_err(|e| format!("stream {id}: {e}")),
            Some(other) => Err(format!("stream {id} uses the unknown filter {other}")),
            None => Ok(data.to_vec()),
        }
    }
    fn resolve(&self, value: &Value) -> Result<Value, String> {
        match value {
            Value::Ref(id) => Ok(self.object(*id)?.0),
            other => Ok(other.clone()),
        }
    }
}

/// Parses and checks the file. An `Err` names the first thing wrong.
pub fn check_pdf(bytes: &[u8]) -> Result<Report, String> {
    if !bytes.starts_with(b"%PDF-1.") {
        return Err("the file does not start with %PDF-1.".into());
    }
    if !bytes.ends_with(b"%%EOF\n") {
        return Err("the file does not end with %%EOF".into());
    }
    let tail_start = bytes.len().saturating_sub(64);
    let tail = String::from_utf8_lossy(&bytes[tail_start..]).into_owned();
    let xref: usize = tail
        .rsplit("startxref\n")
        .next()
        .and_then(|rest| rest.lines().next())
        .and_then(|value| value.parse().ok())
        .ok_or("startxref is missing")?;
    if bytes.get(xref..xref + 5) != Some(b"xref\n") {
        return Err(format!("startxref {xref} does not point at the table"));
    }
    let mut parser = Parser {
        data: bytes,
        at: xref + 5,
    };
    let first: usize = parser
        .word()
        .parse()
        .map_err(|_| "the table has no start")?;
    parser.skip_space();
    let count: usize = parser.word().parse().map_err(|_| "the table has no size")?;
    parser.skip_space();
    if first != 0 || count < 2 {
        return Err("the table's first line is wrong".into());
    }
    let table = parser.at;
    if bytes.get(table..table + 20) != Some(b"0000000000 65535 f \n") {
        return Err("the table's free entry is wrong".into());
    }
    let mut offsets = vec![0usize; count];
    for (id, slot) in offsets.iter_mut().enumerate().skip(1) {
        let entry = bytes
            .get(table + id * 20..table + id * 20 + 20)
            .ok_or("the table is cut off")?;
        let text = std::str::from_utf8(entry).map_err(|_| "a table entry is not text")?;
        if !text.ends_with(" 00000 n \n") {
            return Err(format!("the entry for object {id} is wrong: {text:?}"));
        }
        *slot = text[..10].parse().map_err(|_| "a table offset is bad")?;
    }
    let mut parser = Parser {
        data: bytes,
        at: table + count * 20,
    };
    if parser.word() != "trailer" {
        return Err("the trailer is not after the table".into());
    }
    let trailer = parser.value()?;
    if trailer.get("Size").and_then(Value::number) != Some(count as f64) {
        return Err("the trailer's /Size does not match the table".into());
    }
    let file = File { bytes, offsets };
    // Every object must parse where the table says it is.
    for id in 1..count {
        file.object(id)?;
    }
    let root = trailer
        .get("Root")
        .and_then(Value::reference)
        .ok_or("the trailer has no /Root")?;
    let catalog = file.object(root)?.0;
    if catalog.get("Type").and_then(Value::name) != Some("Catalog") {
        return Err("/Root is not a catalog".into());
    }
    let pages_id = catalog
        .get("Pages")
        .and_then(Value::reference)
        .ok_or("the catalog has no /Pages")?;
    let tree = file.object(pages_id)?.0;
    let declared = tree
        .get("Count")
        .and_then(Value::number)
        .ok_or("the page tree has no /Count")? as usize;
    let Some(Value::Array(kids)) = tree.get("Kids") else {
        return Err("the page tree has no /Kids".into());
    };
    if kids.len() != declared {
        return Err(format!(
            "/Count says {declared} and /Kids has {}",
            kids.len()
        ));
    }
    let mut report = Report {
        objects: count - 1,
        ..Report::default()
    };
    let mut fonts: HashMap<usize, (String, FontReport)> = HashMap::new();
    for kid in kids {
        let id = kid.reference().ok_or("a page is not a reference")?;
        let page = file.object(id)?.0;
        if page.get("Type").and_then(Value::name) != Some("Page") {
            return Err(format!("object {id} is not a page"));
        }
        if page.get("Parent").and_then(Value::reference) != Some(pages_id) {
            return Err(format!("page {id} has the wrong parent"));
        }
        report.pages.push(read_page(&file, &page, &mut fonts)?);
    }
    for (_, (name, font)) in fonts {
        report.fonts.insert(name, font);
    }
    if let Some(info) = trailer.get("Info") {
        let info = file.resolve(info)?;
        if let Some(Value::Text(creator)) = info.get("Creator") {
            report.creator = String::from_utf8_lossy(creator).into_owned();
        }
        if let Some(Value::Text(title)) = info.get("Title") {
            let units: Vec<u16> = title
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair))
                .skip(1)
                .collect();
            report.title = Some(String::from_utf16_lossy(&units));
        }
    }
    Ok(report)
}

fn read_page(
    file: &File<'_>,
    page: &Value,
    fonts: &mut HashMap<usize, (String, FontReport)>,
) -> Result<PageReport, String> {
    let mut out = PageReport::default();
    let Some(Value::Array(media)) = page.get("MediaBox") else {
        return Err("a page has no /MediaBox".into());
    };
    for (slot, value) in out.media_box.iter_mut().zip(media) {
        *slot = value.number().ok_or("a /MediaBox value is not a number")?;
    }
    let resources = file.resolve(page.get("Resources").ok_or("a page has no /Resources")?)?;
    let font_refs = match resources.get("Font") {
        Some(found) => file.resolve(found)?,
        None => Value::Dict(HashMap::new()),
    };
    let ids: Vec<usize> = match page.get("Contents") {
        Some(Value::Ref(id)) => vec![*id],
        Some(Value::Array(items)) => items.iter().filter_map(Value::reference).collect(),
        _ => return Err("a page has no /Contents".into()),
    };
    for id in ids {
        let content = file.stream(id)?;
        read_content(file, &content, &font_refs, fonts, &mut out, &resources)?;
    }
    Ok(out)
}

fn load_font(
    file: &File<'_>,
    id: usize,
    fonts: &mut HashMap<usize, (String, FontReport)>,
) -> Result<(), String> {
    if fonts.contains_key(&id) {
        return Ok(());
    }
    let font = file.object(id)?.0;
    if font.get("Subtype").and_then(Value::name) != Some("Type0")
        || font.get("Encoding").and_then(Value::name) != Some("Identity-H")
    {
        return Err(format!("font object {id} is not Type0 with Identity-H"));
    }
    let base = font
        .get("BaseFont")
        .and_then(Value::name)
        .ok_or("a font has no /BaseFont")?
        .to_string();
    let Some(Value::Array(descendants)) = font.get("DescendantFonts") else {
        return Err(format!("font {base} has no /DescendantFonts"));
    };
    let descendant = file.resolve(descendants.first().ok_or("an empty /DescendantFonts")?)?;
    if descendant.get("Subtype").and_then(Value::name) != Some("CIDFontType0") {
        return Err(format!("font {base} is not CIDFontType0"));
    }
    let descriptor = file.resolve(
        descendant
            .get("FontDescriptor")
            .ok_or_else(|| format!("font {base} has no descriptor"))?,
    )?;
    if descriptor.get("FontName").and_then(Value::name) != Some(&base) {
        return Err(format!("font {base} has a descriptor of another name"));
    }
    let program_id = descriptor
        .get("FontFile3")
        .and_then(Value::reference)
        .ok_or_else(|| format!("font {base} is not embedded"))?;
    let program = file.stream(program_id)?;
    let mut report = read_cff(&program).map_err(|e| format!("font {base}: {e}"))?;
    report.base_font = base.clone();
    report.program_bytes = program.len();
    let Some(Value::Array(widths)) = descendant.get("W") else {
        return Err(format!("font {base} has no /W"));
    };
    let mut parts = widths.iter();
    while let (Some(first), Some(Value::Array(list))) = (parts.next(), parts.next()) {
        let first = first.number().ok_or("a /W start is not a number")? as usize;
        for (at, width) in list.iter().enumerate() {
            report.widths.insert(
                (first + at) as u16,
                width.number().ok_or("a /W value is not a number")?,
            );
        }
    }
    let map_id = font
        .get("ToUnicode")
        .and_then(Value::reference)
        .ok_or_else(|| format!("font {base} has no ToUnicode map"))?;
    report.to_unicode = read_to_unicode(&file.stream(map_id)?)?;
    fonts.insert(id, (base, report));
    Ok(())
}

fn read_to_unicode(map: &[u8]) -> Result<HashMap<u16, String>, String> {
    let text = std::str::from_utf8(map).map_err(|_| "a ToUnicode map is not text")?;
    if !text.contains("begincodespacerange") || !text.contains("endcmap") {
        return Err("a ToUnicode map has no code space or no end".into());
    }
    let mut out = HashMap::new();
    let mut block = false;
    for line in text.lines() {
        if line.ends_with("beginbfchar") {
            block = true;
        } else if line == "endbfchar" {
            block = false;
        } else if block {
            let (code, value) = line.split_once(' ').ok_or("a bfchar line is bad")?;
            let code = code.trim_matches(['<', '>']);
            let code = u16::from_str_radix(code, 16).map_err(|_| "a bfchar code is bad")?;
            let value = value.trim_matches(['<', '>']);
            let mut units = Vec::with_capacity(value.len() / 4);
            for start in (0..value.len()).step_by(4) {
                let unit = value.get(start..start + 4).ok_or("a bfchar value is bad")?;
                units.push(u16::from_str_radix(unit, 16).map_err(|_| "a bfchar value is bad")?);
            }
            out.insert(code, String::from_utf16_lossy(&units));
        }
    }
    Ok(out)
}

fn read_index(data: &[u8], at: usize) -> Result<(Vec<(usize, usize)>, usize), String> {
    let count = usize::from(u16::from_be_bytes([
        *data.get(at).ok_or("CFF is cut off")?,
        *data.get(at + 1).ok_or("CFF is cut off")?,
    ]));
    if count == 0 {
        return Ok((Vec::new(), at + 2));
    }
    let size = usize::from(*data.get(at + 2).ok_or("CFF is cut off")?);
    if !(1..=4).contains(&size) {
        return Err("a CFF index has a bad offset size".into());
    }
    let table = at + 3;
    let base = table + (count + 1) * size - 1;
    let read = |index: usize| -> Result<usize, String> {
        let start = table + index * size;
        let bytes = data.get(start..start + size).ok_or("CFF is cut off")?;
        Ok(bytes
            .iter()
            .fold(0usize, |sum, b| (sum << 8) | usize::from(*b)))
    };
    let mut items = Vec::with_capacity(count);
    let mut last = read(0)?;
    if last != 1 {
        return Err("a CFF index does not start at offset 1".into());
    }
    for index in 1..=count {
        let next = read(index)?;
        if next < last {
            return Err("a CFF index goes backward".into());
        }
        items.push((base + last, base + next));
        last = next;
    }
    if base + last > data.len() {
        return Err("a CFF index runs past the font".into());
    }
    Ok((items, base + last))
}

fn read_dict(data: &[u8]) -> Result<HashMap<u16, Vec<i32>>, String> {
    let mut out = HashMap::new();
    let mut held = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let byte = data[at];
        at += 1;
        match byte {
            0..=21 => {
                let mut operator = u16::from(byte);
                if byte == 12 {
                    operator = 1200 + u16::from(*data.get(at).ok_or("a CFF dict is cut off")?);
                    at += 1;
                }
                out.insert(operator, std::mem::take(&mut held));
            }
            28 => {
                let bytes = data.get(at..at + 2).ok_or("a CFF dict is cut off")?;
                held.push(i32::from(i16::from_be_bytes([bytes[0], bytes[1]])));
                at += 2;
            }
            29 => {
                let bytes = data.get(at..at + 4).ok_or("a CFF dict is cut off")?;
                held.push(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
                at += 4;
            }
            32..=246 => held.push(i32::from(byte) - 139),
            247..=250 => {
                let next = i32::from(*data.get(at).ok_or("a CFF dict is cut off")?);
                held.push((i32::from(byte) - 247) * 256 + next + 108);
                at += 1;
            }
            251..=254 => {
                let next = i32::from(*data.get(at).ok_or("a CFF dict is cut off")?);
                held.push(-(i32::from(byte) - 251) * 256 - next - 108);
                at += 1;
            }
            _ => return Err(format!("a CFF dict has the bad byte {byte}")),
        }
    }
    Ok(out)
}

fn read_cff(data: &[u8]) -> Result<FontReport, String> {
    if data.first() != Some(&1) {
        return Err("the program is not CFF version 1".into());
    }
    let header = usize::from(*data.get(2).ok_or("CFF is cut off")?);
    let (names, at) = read_index(data, header)?;
    if names.len() != 1 {
        return Err("CFF must name one font".into());
    }
    let (tops, at) = read_index(data, at)?;
    let &(start, end) = tops.first().ok_or("CFF has no Top DICT")?;
    let top = read_dict(data.get(start..end).ok_or("CFF is cut off")?)?;
    let (_strings, at) = read_index(data, at)?;
    let (_global, _) = read_index(data, at)?;
    let is_cid_cff = top.get(&1230).is_some_and(|ros| ros.len() == 3);
    let chars_at = top
        .get(&17)
        .and_then(|v| v.first())
        .ok_or("CFF has no CharStrings")?;
    let (chars, _) = read_index(data, *chars_at as usize)?;
    let count = top
        .get(&1234)
        .and_then(|v| v.first())
        .ok_or("CFF has no CIDCount")?;
    if *count as usize != chars.len() {
        return Err(format!(
            "CIDCount says {count}, and CharStrings has {}",
            chars.len()
        ));
    }
    let array_at = top
        .get(&1236)
        .and_then(|v| v.first())
        .ok_or("CFF has no FDArray")?;
    let (array, _) = read_index(data, *array_at as usize)?;
    let &(start, end) = array.first().ok_or("CFF's FDArray is empty")?;
    let font_dict = read_dict(data.get(start..end).ok_or("CFF is cut off")?)?;
    let private = font_dict.get(&18).ok_or("CFF has no Private DICT")?;
    let (size, offset) = (
        *private.first().ok_or("a bad Private entry")? as usize,
        *private.get(1).ok_or("a bad Private entry")? as usize,
    );
    read_dict(
        data.get(offset..offset + size)
            .ok_or("CFF's Private DICT runs past the font")?,
    )?;
    let select_at = *top
        .get(&1237)
        .and_then(|v| v.first())
        .ok_or("CFF has no FDSelect")? as usize;
    if data.get(select_at) != Some(&3) {
        return Err("CFF's FDSelect is not format 3".into());
    }
    let mut charstrings = Vec::with_capacity(chars.len());
    for &(start, end) in &chars {
        let string = data.get(start..end).ok_or("a charstring is cut off")?;
        check_charstring(string)?;
        charstrings.push(string.to_vec());
    }
    Ok(FontReport {
        glyph_count: chars.len(),
        charstrings,
        is_cid_cff,
        ..FontReport::default()
    })
}

/// A charstring must end in `endchar`, keep to the stack limit, and use only the path
/// operators this writer makes.
fn check_charstring(data: &[u8]) -> Result<(), String> {
    let mut at = 0;
    let mut held = 0;
    let mut ended = false;
    while at < data.len() {
        if ended {
            return Err("a charstring goes on after endchar".into());
        }
        let byte = data[at];
        at += 1;
        match byte {
            5 | 8 => {
                let per = if byte == 5 { 2 } else { 6 };
                if held == 0 || held % per != 0 {
                    return Err(format!(
                        "a charstring has {held} numbers for operator {byte}"
                    ));
                }
                held = 0;
            }
            21 => {
                if held != 2 {
                    return Err(format!("rmoveto has {held} numbers"));
                }
                held = 0;
            }
            14 => ended = true,
            28 => {
                at += 2;
                held += 1;
            }
            32..=246 => held += 1,
            247..=254 => {
                at += 1;
                held += 1;
            }
            _ => return Err(format!("a charstring has the operator {byte}")),
        }
        if held > 48 {
            return Err("a charstring's stack is over 48".into());
        }
    }
    if !ended {
        return Err("a charstring has no endchar".into());
    }
    Ok(())
}

fn read_content(
    file: &File<'_>,
    content: &[u8],
    font_refs: &Value,
    fonts: &mut HashMap<usize, (String, FontReport)>,
    out: &mut PageReport,
    resources: &Value,
) -> Result<(), String> {
    let mut parser = Parser {
        data: content,
        at: 0,
    };
    let mut stack: Vec<Value> = Vec::new();
    let (mut font_id, mut size, mut mode) = (None::<usize>, 0.0f64, 0.0f64);
    let (mut x, mut y) = (0.0f64, 0.0f64);
    let mut depth = 0i32;
    let mut in_text = false;
    loop {
        parser.skip_space();
        let Some(&byte) = content.get(parser.at) else {
            break;
        };
        let is_operator = byte.is_ascii_alphabetic() || byte == b'\'' || byte == b'"';
        if !is_operator {
            stack.push(parser.value()?);
            continue;
        }
        let operator = parser.word().to_string();
        let number = |at: usize| -> Result<f64, String> {
            stack
                .get(at)
                .and_then(Value::number)
                .ok_or_else(|| format!("operator {operator} has a bad operand"))
        };
        match operator.as_str() {
            "q" => depth += 1,
            "Q" => {
                depth -= 1;
                if depth < 0 {
                    return Err("Q without q".into());
                }
            }
            "BT" => {
                if in_text {
                    return Err("BT inside BT".into());
                }
                in_text = true;
            }
            "ET" => {
                if !in_text {
                    return Err("ET without BT".into());
                }
                in_text = false;
            }
            "Tf" => {
                let name = stack
                    .first()
                    .and_then(Value::name)
                    .ok_or("Tf has no font")?;
                let id = font_refs
                    .get(name)
                    .and_then(Value::reference)
                    .ok_or_else(|| {
                        format!("the page uses the font {name}, which it does not list")
                    })?;
                load_font(file, id, fonts)?;
                font_id = Some(id);
                size = number(1)?;
            }
            "Tr" => mode = number(0)?,
            "Tm" => {
                x = number(4)?;
                y = number(5)?;
            }
            "Tj" | "TJ" => {
                if !in_text {
                    return Err("text is shown outside BT".into());
                }
                let id = font_id.ok_or("text is shown with no font")?;
                let (name, font) = fonts.get(&id).ok_or("the font was not loaded")?;
                let parts: Vec<&Value> = match stack.first() {
                    Some(Value::Array(items)) => items.iter().collect(),
                    Some(one) => vec![one],
                    None => Vec::new(),
                };
                for part in parts {
                    match part {
                        Value::Number(adjust) => x -= adjust / 1000.0 * size,
                        Value::Text(codes) => {
                            for pair in codes.as_chunks::<2>().0 {
                                let cid = u16::from_be_bytes(*pair);
                                if usize::from(cid) >= font.glyph_count {
                                    return Err(format!(
                                        "{name} has {} glyphs, and the page draws CID {cid}",
                                        font.glyph_count
                                    ));
                                }
                                let width = *font
                                    .widths
                                    .get(&cid)
                                    .ok_or_else(|| format!("{name} has no width for CID {cid}"))?;
                                out.glyphs.push(PlacedGlyph {
                                    font: name.clone(),
                                    cid,
                                    x,
                                    y,
                                    size,
                                    invisible: mode == 3.0,
                                    text: font.to_unicode.get(&cid).cloned().unwrap_or_default(),
                                });
                                x += width / 1000.0 * size;
                            }
                        }
                        _ => return Err("TJ holds something else".into()),
                    }
                }
            }
            "Do" => {
                let name = stack
                    .first()
                    .and_then(Value::name)
                    .ok_or("Do has no name")?;
                let forms = resources.get("XObject").ok_or("Do with no /XObject")?;
                let form = forms
                    .get(name)
                    .and_then(Value::reference)
                    .ok_or_else(|| format!("the page draws {name}, which it does not list"))?;
                file.stream(form)?;
                out.form_calls += 1;
            }
            "re" | "W" | "n" | "cm" | "g" | "rg" | "m" | "l" | "c" | "h" | "f" => {}
            other => return Err(format!("the page uses the operator {other}")),
        }
        stack.clear();
    }
    if depth != 0 {
        return Err("q and Q do not match".into());
    }
    if in_text {
        return Err("BT has no ET".into());
    }
    Ok(())
}
