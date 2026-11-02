//! Reads glyph outlines and font facts from DirectWrite, and keeps each font's subset.
//!
//! Every outline comes from `IDWriteFontFace::GetGlyphRunOutline`, so TrueType, CFF, a
//! variable font's instance and a simulated bold all take one path, and the file holds
//! what the screen shows. A glyph with no outline that a font stores only as a picture
//! is named in an error.
use crate::PdfError;
use crate::cff::{self, Figure, Segment};
use qe_api::Color;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{DWRITE_E_NOCOLOR, E_NOINTERFACE, E_POINTER, S_OK};
use windows::Win32::Graphics::Direct2D::Common::ID2D1SimplifiedGeometrySink;
use windows::Win32::Graphics::Direct2D::{
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1CreateFactory, ID2D1Factory,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_METRICS, DWRITE_GLYPH_IMAGE_FORMATS,
    DWRITE_GLYPH_IMAGE_FORMATS_CFF, DWRITE_GLYPH_IMAGE_FORMATS_COLR,
    DWRITE_GLYPH_IMAGE_FORMATS_JPEG, DWRITE_GLYPH_IMAGE_FORMATS_PNG,
    DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8, DWRITE_GLYPH_IMAGE_FORMATS_TIFF,
    DWRITE_GLYPH_IMAGE_FORMATS_TRUETYPE, DWRITE_GLYPH_OFFSET, DWRITE_GLYPH_RUN,
    DWRITE_MEASURING_MODE_NATURAL, DWriteCreateFactory, IDWriteFactory2, IDWriteFontFace,
    IDWriteFontFace2, IDWriteFontFace4,
};
use windows::core::{BOOL, GUID, HRESULT, IUnknown, Interface};

/// A font's PDF object holds at most this many CIDs. A document that needs more starts
/// another PDF font for the same face.
const MOST_CIDS: usize = 65_000;
/// The most UTF-16 units one CID's map entry holds. PDF allows 256 (512 bytes), but Poppler
/// reads a map one token at a time into 256 bytes, so it warned about entries of 200 units
/// and dropped them. 60 units fit its 63. `check_pdf` refuses a file with a longer entry.
pub const MOST_MAP_UNITS: usize = 60;

/// Cuts text into pieces that each fit one map entry, between characters.
pub fn map_pieces(text: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let (mut start, mut units) = (0, 0);
    for (at, character) in text.char_indices() {
        let wide = character.len_utf16();
        if units + wide > MOST_MAP_UNITS {
            pieces.push(&text[start..at]);
            (start, units) = (at, 0);
        }
        units += wide;
    }
    if start < text.len() {
        pieces.push(&text[start..]);
    }
    pieces
}

/// What one glyph's text is. It decides the CID, because a CID has one ToUnicode entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GlyphText {
    One(char),
    Many(Box<str>),
}

pub struct Cid {
    pub glyph: u16,
    pub text: GlyphText,
    /// In 1/1000 em, from the first time this CID was used.
    pub width: f32,
    /// True when this CID draws nothing and only carries text, such as a tab.
    pub blank: bool,
}

/// One layer of a color glyph.
#[derive(Clone, Copy, Debug)]
pub struct ColorLayer {
    pub glyph: u16,
    pub color: Option<Color>,
}

/// One PDF font: one face, with the glyphs the document uses.
pub struct PdfFont {
    pub font_id: u32,
    pub face: IDWriteFontFace,
    pub postscript_name: String,
    /// Index 0 is `.notdef`.
    pub cids: Vec<Cid>,
    single: HashMap<(u16, char), u16>,
    many: HashMap<(u16, Box<str>), u16>,
    blanks: HashMap<Box<str>, u16>,
}

impl PdfFont {
    fn new(font_id: u32, face: IDWriteFontFace, postscript_name: String) -> Self {
        Self {
            font_id,
            face,
            postscript_name,
            cids: vec![Cid {
                glyph: 0,
                text: GlyphText::Many("".into()),
                width: 0.0,
                blank: true,
            }],
            single: HashMap::new(),
            many: HashMap::new(),
            blanks: HashMap::new(),
        }
    }
    fn is_full(&self) -> bool {
        self.cids.len() >= MOST_CIDS
    }
}

