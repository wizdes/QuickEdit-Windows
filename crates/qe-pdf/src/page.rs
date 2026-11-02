//! Fills pages with rows, as `PDFRenderer.Pagination` does, and writes their content.
//!
//! A page takes every whole row that fits. A row taller than the page goes alone on its own
//! page, and the text area's clip cuts it there. Pages go to the file as they fill. The
//! footers wait for the end, when the page count is known, as extra content streams.
use crate::cff::Segment;
use crate::fonts::{
    ColorLayer, FontSet, GlyphText, MOST_MAP_UNITS, PdfFont, build_subset, face_facts,
    glyph_outline, map_pieces,
};
use crate::text::glyph_texts;
use crate::writer::{CATALOG_ID, PAGES_ID, PdfFile, RESOURCES_ID};
use crate::{DIPS_PER_POINT, PdfError, PdfOptions};
use qe_api::testing::StringStore;
use qe_api::{Dip, LayoutGlyphRun, LayoutRow, LineIndex, TextSnapshot};
use qe_layout::DirectWriteLayout;
use std::collections::HashMap;
use std::ops::Range;
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
    /// A font and em size for text in a row that has no glyphs at all, such as a lone tab.
    plain_font: (u32, f32),
}

/// One part of a row, in the store's order.
enum RowPart {
    Run(usize),
    Gap(Range<usize>),
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
    pub fn new(options: &PdfOptions, scale: f32, plain_font: (u32, f32)) -> Self {
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
            plain_font,
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
        let origin = |builder: &Self, x: f32, y: f64| {
            (
                margin + f64::from(x) * scale,
                paper_height - margin - (builder.used + y) * scale,
            )
        };
        // The common row is one run that holds all of its text, and needs no list.
        if let [run] = row.glyph_runs.as_slice()
            && run.range == row.range
            && !run.glyph_ids.is_empty()
        {
            let y = f64::from(run.origin.y.0) - f64::from(row.y.0);
            let at = origin(self, run.origin.x.0, y);
            self.place_run(run, row, source, fonts, file, at, scale)?;
            self.used += height;
            return Ok(closed);
        }
        // Layout gives a tab no run, and may give no glyph to a character it does not draw.
        // Those are the row's gaps. They go in the file in the store's order, as blanks.
        let parts = row_parts(row);
        let baseline = row
            .glyph_runs
            .iter()
            .find(|run| !run.glyph_ids.is_empty())
            .map_or(height * 0.8, |run| {
                f64::from(run.origin.y.0) - f64::from(row.y.0)
            });
        let mut last_font = None;
        for part in &parts {
            match part {
                RowPart::Run(index) => {
                    let run = &row.glyph_runs[*index];
                    let y = f64::from(run.origin.y.0) - f64::from(row.y.0);
                    let at = origin(self, run.origin.x.0, y);
                    self.place_run(run, row, source, fonts, file, at, scale)?;
                    last_font = Some((run.font_id, run.font_em_size.0));
                }
                RowPart::Gap(gap) => {
                    let (font_id, em) = last_font
                        .or_else(|| {
                            row.glyph_runs
                                .iter()
                                .find(|run| !run.glyph_ids.is_empty())
                                .map(|run| (run.font_id, run.font_em_size.0))
                        })
                        .unwrap_or(self.plain_font);
                    let (x, width) = gap_place(row, gap);
                    let at = origin(self, x, baseline);
                    self.place_gap(gap, source, fonts, (font_id, em), at, width)?;
                }
            }
        }
        self.used += height;
        Ok(closed)
    }

    /// Puts a gap's text in the file at its place, with nothing to see.
    #[allow(clippy::too_many_arguments)]
    fn place_gap(
        &mut self,
        gap: &Range<usize>,
        source: &dyn TextSnapshot,
        fonts: &mut FontSet,
        (font_id, em): (u32, f32),
        at: (f64, f64),
        width: f32,
    ) -> Result<(), PdfError> {
        let mut text = String::new();
        let range = qe_api::ByteOffset(gap.start)..qe_api::ByteOffset(gap.end);
        if !source.visit_chunks(range, &mut |part| text.push_str(part)) {
            return Err(PdfError::Read(
                "The text could not be read while the PDF was being written, so no PDF was \
                 written."
                    .into(),
            ));
        }
        let size = (f64::from(em) * self.dip_scale) as f32;
        let per_em = 1000.0 / em.max(0.001);
        for piece in map_pieces(&text) {
            let (font, cid) = fonts
                .blank_cid(font_id, piece, width * per_em)
                .map_err(PdfError::Font)?;
            self.place_blank(font, cid, size, at);
        }
        Ok(())
    }

