//! Fills pages with rows, as `PDFRenderer.Pagination` does, and writes their content.
//!
//! A page takes every whole row that fits. A row taller than the page goes alone on its own
//! page, and the text area's clip cuts it there. Pages go to the file as they fill. The
//! footers wait for the end, when the page count is known, as extra content streams.
use crate::cff::Segment;
use crate::fonts::{
    ColorLayer, FontSet, GlyphText, PdfFont, build_subset, face_facts, glyph_outline,
};
use crate::text::glyph_texts;
use crate::writer::{CATALOG_ID, PAGES_ID, PdfFile, RESOURCES_ID};
use crate::{DIPS_PER_POINT, PdfError, PdfOptions};
use qe_api::testing::StringStore;
use qe_api::{Dip, LayoutGlyphRun, LayoutRow, LineIndex, TextSnapshot};
use qe_layout::DirectWriteLayout;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;

/// The header and footer font, the Mac's system font at 9 pt.
const HEADER_FONT: &str = "Segoe UI";
const HEADER_POINTS: f32 = 9.0;
/// The space kept between the name and the date.
const HEADER_GAP: f32 = 18.0;
/// What a page's content buffer starts with, so that most pages need no growth.
const CONTENT_START: usize = 16 * 1024;
const POINTS_PER_DIP: f32 = 72.0 / 96.0;

/// What the header prints.
pub struct HeaderText<'a> {
    pub title: &'a str,
    pub date: &'a str,
    pub on: bool,
}
impl<'a> HeaderText<'a> {
    pub fn of(options: &'a PdfOptions) -> Self {
        Self {
            title: &options.title,
            date: &options.date,
            on: options.header_and_footer,
        }
    }
}

struct PageRecord {
    page_id: usize,
    footer_id: Option<usize>,
}

#[derive(PartialEq, Clone, Copy)]
struct CurrentFont {
    font: usize,
    size: f32,
}

pub struct PageBuilder {
    paper: (f32, f32),
    margin: f32,
    /// Points per dip on the page.
    dip_scale: f64,
    /// The height a page holds, in dips of the layout.
    page_height: f64,
    used: f64,
    content: Vec<u8>,
    content_started: bool,
    records: Vec<PageRecord>,
    has_footer: bool,
    current: Option<CurrentFont>,
    /// Each color glyph's form number.
    color_forms: HashMap<(u32, u16), usize>,
    /// Each form's object id, face and layers, kept until the end.
    forms: Vec<(usize, u32, Arc<[ColorLayer]>)>,
    color_glyphs: u64,
    // Kept here so that a run costs no allocation.
    text_buffer: String,
    clip: String,
}

fn number(out: &mut Vec<u8>, value: f64) {
    // Three decimals are a thousandth of a point. This is the hot path of a large export.
    let scaled = (value * 1000.0).round() as i64;
    let negative = scaled < 0;
    let scaled = scaled.unsigned_abs();
    if negative {
        out.push(b'-');
    }
    let part = scaled % 1000;
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    let mut rest = scaled / 1000;
    loop {
        at -= 1;
        digits[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[at..]);
    if part > 0 {
        out.push(b'.');
        out.push(b'0' + (part / 100) as u8);
        if !part.is_multiple_of(100) {
            out.push(b'0' + (part / 10 % 10) as u8);
            if !part.is_multiple_of(10) {
                out.push(b'0' + (part % 10) as u8);
            }
        }
    }
}

fn hex16(out: &mut Vec<u8>, value: u16) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    for shift in [12, 8, 4, 0] {
        out.push(DIGITS[usize::from((value >> shift) & 15)]);
    }
}

impl PageBuilder {
    pub fn new(options: &PdfOptions, scale: f32) -> Self {
        let text_height = options.paper_height - 2.0 * options.margin;
        let text_width = options.paper_width - 2.0 * options.margin;
        let margin = options.margin;
        Self {
            paper: (options.paper_width, options.paper_height),
            margin,
            dip_scale: f64::from(scale) / f64::from(DIPS_PER_POINT),
            page_height: f64::from(text_height) * f64::from(DIPS_PER_POINT) / f64::from(scale),
            used: 0.0,
            content: Vec::with_capacity(CONTENT_START),
            content_started: false,
            records: Vec::new(),
            has_footer: options.header_and_footer,
            current: None,
            color_forms: HashMap::new(),
            forms: Vec::new(),
            color_glyphs: 0,
            text_buffer: String::new(),
            clip: format!("q {margin} {margin} {text_width} {text_height} re W n\nBT\n"),
        }
    }
    pub fn page_index(&self) -> usize {
        self.records.len()
    }
    pub fn color_glyphs(&self) -> u64 {
        self.color_glyphs
    }