/// All the fonts of one PDF.
#[derive(Default)]
pub struct FontSet {
    pub fonts: Vec<PdfFont>,
    /// The font now being filled for each face.
    current: HashMap<u32, usize>,
    /// For each face: the glyphs already checked, and each color glyph's layers.
    colors: HashMap<(u32, u16), Option<Arc<[ColorLayer]>>>,
    is_color: HashMap<u32, bool>,
    /// Glyphs asked about picture formats.
    checked: HashMap<(u32, u16), ()>,
}

impl FontSet {
    /// The font for this face and whether it has room, adding it at first use.
    fn font_for(&mut self, font_id: u32) -> Result<usize, String> {
        if let Some(&index) = self.current.get(&font_id)
            && !self.fonts[index].is_full()
        {
            return Ok(index);
        }
        let face = qe_layout::font_face(font_id).ok_or_else(|| {
            format!("The PDF cannot find a font that layout used (number {font_id}).")
        })?;
        let name = postscript_name(&face);
        check_embedding_allowed(&face, &name)?;
        self.fonts.push(PdfFont::new(font_id, face, name));
        let index = self.fonts.len() - 1;
        self.current.insert(font_id, index);
        Ok(index)
    }

    /// A CID for one character's glyph. This is the common path, so it makes no string.
    pub fn cid_for_char(
        &mut self,
        font_id: u32,
        glyph: u16,
        text: char,
        width: f32,
    ) -> Result<(usize, u16), String> {
        let index = self.font_for(font_id)?;
        if let Some(&cid) = self.fonts[index].single.get(&(glyph, text)) {
            return Ok((index, cid));
        }
        self.check_glyph(index, glyph)?;
        let font = &mut self.fonts[index];
        let cid = font.cids.len() as u16;
        font.cids.push(Cid {
            glyph,
            text: GlyphText::One(text),
            width,
            blank: false,
        });
        font.single.insert((glyph, text), cid);
        Ok((index, cid))
    }

    /// A CID for a glyph that stands for any other text: several characters, or none.
    pub fn cid_for_text(
        &mut self,
        font_id: u32,
        glyph: u16,
        text: &str,
        width: f32,
    ) -> Result<(usize, u16), String> {
        let mut chars = text.chars();
        if let (Some(only), None) = (chars.next(), chars.next()) {
            return self.cid_for_char(font_id, glyph, only, width);
        }
        let index = self.font_for(font_id)?;
        if let Some(&cid) = self.fonts[index].many.get(&(glyph, text.into())) {
            return Ok((index, cid));
        }
        self.check_glyph(index, glyph)?;
        let font = &mut self.fonts[index];
        let cid = font.cids.len() as u16;
        font.cids.push(Cid {
            glyph,
            text: GlyphText::Many(text.into()),
            width,
            blank: false,
        });
        font.many.insert((glyph, text.into()), cid);
        Ok((index, cid))
    }

    /// A CID with an empty shape that stands for this text: a tab, a character that layout
    /// gives no glyph, or the end of a cluster too long for one map entry.
    pub fn blank_cid(
        &mut self,
        font_id: u32,
        text: &str,
        width: f32,
    ) -> Result<(usize, u16), String> {
        let index = self.font_for(font_id)?;
        let font = &mut self.fonts[index];
        if let Some(&cid) = font.blanks.get(text) {
            return Ok((index, cid));
        }
        let cid = font.cids.len() as u16;
        font.cids.push(Cid {
            glyph: 0,
            text: GlyphText::Many(text.into()),
            width,
            blank: true,
        });
        font.blanks.insert(text.into(), cid);
        Ok((index, cid))
    }

    /// The picture check, for a glyph that is drawn as a shape and so gets no CID.
    pub fn check_glyph_of(&mut self, font_id: u32, glyph: u16) -> Result<(), String> {
        let index = self.font_for(font_id)?;
        self.check_glyph(index, glyph)
    }