    fn place_blank(&mut self, font: usize, cid: u16, size: f32, at: (f64, f64)) {
        self.set_font(font, size);
        self.move_text(at.0, at.1);
        self.content.push(b'<');
        hex16(&mut self.content, cid);
        self.content.extend_from_slice(b"> Tj\n");
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
        // A piece of text too long for one map entry goes on in blanks after its glyph.
        let plain = !right_to_left
            && !shared
                .iter()
                .any(|piece| piece.is_empty() || piece.len() > MOST_MAP_UNITS)
            && run.offsets.iter().all(|o| o.x.0 == 0.0 && o.y.0 == 0.0)
            && !fonts.has_color(run.font_id).map_err(PdfError::Font)?;
        let mut pen = at.0;
        let mut open_font = None;
        for index in 0..count {
            let advance = f64::from(run.advances[index].0);
            let width = (advance * per_em) as f32;
            let glyph = run.glyph_ids[index];
            // A glyph that stands for no text, such as the second part of a split letter, has
            // no map entry to give. A reader then makes up text from its number, so this
            // glyph goes in the file as a shape, which no reader takes for text.
            let textless = !direct && shared.get(index).is_none_or(String::is_empty);
            let (font, cid) = if direct {
                let character = char::from(text.as_bytes()[index]);
                fonts.cid_for_char(run.font_id, glyph, character, width)
            } else if textless {
                Ok((0, 0))
            } else {
                let piece = shared.get(index).map_or("", String::as_str);
                let head = map_pieces(piece).first().copied().unwrap_or("");
                fonts.cid_for_text(run.font_id, glyph, head, width)
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
            if textless {
                fonts
                    .check_glyph_of(run.font_id, glyph)
                    .map_err(PdfError::Font)?;
                let layers = fonts
                    .color_layers(run.font_id, glyph)
                    .map_err(PdfError::Font)?
                    .filter(|layers| !layers.is_empty())
                    .unwrap_or_else(|| Arc::from([ColorLayer { glyph, color: None }]));
                self.draw_form(run.font_id, glyph, layers, file, size, (glyph_x, glyph_y));
                continue;
            }
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
                    self.content.extend_from_slice(b"> Tj 0 Tr\n");
                    self.draw_form(run.font_id, glyph, layers, file, size, (glyph_x, glyph_y));
                }
            }
            if !direct && let Some(piece) = shared.get(index) {
                for rest in map_pieces(piece).into_iter().skip(1) {
                    let (font, cid) = fonts
                        .blank_cid(run.font_id, rest, 0.0)
                        .map_err(PdfError::Font)?;
                    self.place_blank(font, cid, size, (glyph_x, glyph_y));
                }
            }
        }
        if open_font.is_some() {
            self.content.extend_from_slice(b">] TJ\n");
        }
        Ok(())
    }

    /// Draws a glyph's shapes at a place, in the middle of a text block.
    fn draw_form(
        &mut self,
        font_id: u32,
        glyph: u16,
        layers: Arc<[ColorLayer]>,
        file: &mut PdfFile,
        size: f32,
        at: (f64, f64),
    ) {
        let form = self.form_for(font_id, glyph, layers, file);
        let per_unit = f64::from(size) / 1000.0;
        self.content.extend_from_slice(b"ET\nq ");
        number6(&mut self.content, per_unit);
        self.content.extend_from_slice(b" 0 0 ");
        number6(&mut self.content, per_unit);
        self.content.push(b' ');
        number(&mut self.content, at.0);
        self.content.push(b' ');
        number(&mut self.content, at.1);
        self.content
            .extend_from_slice(format!(" cm /X{form} Do Q\nBT\n").as_bytes());
    }

    /// The form that draws a glyph's layers, one form for each glyph used.
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
        self.write_forms(fonts, file, cancel)?;
        for (index, font) in fonts.fonts.iter().enumerate() {
            let id = write_font(index, font, file, cancel)?;
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

    fn write_forms(
        &mut self,
        fonts: &mut FontSet,
        file: &mut PdfFile,
        cancel: &AtomicBool,
    ) -> Result<(), PdfError> {
        for (id, font_id, layers) in &self.forms {
            if cancel.load(Ordering::Relaxed) {
                return Err(PdfError::Cancelled);
            }
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
///
/// The tail is the end of the text in the store's order, which is not the row's right side
/// in right-to-left text, and shaping can change widths at a cut. So each try is measured
/// whole, and the longest one that fits wins. A page too narrow for even the ellipsis
/// prints no name, because the name must never run into the date.
fn fit_title(engine: &DirectWriteLayout, title: &str, width: f32) -> Result<LaidOutText, PdfError> {
    let whole = lay_out_text(engine, title)?;
    let fits = |text: &LaidOutText| row_width(&text.row) * POINTS_PER_DIP <= width;
    if fits(&whole) {
        return Ok(whole);
    }
    let name = whole.store.as_str();
    let mut cuts: Vec<usize> = whole
        .row
        .caret_stops
        .iter()
        .map(|stop| stop.byte.0)
        .filter(|&byte| byte < name.len() && name.is_char_boundary(byte))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let attempt = |cut: usize| {
        let kept = name.get(..cut).unwrap_or("");
        lay_out_text(engine, &format!("{kept}\u{2026}"))
    };
    let (mut low, mut high) = (0, cuts.len());
    let mut best = None;
    while low < high {
        let middle = (low + high) / 2;
        let text = attempt(cuts[middle])?;
        if fits(&text) {
            best = Some(text);
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    match best {
        Some(text) => Ok(text),
        None => lay_out_text(engine, ""),
    }
}

/// A row's runs and gaps, sorted by where their text starts. A run with no glyphs counts
/// as a gap, so that its text still reaches the file.
fn row_parts(row: &LayoutRow) -> Vec<RowPart> {
    let mut runs: Vec<usize> = (0..row.glyph_runs.len())
        .filter(|&index| !row.glyph_runs[index].glyph_ids.is_empty())
        .collect();
    runs.sort_by_key(|&index| row.glyph_runs[index].range.start);
    let mut parts = Vec::with_capacity(runs.len() * 2 + 1);
    let mut at = row.range.start.0;
    for index in runs {
        let range = &row.glyph_runs[index].range;
        if range.start.0 > at {
            parts.push(RowPart::Gap(at..range.start.0));
        }
        at = at.max(range.end.0);
        parts.push(RowPart::Run(index));
    }
    if row.range.end.0 > at {
        parts.push(RowPart::Gap(at..row.range.end.0));
    }
    parts
}

/// Where a gap starts on its row, and how wide it is, in dips.
fn gap_place(row: &LayoutRow, gap: &Range<usize>) -> (f32, f32) {
    let stop_at = |byte: usize| row.caret_stops.iter().find(|stop| stop.byte.0 == byte);
    let start = stop_at(gap.start).map_or(0.0, |stop| stop.x.0);
    let end = stop_at(gap.end).map_or(start, |stop| stop.x_before.0);
    (start, (end - start).abs())
}

/// Writes one font's objects, and returns the id of the font that pages name.
fn write_font(
    index: usize,
    font: &PdfFont,
    file: &mut PdfFile,
    cancel: &AtomicBool,
) -> Result<usize, PdfError> {
    // A subset's name starts with six capital letters and a plus.
    let mut tag = String::new();
    let mut rest = index;
    for _ in 0..6 {
        tag.insert(0, char::from(b'A' + (rest % 26) as u8));
        rest /= 26;
    }
    let name = format!("{tag}+{}", font.postscript_name);
    let built = build_subset(font, &name, cancel)?;
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

/// The ToUnicode map: each CID's text in UTF-16. Every CID that a page draws has text, and
/// only the unused `.notdef` has no entry.
fn to_unicode(font: &PdfFont) -> String {
    // PDF allows at most 100 entries in a block. `map_pieces` keeps each entry short.
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
        for unit in text.encode_utf16() {
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