    /// Adds a row, starting a new page if it does not fit. True when a page was finished.
    pub fn place_row(
        &mut self,
        row: &LayoutRow,
        source: &dyn TextSnapshot,
        fonts: &mut FontSet,
        file: &mut PdfFile,
    ) -> Result<bool, PdfError> {
        let height = f64::from(row.height.0);
        let mut closed = false;
        if self.used + height > self.page_height + 0.001 && self.used > 0.0 {
            self.close_page(file)?;
            closed = true;
        }
        self.start_content();
        let scale = self.dip_scale;
        let margin = f64::from(self.margin);
        let paper_height = f64::from(self.paper.1);
        for run in &row.glyph_runs {
            let baseline = self.used + f64::from(run.origin.y.0) - f64::from(row.y.0);
            let at = (
                margin + f64::from(run.origin.x.0) * scale,
                paper_height - margin - baseline * scale,
            );
            self.place_run(run, row, source, fonts, file, at, scale)?;
        }
        self.used += height;
        Ok(closed)
    }

    fn start_content(&mut self) {
        if !self.content_started {
            self.content_started = true;
            self.content.clear();
            self.content.extend_from_slice(self.clip.as_bytes());
            self.current = None;
        }
    }

    fn close_page(&mut self, file: &mut PdfFile) -> Result<(), PdfError> {
        self.start_content();
        self.content.extend_from_slice(b"ET\nQ\n");
        let content_id = file.new_id();
        file.stream(content_id, "", &self.content)?;
        let page_id = file.new_id();
        let footer_id = self.has_footer.then(|| file.new_id());
        let contents = match footer_id {
            Some(footer) => format!("[{content_id} 0 R {footer} 0 R]"),
            None => format!("{content_id} 0 R"),
        };
        file.object(
            page_id,
            format!(
                "<< /Type /Page /Parent {PAGES_ID} 0 R /MediaBox [0 0 {} {}] /Resources {RESOURCES_ID} 0 R /Contents {contents} >>",
                self.paper.0, self.paper.1
            )
            .as_bytes(),
        )?;
        self.records.push(PageRecord { page_id, footer_id });
        self.used = 0.0;
        self.content_started = false;
        Ok(())
    }

    fn set_font(&mut self, font: usize, size: f32) {
        let wanted = CurrentFont { font, size };
        if self.current != Some(wanted) {
            self.current = Some(wanted);
            self.content
                .extend_from_slice(format!("/F{font} ").as_bytes());
            number(&mut self.content, f64::from(size));
            self.content.extend_from_slice(b" Tf\n");
        }
    }
    fn move_text(&mut self, x: f64, y: f64) {
        self.content.extend_from_slice(b"1 0 0 1 ");
        number(&mut self.content, x);
        self.content.push(b' ');
        number(&mut self.content, y);
        self.content.extend_from_slice(b" Tm\n");
    }