    /// A glyph that exists only as a picture has no outline to embed, and would come out
    /// blank. That is an error in plain words.
    fn check_glyph(&mut self, index: usize, glyph: u16) -> Result<(), String> {
        let font = &self.fonts[index];
        if self.checked.insert((font.font_id, glyph), ()).is_some() {
            return Ok(());
        }
        let Ok(face4) = font.face.cast::<IDWriteFontFace4>() else {
            return Ok(());
        };
        let Ok(formats) = (unsafe { face4.GetGlyphImageFormats(glyph, 0, u32::MAX) }) else {
            return Ok(());
        };
        let has = |flag: DWRITE_GLYPH_IMAGE_FORMATS| formats.0 & flag.0 != 0;
        let outline = has(DWRITE_GLYPH_IMAGE_FORMATS_TRUETYPE)
            || has(DWRITE_GLYPH_IMAGE_FORMATS_CFF)
            || has(DWRITE_GLYPH_IMAGE_FORMATS_COLR);
        let picture = has(DWRITE_GLYPH_IMAGE_FORMATS_PNG)
            || has(DWRITE_GLYPH_IMAGE_FORMATS_JPEG)
            || has(DWRITE_GLYPH_IMAGE_FORMATS_TIFF)
            || has(DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8);
        if picture && !outline {
            return Err(format!(
                "The font {} stores some of its characters only as pictures, and QuickEdit \
                 cannot put those in a PDF. Choose another font and export again.",
                font.postscript_name
            ));
        }
        Ok(())
    }

    /// True when the face has color glyphs. The answer is kept, so a run can ask.
    pub fn has_color(&mut self, font_id: u32) -> Result<bool, String> {
        if let Some(&known) = self.is_color.get(&font_id) {
            return Ok(known);
        }
        let index = self.font_for(font_id)?;
        let known = self.fonts[index]
            .face
            .cast::<IDWriteFontFace2>()
            .is_ok_and(|face| unsafe { face.IsColorFont().as_bool() });
        self.is_color.insert(font_id, known);
        Ok(known)
    }

    /// The layers of a color glyph, or `None` for an ordinary glyph.
    pub fn color_layers(
        &mut self,
        font_id: u32,
        glyph: u16,
    ) -> Result<Option<Arc<[ColorLayer]>>, String> {
        if !self.has_color(font_id)? {
            return Ok(None);
        }
        if let Some(found) = self.colors.get(&(font_id, glyph)) {
            return Ok(found.clone());
        }
        let index = self.font_for(font_id)?;
        let found = translate_color(&self.fonts[index].face, glyph)?;
        self.colors.insert((font_id, glyph), found.clone());
        Ok(found)
    }

    /// The face behind a font id already in use.
    pub fn face_of(&mut self, font_id: u32) -> Result<IDWriteFontFace, String> {
        let index = self.font_for(font_id)?;
        Ok(self.fonts[index].face.clone())
    }
}

fn translate_color(
    face: &IDWriteFontFace,
    glyph: u16,
) -> Result<Option<Arc<[ColorLayer]>>, String> {
    let factory: IDWriteFactory2 = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
        .map_err(|e| format!("DirectWrite did not start: {e}"))?;
    let (advance, offset) = (0f32, DWRITE_GLYPH_OFFSET::default());
    let run = DWRITE_GLYPH_RUN {
        fontFace: std::mem::ManuallyDrop::new(Some(face.clone())),
        fontEmSize: 1000.0,
        glyphCount: 1,
        glyphIndices: &glyph,
        glyphAdvances: &advance,
        glyphOffsets: &offset,
        isSideways: BOOL(0),
        bidiLevel: 0,
    };
    let translated = unsafe {
        factory.TranslateColorGlyphRun(0.0, 0.0, &run, None, DWRITE_MEASURING_MODE_NATURAL, None, 0)
    };
    let mut run = run;
    unsafe { std::mem::ManuallyDrop::drop(&mut run.fontFace) };
    let layers = match translated {
        Ok(layers) => layers,
        Err(e) if e.code() == DWRITE_E_NOCOLOR => return Ok(None),
        Err(e) => return Err(format!("DirectWrite could not read a color glyph: {e}")),
    };
    let mut found = Vec::new();
    let problem =
        |e: windows::core::Error| format!("DirectWrite could not read a color glyph: {e}");
    while unsafe { layers.MoveNext().map_err(problem)? }.as_bool() {
        let layer = unsafe { layers.GetCurrentRun().map_err(problem)?.as_ref() };
        let Some(layer) = layer else {
            continue;
        };
        let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        let color = (layer.paletteIndex != u16::MAX).then(|| Color {
            r: channel(layer.runColor.r),
            g: channel(layer.runColor.g),
            b: channel(layer.runColor.b),
            a: channel(layer.runColor.a),
        });
        let ids = layer.glyphRun.glyphIndices;
        for at in 0..layer.glyphRun.glyphCount as usize {
            found.push(ColorLayer {
                glyph: unsafe { *ids.add(at) },
                color,
            });
        }
    }
    Ok(Some(found.into()))
}

/// A hand-written COM object for `ID2D1SimplifiedGeometrySink`. The `windows` crate's trait
/// for it names a type from `windows-numerics`, which is not a direct dependency here,
/// so this keeps its own layout of the same 8-byte point. Its layout is the COM ABI.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawPoint {
    x: f32,
    y: f32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct RawBezier {
    a: RawPoint,
    b: RawPoint,
    c: RawPoint,
}
#[repr(C)]
struct SinkVtable {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    set_fill_mode: unsafe extern "system" fn(*mut c_void, i32),
    set_segment_flags: unsafe extern "system" fn(*mut c_void, i32),
    begin_figure: unsafe extern "system" fn(*mut c_void, RawPoint, i32),
    add_lines: unsafe extern "system" fn(*mut c_void, *const RawPoint, u32),
    add_beziers: unsafe extern "system" fn(*mut c_void, *const RawBezier, u32),
    end_figure: unsafe extern "system" fn(*mut c_void, i32),
    close: unsafe extern "system" fn(*mut c_void) -> HRESULT,
}
#[repr(C)]
struct OutlineSink {
    vtable: *const SinkVtable,
    references: AtomicU32,
    figures: Arc<Mutex<Vec<Figure>>>,
}

static SINK_VTABLE: SinkVtable = SinkVtable {
    query_interface: sink_query,
    add_ref: sink_add_ref,
    release: sink_release,
    set_fill_mode: sink_ignore,
    set_segment_flags: sink_ignore,
    begin_figure: sink_begin,
    add_lines: sink_lines,
    add_beziers: sink_beziers,
    end_figure: sink_ignore,
    close: sink_close,
};

unsafe extern "system" fn sink_query(
    this: *mut c_void,
    id: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if id.is_null() || out.is_null() {
        return E_POINTER;
    }
    let id = unsafe { *id };
    if id == IUnknown::IID || id == ID2D1SimplifiedGeometrySink::IID {
        unsafe {
            sink_add_ref(this);
            *out = this;
        }
        S_OK
    } else {
        unsafe { *out = std::ptr::null_mut() };
        E_NOINTERFACE
    }
}
unsafe extern "system" fn sink_add_ref(this: *mut c_void) -> u32 {
    let sink = unsafe { &*this.cast::<OutlineSink>() };
    sink.references.fetch_add(1, Ordering::Relaxed) + 1
}
unsafe extern "system" fn sink_release(this: *mut c_void) -> u32 {
    let left = {
        let sink = unsafe { &*this.cast::<OutlineSink>() };
        sink.references.fetch_sub(1, Ordering::AcqRel) - 1
    };
    if left == 0 {
        drop(unsafe { Box::from_raw(this.cast::<OutlineSink>()) });
    }
    left
}
unsafe extern "system" fn sink_ignore(_this: *mut c_void, _value: i32) {}
unsafe extern "system" fn sink_close(_this: *mut c_void) -> HRESULT {
    S_OK
}
unsafe extern "system" fn sink_begin(this: *mut c_void, start: RawPoint, _kind: i32) {
    let sink = unsafe { &*this.cast::<OutlineSink>() };
    if let Ok(mut figures) = sink.figures.lock() {
        figures.push(Figure {
            start: flip(start),
            segments: Vec::new(),
        });
    }
}
unsafe extern "system" fn sink_lines(this: *mut c_void, points: *const RawPoint, count: u32) {
    let sink = unsafe { &*this.cast::<OutlineSink>() };
    let points = unsafe { std::slice::from_raw_parts(points, count as usize) };
    if let Ok(mut figures) = sink.figures.lock()
        && let Some(figure) = figures.last_mut()
    {
        figure
            .segments
            .extend(points.iter().map(|point| Segment::Line(flip(*point))));
    }
}
unsafe extern "system" fn sink_beziers(this: *mut c_void, curves: *const RawBezier, count: u32) {
    let sink = unsafe { &*this.cast::<OutlineSink>() };
    let curves = unsafe { std::slice::from_raw_parts(curves, count as usize) };
    if let Ok(mut figures) = sink.figures.lock()
        && let Some(figure) = figures.last_mut()
    {
        figure.segments.extend(
            curves
                .iter()
                .map(|curve| Segment::Curve(flip(curve.a), flip(curve.b), flip(curve.c))),
        );
    }
}