    #[allow(clippy::too_many_arguments)]
    fn place_run(
        &mut self,
        run: &LayoutGlyphRun,
        row: &LayoutRow,
        source: &dyn TextSnapshot,
        fonts: &mut FontSet,
        file: &mut PdfFile,
        at: (f64, f64),
        scale: f64,
    ) -> Result<(), PdfError> {
        let count = run.glyph_ids.len();
        if count == 0 {
            return Ok(());
        }
        if run.advances.len() != count || run.offsets.len() != count {
            return Err(PdfError::Read(
                "Layout gave a glyph run with mismatched lists, so the PDF stopped.".into(),
            ));
        }
        let mut text = std::mem::take(&mut self.text_buffer);
        text.clear();
        let read = source.visit_chunks(run.range.clone(), &mut |part| text.push_str(part));
        if !read {
            return Err(PdfError::Read(
                "The text could not be read while the PDF was being written.".into(),
            ));
        }
        let result = self.place_glyphs(run, row, &text, fonts, file, at, scale);
        self.text_buffer = text;
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn place_glyphs(
        &mut self,
        run: &LayoutGlyphRun,
        row: &LayoutRow,
        text: &str,
        fonts: &mut FontSet,
        file: &mut PdfFile,
        at: (f64, f64),
        scale: f64,
    ) -> Result<(), PdfError> {
        let count = run.glyph_ids.len();
        let em = run.font_em_size.0;
        let size = (f64::from(em) * scale) as f32;
        let per_em = 1000.0 / f64::from(em.max(0.001));
        // One byte for one glyph is plain ASCII, the common case, and needs no cluster walk.
        let direct = text.is_ascii() && text.len() == count;
        let shared = if direct {
            Vec::new()
        } else {
            glyph_texts(run, row, text)
        };
        // Plain left-to-right text with no offsets goes in one TJ. Everything else is
        // placed glyph by glyph, which also suits marks and Arabic.
        let right_to_left = run.bidi_level & 1 == 1;
        let plain = !right_to_left
            && run.offsets.iter().all(|o| o.x.0 == 0.0 && o.y.0 == 0.0)
            && !fonts.has_color(run.font_id).map_err(PdfError::Font)?;
        let mut pen = at.0;
        let mut open_font = None;
        for index in 0..count {
            let advance = f64::from(run.advances[index].0);
            let width = (advance * per_em) as f32;
            let glyph = run.glyph_ids[index];
            let (font, cid) = if direct {
                let character = char::from(text.as_bytes()[index]);
                fonts.cid_for_char(run.font_id, glyph, character, width)
            } else {
                let piece = shared.get(index).map_or("", String::as_str);
                fonts.cid_for_text(run.font_id, glyph, piece, width)
            }
            .map_err(PdfError::Font)?;
            if plain {
                if open_font != Some(font) {
                    if open_font.is_some() {
                        self.content.extend_from_slice(b">] TJ\n");
                    }
                    self.set_font(font, size);
                    self.move_text(pen, at.1);
                    self.content.extend_from_slice(b"[<");
                    open_font = Some(font);
                }
                hex16(&mut self.content, cid);
                let natural = fonts.fonts[font].cids[usize::from(cid)].width;
                let adjust = f64::from(natural) - advance * per_em;
                if adjust.abs() > 0.05 {
                    self.content.extend_from_slice(b"> ");
                    number(&mut self.content, adjust);
                    self.content.extend_from_slice(b" <");
                }
                pen += advance * scale;
                continue;
            }
            let offset = run.offsets[index];
            let glyph_x = if right_to_left {
                pen -= advance * scale;
                pen - f64::from(offset.x.0) * scale
            } else {
                let x = pen + f64::from(offset.x.0) * scale;
                pen += advance * scale;
                x
            };
            let glyph_y = at.1 + f64::from(offset.y.0) * scale;
            self.set_font(font, size);
            let layers = fonts
                .color_layers(run.font_id, glyph)
                .map_err(PdfError::Font)?
                .filter(|layers| !layers.is_empty());
            self.move_text(glyph_x, glyph_y);
            match layers {
                None => {
                    self.content.push(b'<');
                    hex16(&mut self.content, cid);
                    self.content.extend_from_slice(b"> Tj\n");
                }
                Some(layers) => {
                    self.color_glyphs += 1;
                    // Invisible text for selection and search, and colored shapes to see.
                    self.content.extend_from_slice(b"3 Tr <");
                    hex16(&mut self.content, cid);
                    self.content.extend_from_slice(b"> Tj 0 Tr\nET\nq ");
                    let form = self.form_for(run.font_id, glyph, layers, file);
                    let per_unit = f64::from(size) / 1000.0;
                    number6(&mut self.content, per_unit);
                    self.content.extend_from_slice(b" 0 0 ");
                    number6(&mut self.content, per_unit);
                    self.content.push(b' ');
                    number(&mut self.content, glyph_x);
                    self.content.push(b' ');
                    number(&mut self.content, glyph_y);
                    self.content
                        .extend_from_slice(format!(" cm /X{form} Do Q\nBT\n").as_bytes());
                }
            }
        }
        if open_font.is_some() {
            self.content.extend_from_slice(b">] TJ\n");
        }
        Ok(())
    }

    /// The form that draws a color glyph's layers, one form for each glyph used.
    fn form_for(
        &mut self,
        font_id: u32,
        glyph: u16,
        layers: Arc<[ColorLayer]>,
        file: &mut PdfFile,
    ) -> usize {
        if let Some(&form) = self.color_forms.get(&(font_id, glyph)) {
            return form;
        }
        let form = self.forms.len();
        let id = file.new_id();
        self.forms.push((id, font_id, layers));
        self.color_forms.insert((font_id, glyph), form);
        form
    }

    /// Ends the last page, adds the headers and footers now that the page count is known,
    /// and writes the fonts and the file's tree. Returns the page count.
    pub fn finish_pages(
        &mut self,
        fonts: &mut FontSet,
        file: &mut PdfFile,
        header: HeaderText<'_>,
        cancel: &AtomicBool,
    ) -> Result<usize, PdfError> {
        if self.content_started || self.records.is_empty() {
            self.close_page(file)?;
        }
        let pages = self.records.len();
        if header.on {
            self.write_headers(fonts, file, &header, cancel)?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(PdfError::Cancelled);
        }
        let mut resources = String::from("<< /Font <<");
        self.write_forms(fonts, file)?;
        for (index, font) in fonts.fonts.iter().enumerate() {
            let id = write_font(index, font, file)?;
            resources.push_str(&format!(" /F{index} {id} 0 R"));
        }
        resources.push_str(" >>");
        if !self.forms.is_empty() {
            resources.push_str(" /XObject <<");
            for (index, (id, _, _)) in self.forms.iter().enumerate() {
                resources.push_str(&format!(" /X{index} {id} 0 R"));
            }
            resources.push_str(" >>");
        }
        resources.push_str(" >>");
        file.object(RESOURCES_ID, resources.as_bytes())?;
        let mut tree = format!("<< /Count {pages} /Type /Pages /Kids [");
        for record in &self.records {
            tree.push_str(&format!("{} 0 R ", record.page_id));
        }
        tree.push_str("] >>");
        file.object(PAGES_ID, tree.as_bytes())?;
        file.object(
            CATALOG_ID,
            format!("<< /Type /Catalog /Pages {PAGES_ID} 0 R >>").as_bytes(),
        )?;
        Ok(pages)
    }

    /// The name at the left and the date at the right, in the middle of the top margin,
    /// and "Page N of M" at the right, in the middle of the bottom margin.
    fn write_headers(
        &mut self,
        fonts: &mut FontSet,
        file: &mut PdfFile,
        header: &HeaderText<'_>,
        cancel: &AtomicBool,
    ) -> Result<(), PdfError> {
        let engine =
            DirectWriteLayout::with_font(HEADER_FONT, HEADER_POINTS, DWRITE_FONT_WEIGHT_NORMAL)
                .map_err(|e| PdfError::Font(format!("The header's font could not be used: {e}")))?
                .with_wrap(false);
        let right = self.paper.0 - self.margin;
        let date = lay_out_text(&engine, header.date)?;
        let date_width = row_width(&date.row) * POINTS_PER_DIP;
        let height = (date.row.height.0 * POINTS_PER_DIP).ceil();
        let top = (self.margin - height) / 2.0;
        let title_width = (right - date_width - HEADER_GAP - self.margin).max(0.0);
        let title = fit_title(&engine, header.title, title_width)?;

        self.content.clear();
        self.content.extend_from_slice(b"BT\n");
        self.current = None;
        self.place_header_row(&title, self.margin, top, fonts, file)?;
        self.place_header_row(&date, right - date_width, top, fonts, file)?;
        let common = self.content.clone();

        let total = self.records.len();
        let footer_top = self.paper.1 - self.margin + (self.margin - height) / 2.0;
        for index in 0..total {
            if cancel.load(Ordering::Relaxed) {
                return Err(PdfError::Cancelled);
            }
            self.content.clear();
            self.content.extend_from_slice(&common);
            self.current = None;
            let line = lay_out_text(&engine, &format!("Page {} of {total}", index + 1))?;
            let width = row_width(&line.row) * POINTS_PER_DIP;
            self.place_header_row(&line, right - width, footer_top, fonts, file)?;
            self.content.extend_from_slice(b"ET\n");
            if let Some(id) = self.records.get(index).and_then(|record| record.footer_id) {
                file.stream(id, "", &self.content)?;
            }
        }
        Ok(())
    }
    fn place_header_row(
        &mut self,
        line: &LaidOutText,
        x: f32,
        top: f32,
        fonts: &mut FontSet,
        file: &mut PdfFile,
    ) -> Result<(), PdfError> {
        let scale = f64::from(POINTS_PER_DIP);
        for run in &line.row.glyph_runs {
            let baseline =
                f64::from(top) + (f64::from(run.origin.y.0) - f64::from(line.row.y.0)) * scale;
            let at = (
                f64::from(x) + f64::from(run.origin.x.0) * scale,
                f64::from(self.paper.1) - baseline,
            );
            self.place_run(run, &line.row, &line.store, fonts, file, at, scale)?;
        }
        Ok(())
    }

    fn write_forms(&mut self, fonts: &mut FontSet, file: &mut PdfFile) -> Result<(), PdfError> {
        for (id, font_id, layers) in &self.forms {
            let face = fonts.face_of(*font_id).map_err(PdfError::Font)?;
            let mut out = Vec::new();
            let mut bbox = [0f64, 0.0, 0.0, 0.0];
            for layer in layers.iter() {
                match layer.color {
                    Some(color) => {
                        for channel in [color.r, color.g, color.b] {
                            number(&mut out, f64::from(channel) / 255.0);
                            out.push(b' ');
                        }
                        out.extend_from_slice(b"rg\n");
                    }
                    None => out.extend_from_slice(b"0 g\n"),
                }
                let figures = glyph_outline(&face, layer.glyph).map_err(PdfError::Font)?;
                let mut point = |out: &mut Vec<u8>, at: (f32, f32), end: &[u8]| {
                    let (x, y) = (f64::from(at.0), f64::from(at.1));
                    bbox = [
                        bbox[0].min(x),
                        bbox[1].min(y),
                        bbox[2].max(x),
                        bbox[3].max(y),
                    ];
                    number(out, x);
                    out.push(b' ');
                    number(out, y);
                    out.extend_from_slice(end);
                };
                for figure in &figures {
                    point(&mut out, figure.start, b" m\n");
                    for segment in &figure.segments {
                        match *segment {
                            Segment::Line(to) => point(&mut out, to, b" l\n"),
                            Segment::Curve(a, b, c) => {
                                point(&mut out, a, b" ");
                                point(&mut out, b, b" ");
                                point(&mut out, c, b" c\n");
                            }
                        }
                    }
                    out.extend_from_slice(b"h\n");
                }
                if !figures.is_empty() {
                    out.extend_from_slice(b"f\n");
                }
            }
            let dict = format!(
                "/Type /XObject /Subtype /Form /BBox [{} {} {} {}]",
                bbox[0].floor(),
                bbox[1].floor(),
                bbox[2].ceil(),
                bbox[3].ceil()
            );
            file.stream(*id, &dict, &out)?;
        }
        Ok(())
    }
}

/// Six decimals, for a form's scale, which is a small number.
fn number6(out: &mut Vec<u8>, value: f64) {
    out.extend_from_slice(format!("{value:.6}").as_bytes());
}

struct LaidOutText {
    store: StringStore,
    row: LayoutRow,
}

fn lay_out_text(engine: &DirectWriteLayout, text: &str) -> Result<LaidOutText, PdfError> {
    let one_line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let store = StringStore::new(one_line);
    let layout = engine
        .try_layout_line(&store, LineIndex(0), Dip(1_000_000.0))
        .map_err(|e| PdfError::Layout {
            line: 0,
            message: format!("the header or footer: {e}"),
        })?;
    let row = layout
        .rows
        .into_iter()
        .next()
        .ok_or_else(|| PdfError::Layout {
            line: 0,
            message: "the header or footer has no row".into(),
        })?;
    Ok(LaidOutText { store, row })
}

fn row_width(row: &LayoutRow) -> f32 {
    row.caret_stops
        .iter()
        .map(|stop| stop.x.0.max(stop.x_before.0))
        .fold(0.0, f32::max)
}

/// The name, cut at its tail with an ellipsis if it is too wide, as the Mac's is.
fn fit_title(engine: &DirectWriteLayout, title: &str, width: f32) -> Result<LaidOutText, PdfError> {
    let whole = lay_out_text(engine, title)?;
    if row_width(&whole.row) * POINTS_PER_DIP <= width {
        return Ok(whole);
    }
    let ellipsis = lay_out_text(engine, "\u{2026}")?;
    let room = width / POINTS_PER_DIP - row_width(&ellipsis.row);
    let cut = whole
        .row
        .caret_stops
        .iter()
        .filter(|stop| stop.x.0 <= room)
        .map(|stop| stop.byte.0)
        .max()
        .unwrap_or(0);
    let kept = whole.store.as_str().get(..cut).unwrap_or("");
    lay_out_text(engine, &format!("{kept}\u{2026}"))
}

/// Writes one font's objects, and returns the id of the font that pages name.
fn write_font(index: usize, font: &PdfFont, file: &mut PdfFile) -> Result<usize, PdfError> {
    // A subset's name starts with six capital letters and a plus.
    let mut tag = String::new();
    let mut rest = index;
    for _ in 0..6 {
        tag.insert(0, char::from(b'A' + (rest % 26) as u8));
        rest /= 26;
    }
    let name = format!("{tag}+{}", font.postscript_name);
    let built = build_subset(font, &name).map_err(PdfError::Font)?;
    let facts = face_facts(&font.face);

    let file_id = file.new_id();
    file.stream(file_id, "/Subtype /CIDFontType0C", &built.file)?;
    let descriptor = file.new_id();
    let [left, bottom, right, top] = built.bbox;
    file.object(
        descriptor,
        format!(
            "<< /Type /FontDescriptor /FontName /{name} /Flags 4 /FontBBox [{left} {bottom} {right} {top}] /ItalicAngle 0 /Ascent {} /Descent {} /CapHeight {} /StemV 80 /FontFile3 {file_id} 0 R >>",
            facts.ascent, facts.descent, facts.cap_height
        )
        .as_bytes(),
    )?;

    let mut widths = Vec::from(&b"[0 ["[..]);
    for cid in &font.cids {
        number(&mut widths, f64::from(cid.width));
        widths.push(b' ');
    }
    widths.extend_from_slice(b"]]");
    let widths = String::from_utf8_lossy(&widths);
    let descendant = file.new_id();
    file.object(
        descendant,
        format!(
            "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /{name} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {descriptor} 0 R /DW 0 /W {widths} >>"
        )
        .as_bytes(),
    )?;

    let map = file.new_id();
    file.stream(map, "", to_unicode(font).as_bytes())?;
    let id = file.new_id();
    file.object(
        id,
        format!(
            "<< /Type /Font /Subtype /Type0 /BaseFont /{name} /Encoding /Identity-H /DescendantFonts [{descendant} 0 R] /ToUnicode {map} 0 R >>"
        )
        .as_bytes(),
    )?;
    Ok(id)
}

/// The ToUnicode map: each CID's text in UTF-16. A glyph that stands for no text, such as
/// the second glyph of a split letter, has no entry.
fn to_unicode(font: &PdfFont) -> String {
    // PDF allows at most 512 bytes for one entry, and at most 100 entries in a block.
    const MOST_UNITS: usize = 256;
    let mut entries = Vec::with_capacity(font.cids.len());
    let mut buffer = [0u8; 4];
    for (cid, entry) in font.cids.iter().enumerate() {
        let text: &str = match &entry.text {
            GlyphText::One(character) => character.encode_utf8(&mut buffer),
            GlyphText::Many(text) => text,
        };
        if text.is_empty() {
            continue;
        }
        let mut units = String::new();
        for unit in text.encode_utf16().take(MOST_UNITS) {
            units.push_str(&format!("{unit:04X}"));
        }
        entries.push(format!("<{cid:04X}> <{units}>"));
    }
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for block in entries.chunks(100) {
        out.push_str(&format!("{} beginbfchar\n", block.len()));
        for entry in block {
            out.push_str(entry);
            out.push('\n');
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out
}