/// DirectWrite's y goes down. A font's and a PDF's go up.
fn flip(point: RawPoint) -> (f32, f32) {
    (point.x, -point.y)
}

fn new_sink(figures: &Arc<Mutex<Vec<Figure>>>) -> ID2D1SimplifiedGeometrySink {
    let raw = Box::into_raw(Box::new(OutlineSink {
        vtable: &SINK_VTABLE,
        references: AtomicU32::new(1),
        figures: figures.clone(),
    }));
    // This takes over the one reference, and dropping it releases the object.
    unsafe { ID2D1SimplifiedGeometrySink::from_raw(raw.cast()) }
}

thread_local! {
    /// Direct2D's geometry code has no GPU part, and it loads only when a PDF is written.
    static GEOMETRY: RefCell<Option<ID2D1Factory>> = const { RefCell::new(None) };
}

/// A glyph's outline, at 1000 units to the em, with the overlaps removed.
///
/// A variable font's glyph, such as Cascadia Mono's `f` and `x`, is built from strokes that
/// overlap. CFF and PDF say that overlaps fill, but Windows' own PDF reader showed holes at
/// them, so the file holds one clean shape that every reader draws the same way.
pub fn glyph_outline(face: &IDWriteFontFace, glyph: u16) -> Result<Vec<Figure>, String> {
    let problem = |e: windows::core::Error| format!("A glyph's outline could not be read: {e}");
    let figures = Arc::new(Mutex::new(Vec::new()));
    let sink = new_sink(&figures);
    unsafe {
        face.GetGlyphRunOutline(1000.0, &glyph, None, None, 1, false, false, &sink)
            .map_err(problem)?;
    }
    let count = figures.lock().map_or(0, |figures| figures.len());
    if count >= 2 {
        figures
            .lock()
            .map_err(|_| "A glyph's outline could not be read.".to_string())?
            .clear();
        let factory = GEOMETRY.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                let made: ID2D1Factory =
                    unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                        .map_err(problem)?;
                *slot = Some(made);
            }
            slot.clone()
                .ok_or_else(|| "Direct2D did not start.".to_string())
        })?;
        let geometry = unsafe { factory.CreatePathGeometry() }.map_err(problem)?;
        let path = unsafe { geometry.Open() }.map_err(problem)?;
        unsafe {
            face.GetGlyphRunOutline(1000.0, &glyph, None, None, 1, false, false, &path)
                .map_err(problem)?;
            path.Close().map_err(problem)?;
            geometry.Outline(None, 0.25, &sink).map_err(problem)?;
        }
    }
    drop(sink);
    let mut figures = figures
        .lock()
        .map_err(|_| "A glyph's outline could not be read.".to_string())?;
    Ok(std::mem::take(&mut *figures))
}

/// A subset's font file, its bounding box, and the box as the descriptor needs it.
pub struct BuiltFont {
    pub file: Vec<u8>,
    pub bbox: [i32; 4],
}

/// Cancel is read before each glyph, because a font with many glyphs takes a long time.
pub fn build_subset(
    font: &PdfFont,
    name: &str,
    cancel: &AtomicBool,
) -> Result<BuiltFont, PdfError> {
    // Most CIDs share a glyph with no other, but a glyph can have several texts.
    let mut made: HashMap<u16, Arc<[u8]>> = HashMap::new();
    let mut strings: Vec<Arc<[u8]>> = Vec::with_capacity(font.cids.len());
    let mut bbox = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    let empty: Arc<[u8]> = Arc::from(cff::charstring(&[]));
    for entry in &font.cids {
        if entry.blank {
            strings.push(empty.clone());
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(PdfError::Cancelled);
        }
        if let Some(found) = made.get(&entry.glyph) {
            strings.push(found.clone());
            continue;
        }
        let figures = glyph_outline(&font.face, entry.glyph).map_err(PdfError::Font)?;
        grow_box(&mut bbox, &figures);
        let string: Arc<[u8]> = Arc::from(cff::charstring(&figures));
        made.insert(entry.glyph, string.clone());
        strings.push(string);
    }
    if bbox[0] > bbox[2] {
        bbox = [0, 0, 0, 0];
    }
    let slices: Vec<&[u8]> = strings.iter().map(|s| &**s).collect();
    Ok(BuiltFont {
        file: cff::build(name, &slices, bbox).map_err(PdfError::Font)?,
        bbox,
    })
}

fn grow_box(bbox: &mut [i32; 4], figures: &[Figure]) {
    let mut add = |point: (f32, f32)| {
        bbox[0] = bbox[0].min(point.0.floor() as i32);
        bbox[1] = bbox[1].min(point.1.floor() as i32);
        bbox[2] = bbox[2].max(point.0.ceil() as i32);
        bbox[3] = bbox[3].max(point.1.ceil() as i32);
    };
    for figure in figures {
        add(figure.start);
        for segment in &figure.segments {
            match *segment {
                Segment::Line(to) => add(to),
                Segment::Curve(a, b, c) => {
                    add(a);
                    add(b);
                    add(c);
                }
            }
        }
    }
}

/// The face's height facts, in 1/1000 em, for the font descriptor.
pub struct FaceFacts {
    pub ascent: i32,
    pub descent: i32,
    pub cap_height: i32,
}
pub fn face_facts(face: &IDWriteFontFace) -> FaceFacts {
    let mut metrics = DWRITE_FONT_METRICS::default();
    unsafe { face.GetMetrics(&mut metrics) };
    let per_em = f32::from(metrics.designUnitsPerEm.max(1));
    let scale = |value: f32| (value * 1000.0 / per_em).round() as i32;
    FaceFacts {
        ascent: scale(f32::from(metrics.ascent)),
        descent: -scale(f32::from(metrics.descent)),
        cap_height: scale(f32::from(metrics.capHeight)),
    }
}

const fn tag(name: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*name)
}

/// Runs `read` on one of the font's tables, if it has it.
fn with_table<T>(face: &IDWriteFontFace, table: u32, read: impl FnOnce(&[u8]) -> T) -> Option<T> {
    let mut data = std::ptr::null_mut();
    let mut size = 0u32;
    let mut context = std::ptr::null_mut();
    let mut exists = BOOL(0);
    unsafe {
        face.TryGetFontTable(table, &mut data, &mut size, &mut context, &mut exists)
            .ok()?;
    }
    if !exists.as_bool() || data.is_null() {
        return None;
    }
    let result = read(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size as usize) });
    unsafe { face.ReleaseFontTable(context) };
    Some(result)
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

/// A font's license can forbid what a PDF needs. In OS/2 `fsType`, the low bits equal to 2
/// say restricted, bit 8 says no subsetting (every font here is a subset), and bit 9 says
/// outlines may not go in a document.
fn embedding_forbidden(bits: u16) -> bool {
    bits & 0x000F == 0x0002 || bits & 0x0100 != 0 || bits & 0x0200 != 0
}

fn check_embedding_allowed(face: &IDWriteFontFace, name: &str) -> Result<(), String> {
    let kind = with_table(face, tag(b"OS/2"), |table| be16(table, 8)).flatten();
    match kind {
        Some(bits) if embedding_forbidden(bits) => Err(format!(
            "The license of the font {name} does not allow it to be embedded in a PDF. \
             Choose another font and export again."
        )),
        _ => Ok(()),
    }
}

/// The font's PostScript name from its `name` table, kept to the letters PDF names allow.
fn postscript_name(face: &IDWriteFontFace) -> String {
    let found = with_table(face, tag(b"name"), read_postscript_name).flatten();
    let name: String = found
        .unwrap_or_else(|| "Font".into())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(60)
        .collect();
    if name.is_empty() { "Font".into() } else { name }
}

fn read_postscript_name(table: &[u8]) -> Option<String> {
    let count = usize::from(be16(table, 2)?);
    let strings = usize::from(be16(table, 4)?);
    for at in 0..count {
        let record = 6 + at * 12;
        if be16(table, record + 6)? != 6 {
            continue;
        }
        let platform = be16(table, record)?;
        let length = usize::from(be16(table, record + 8)?);
        let offset = strings + usize::from(be16(table, record + 10)?);
        let bytes = table.get(offset..offset + length)?;
        return Some(if platform == 1 {
            bytes.iter().map(|&b| b as char).collect()
        } else {
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair))
                .collect();
            String::from_utf16_lossy(&units)
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_api::testing::StringStore;
    use qe_api::{Dip, LineIndex};
    use qe_layout::DirectWriteLayout;
    use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;

    fn font_with_glyphs() -> FontSet {
        let engine =
            DirectWriteLayout::with_font("Cascadia Mono", 10.0, DWRITE_FONT_WEIGHT_NORMAL).unwrap();
        let layout = engine
            .try_layout_line(&StringStore::new("abc"), LineIndex(0), Dip(500.0))
            .unwrap();
        let run = &layout.rows[0].glyph_runs[0];
        let mut set = FontSet::default();
        for glyph in &run.glyph_ids {
            set.cid_for_text(run.font_id, *glyph, "x", 500.0).unwrap();
        }
        set
    }

    #[test]
    fn a_license_bit_that_forbids_a_subset_stops_the_export() {
        assert!(!embedding_forbidden(0));
        assert!(!embedding_forbidden(0x0008));
        assert!(embedding_forbidden(0x0002));
        assert!(embedding_forbidden(0x0100));
        assert!(embedding_forbidden(0x0200));
    }

    #[test]
    fn font_building_stops_on_cancel() {
        let set = font_with_glyphs();
        let result = build_subset(&set.fonts[0], "Test", &AtomicBool::new(true));
        assert!(matches!(result, Err(PdfError::Cancelled)));
        assert!(build_subset(&set.fonts[0], "Test", &AtomicBool::new(false)).is_ok());
    }

    #[test]
    fn a_blank_cid_has_an_empty_shape_and_is_kept_once() {
        let mut set = font_with_glyphs();
        let font_id = set.fonts[0].font_id;
        let (font, first) = set.blank_cid(font_id, "\t", 300.0).unwrap();
        assert_eq!(set.blank_cid(font_id, "\t", 900.0).unwrap(), (font, first));
        assert_ne!(set.blank_cid(font_id, "\u{200b}", 0.0).unwrap().1, first);
        assert!(set.fonts[font].cids[usize::from(first)].blank);
    }

    #[test]
    fn long_text_is_cut_between_characters_and_lost_nowhere() {
        for text in [
            "\u{1f468}".repeat(150),
            "a".repeat(MOST_MAP_UNITS + 1),
            "\u{fe0f}".repeat(405),
            "a".repeat(MOST_MAP_UNITS - 1) + "\u{1f468}" + "b",
            String::new(),
        ] {
            let pieces = map_pieces(&text);
            assert_eq!(pieces.concat(), text);
            assert!(
                pieces
                    .iter()
                    .all(|p| p.encode_utf16().count() <= MOST_MAP_UNITS)
            );
        }
        assert_eq!(map_pieces(&"a".repeat(MOST_MAP_UNITS)).len(), 1);
        assert_eq!(map_pieces(&"a".repeat(MOST_MAP_UNITS + 1)).len(), 2);
    }
}
