//! All text takes one path: find the shape, find each glyph's coverage in a cache,
//! and blend it into the pixels. DirectWrite only makes the coverage. So text obeys
//! every clip exactly, and a repeated draw cannot make it bolder.
use crate::{
    TextSettings, error,
    geom::{Area, snap},
    shapes::Surface,
};
use qe_api::{Color, Dip, Point, RenderError};
use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{BuildHasherDefault, Hasher},
    mem::ManuallyDrop,
    rc::Rc,
    sync::{Arc, Mutex},
};
use windows::{
    Win32::{Foundation::DWRITE_E_NOCOLOR, Graphics::DirectWrite::*},
    core::{BOOL, HRESULT, HSTRING, Interface, PCWSTR, implement},
};

/// A small, quick hash. Nothing here comes from a hostile source.
#[derive(Default)]
pub(crate) struct QuickHasher(u64);
const SEED: u64 = 0x517c_c1b7_2722_0a95;
impl Hasher for QuickHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(word));
        }
    }
    #[inline]
    fn write_u8(&mut self, v: u8) {
        self.write_u64(u64::from(v));
    }
    #[inline]
    fn write_u32(&mut self, v: u32) {
        self.write_u64(u64::from(v));
    }
    #[inline]
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(SEED);
    }
    #[inline]
    fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}
type Map<K, V> = HashMap<K, V, BuildHasherDefault<QuickHasher>>;

/// 32 MiB holds about 40,000 glyphs at ordinary sizes.
pub(crate) const GLYPH_CACHE_BYTES: usize = 32 * 1024 * 1024;
const SHAPE_CACHE_BYTES: usize = 8 * 1024 * 1024;
const MOST_INKS: usize = 64;
const MOST_COLOR_GLYPHS: usize = 4096;
/// Coverage is made for this many positions across one pixel.
const SUBPIXEL_POSITIONS: f64 = 4.0;
/// Glyph edges spread past the outline by the ClearType filter.
const FILTER_REACH: f64 = 3.0;

struct Face {
    face: IDWriteFontFace,
    is_color: bool,
    is_monospaced: bool,
    units_per_em: f32,
    /// How far ink reaches from a glyph's origin, as a share of the font size. Each
    /// value comes from the font and is at least a safe minimum, because some fonts
    /// understate these numbers.
    left: f32,
    right: f32,
    up: f32,
    down: f32,
}

struct GlyphImage {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    /// Three coverage values for each pixel, in red, green, blue order. They do not
    /// depend on any color.
    coverage: Box<[u8]>,
    used: u32,
}
impl GlyphImage {
    fn bytes(&self) -> usize {
        self.coverage.len() + std::mem::size_of::<Self>() + 16
    }
}

/// Light values for the gamma that DirectWrite blends with.
struct Gamma {
    to_linear: [u16; 256],
    from_linear: Box<[u8]>,
}
impl Gamma {
    fn new(gamma: f32) -> Self {
        let gamma = f64::from(gamma.max(1.0));
        let mut to_linear = [0u16; 256];
        for (i, v) in to_linear.iter_mut().enumerate() {
            *v = ((i as f64 / 255.0).powf(gamma) * 65535.0).round() as u16;
        }
        // Each byte owns the light values nearest to its own, so a value converted
        // both ways stays the same. This costs only one fill, not 65,536 powers.
        let mut from_linear = vec![255u8; 65536].into_boxed_slice();
        let mut start = 0usize;
        for i in 0..255 {
            let end = (usize::from(to_linear[i]) + usize::from(to_linear[i + 1])).div_ceil(2);
            from_linear[start..end.max(start)].fill(i as u8);
            start = end.max(start);
        }
        Self {
            to_linear,
            from_linear,
        }
    }
}
/// The output bytes for one background color, for each coverage value and channel.
struct OnBackground {
    pixel: u32,
    out: [[u8; 256]; 3],
}
const BACKGROUNDS_PER_INK: usize = 4;
/// What blending needs for one ink color.
struct Ink {
    linear: [u32; 3],
    /// The blend alpha for each coverage value and channel.
    alpha: [[u8; 256]; 3],
    backgrounds: Vec<OnBackground>,
    next_slot: usize,
    candidate: u32,
    gamma: Rc<Gamma>,
}
impl Ink {
    #[inline]
    fn channel(&self, c: usize, coverage: u8, destination: u32) -> u32 {
        let alpha = u32::from(self.alpha[c][coverage as usize]);
        let light = (self.linear[c] * alpha
            + u32::from(self.gamma.to_linear[destination as usize]) * (255 - alpha)
            + 127)
            / 255;
        u32::from(self.gamma.from_linear[light as usize])
    }
    /// The exact blend. `coverage` is in red, green, blue order.
    #[inline]
    fn blend(&self, p: u32, coverage: &[u8; 3]) -> u32 {
        let r = self.channel(0, coverage[0], (p >> 16) & 255);
        let g = self.channel(1, coverage[1], (p >> 8) & 255);
        let b = self.channel(2, coverage[2], p & 255);
        b | (g << 8) | (r << 16) | 0xFF00_0000
    }
    /// Text nearly always lies on one flat color, and a table for that color saves
    /// all the arithmetic. A color gets a table when two glyphs in a row start on it,
    /// so that a glyph which starts on another glyph's edge does not make a table.
    fn table(&mut self, pixel: u32) -> Option<usize> {
        if let Some(found) = self.backgrounds.iter().position(|t| t.pixel == pixel) {
            return Some(found);
        }
        if self.candidate != pixel {
            self.candidate = pixel;
            return None;
        }
        let mut made = OnBackground {
            pixel,
            out: [[0; 256]; 3],
        };
        let channels = [(pixel >> 16) & 255, (pixel >> 8) & 255, pixel & 255];
        for (c, destination) in channels.into_iter().enumerate() {
            for coverage in 0..=255u8 {
                made.out[c][coverage as usize] = self.channel(c, coverage, destination) as u8;
            }
        }
        if self.backgrounds.len() < BACKGROUNDS_PER_INK {
            self.backgrounds.push(made);
            Some(self.backgrounds.len() - 1)
        } else {
            let slot = self.next_slot;
            self.next_slot = (slot + 1) % BACKGROUNDS_PER_INK;
            self.backgrounds[slot] = made;
            Some(slot)
        }
    }
}

#[derive(Clone, Copy)]
struct Layer {
    glyph: u16,
    x: f32,
    y: f32,
    /// `None` means the color of the text.
    color: Option<Color>,
}

pub(crate) struct ShapedRun {
    face: u32,
    em: f32,
    bidi: u8,
    x: f32,
    y: f32,
    ids: Vec<u16>,
    advances: Vec<Dip>,
    offsets: Vec<Point>,
}
pub(crate) struct Shaped {
    runs: Vec<ShapedRun>,
    bytes: usize,
}

/// One glyph run's data. Glyph runs and shaped text both draw through this.
pub(crate) struct RunRef<'a> {
    pub face: u32,
    pub em: f32,
    pub bidi: u8,
    pub ids: &'a [u16],
    pub advances: &'a [Dip],
    pub offsets: &'a [Point],
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Counts {
    pub rasterized: u64,
    pub drawn: u64,
}

pub(crate) struct Text {
    factory: IDWriteFactory,
    params: IDWriteRenderingParams,
    settings: TextSettings,
    families: Map<String, Option<u32>>,
    raw_faces: Map<usize, u32>,
    faces: Vec<Face>,
    font_ids: Map<u32, u32>,
    shaped: Map<(u32, u32), Map<String, Rc<Shaped>>>,
    shaped_bytes: usize,
    glyphs: Map<u64, GlyphImage>,
    glyph_bytes: usize,
    glyph_limit: usize,
    inks: Map<u32, Rc<RefCell<Ink>>>,
    gamma: Rc<Gamma>,
    color_layers: Map<u64, Option<Rc<[Layer]>>>,
    tick: u32,
    pub counts: Counts,
}

fn system_settings(params: &IDWriteRenderingParams) -> TextSettings {
    unsafe {
        TextSettings {
            gamma: params.GetGamma(),
            enhanced_contrast: params.GetEnhancedContrast(),
            cleartype_level: params.GetClearTypeLevel(),
            geometry: params.GetPixelGeometry(),
            mode: params.GetRenderingMode(),
        }
    }
}
fn wide(s: &str) -> HSTRING {
    HSTRING::from(s)
}

impl Text {
    pub fn new() -> Result<Self, RenderError> {
        let factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).map_err(error)? };
        let params = unsafe { factory.CreateRenderingParams().map_err(error)? };
        let settings = system_settings(&params);
        Ok(Self {
            factory,
            params,
            settings,
            families: Map::default(),
            raw_faces: Map::default(),
            faces: Vec::new(),
            font_ids: Map::default(),
            shaped: Map::default(),
            shaped_bytes: 0,
            glyphs: Map::default(),
            glyph_bytes: 0,
            glyph_limit: GLYPH_CACHE_BYTES,
            inks: Map::default(),
            gamma: Rc::new(Gamma::new(settings.gamma)),
            color_layers: Map::default(),
            tick: 0,
            counts: Counts::default(),
        })
    }
    pub fn gdi_interop(&self) -> Result<IDWriteGdiInterop, RenderError> {
        unsafe { self.factory.GetGdiInterop().map_err(error) }
    }
    pub fn settings(&self) -> TextSettings {
        self.settings
    }
    /// New settings change every glyph's coverage, so the glyphs start over.
    pub fn set_settings(&mut self, v: TextSettings) -> Result<(), RenderError> {
        self.params = unsafe {
            self.factory
                .CreateCustomRenderingParams(
                    v.gamma,
                    v.enhanced_contrast,
                    v.cleartype_level,
                    v.geometry,
                    v.mode,
                )
                .map_err(error)?
        };
        self.settings = v;
        self.glyphs.clear();
        self.glyph_bytes = 0;
        self.inks.clear();
        self.gamma = Rc::new(Gamma::new(v.gamma));
        Ok(())
    }
    pub fn next_draw(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }
    pub fn glyph_bytes(&self) -> usize {
        self.glyph_bytes
    }
    pub fn glyph_entries(&self) -> usize {
        self.glyphs.len()
    }
    pub fn set_glyph_limit(&mut self, bytes: usize) {
        self.glyph_limit = bytes;
        self.evict_to(bytes);
    }
    fn evict_to(&mut self, limit: usize) {
        if self.glyph_bytes <= limit {
            return;
        }
        let mut order: Vec<(u32, u64, usize)> = self
            .glyphs
            .iter()
            .map(|(k, g)| (self.tick.wrapping_sub(g.used), *k, g.bytes()))
            .collect();
        // The longest unused come first. Drop a quarter more than needed, so that the
        // sort does not run again for every new glyph.
        order.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.0));
        let goal = limit - limit / 4;
        for (_, key, bytes) in order {
            if self.glyph_bytes <= goal {
                break;
            }
            self.glyphs.remove(&key);
            self.glyph_bytes -= bytes;
        }
    }

    pub fn register_id(&mut self, id: u32, face: u32) {
        self.font_ids.insert(id, face);
    }
    pub fn face_of_id(&self, id: u32) -> Option<u32> {
        self.font_ids.get(&id).copied()
    }
    pub fn registered(&self) -> Vec<(u32, IDWriteFontFace)> {
        self.font_ids
            .iter()
            .map(|(id, f)| (*id, self.faces[*f as usize].face.clone()))
            .collect()
    }
    pub fn intern(&mut self, face: &IDWriteFontFace) -> u32 {
        let raw = face.as_raw() as usize;
        if let Some(id) = self.raw_faces.get(&raw) {
            return *id;
        }
        let mut units = 2048.0;
        let (mut left, mut right, mut up, mut down) = (-0.5f32, 1.5f32, 1.2f32, 0.5f32);
        let mut is_monospaced = false;
        if let Ok(face1) = face.cast::<IDWriteFontFace1>() {
            let mut m = DWRITE_FONT_METRICS1::default();
            unsafe { face1.GetMetrics(&mut m) };
            units = f32::from(m.Base.designUnitsPerEm.max(1));
            left = left.min(f32::from(m.glyphBoxLeft) / units);
            right = right.max(f32::from(m.glyphBoxRight) / units);
            up = up.max(f32::from(m.glyphBoxTop) / units);
            down = down.max(-f32::from(m.glyphBoxBottom) / units);
            is_monospaced = unsafe { face1.IsMonospacedFont() }.as_bool();
        }
        let is_color = face
            .cast::<IDWriteFontFace2>()
            .is_ok_and(|f| unsafe { f.IsColorFont() }.as_bool());
        let id = self.faces.len() as u32;
        self.faces.push(Face {
            face: face.clone(),
            is_color,
            is_monospaced,
            units_per_em: units,
            left,
            right,
            up,
            down,
        });
        self.raw_faces.insert(raw, id);
        id
    }
    /// The face of an installed family. A missing family is remembered, because the
    /// system collection does not change during the run.
    pub fn family(&mut self, name: &str) -> Result<u32, RenderError> {
        if let Some(found) = self.families.get(name) {
            return found.ok_or_else(|| missing_family(name));
        }
        let found = self.find_family(name)?;
        self.families.insert(name.to_owned(), found);
        found.ok_or_else(|| missing_family(name))
    }
    fn find_family(&mut self, name: &str) -> Result<Option<u32>, RenderError> {
        let mut collection = None;
        unsafe {
            self.factory
                .GetSystemFontCollection(&mut collection, false)
                .map_err(error)?;
        }
        let collection = collection.ok_or_else(|| error("the font collection is missing"))?;
        let (mut index, mut exists) = (0, BOOL(0));
        let name = wide(name);
        unsafe {
            collection
                .FindFamilyName(PCWSTR(name.as_ptr()), &mut index, &mut exists)
                .map_err(error)?;
        }
        if !exists.as_bool() {
            return Ok(None);
        }
        let face = unsafe {
            collection
                .GetFontFamily(index)
                .map_err(error)?
                .GetFirstMatchingFont(
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                )
                .map_err(error)?
                .CreateFontFace()
                .map_err(error)?
        };
        Ok(Some(self.intern(&face)))
    }
    pub fn icon_glyph(&self, face: u32, point: u32) -> Result<u16, RenderError> {
        let mut glyph = 0u16;
        unsafe {
            self.faces[face as usize]
                .face
                .GetGlyphIndices(&point, 1, &mut glyph)
                .map_err(error)?;
        }
        Ok(glyph)
    }

    /// The shape of a `TextRun`. Ordinary text in a fixed-pitch font maps straight
    /// to glyphs. Anything else goes through DirectWrite's text layout, which gives
    /// font fallback, shaping and direction, and the same widths that the measurer
    /// gives. Either result stays in a cache, so a warm frame calls neither.
    pub fn shape(
        &mut self,
        family: &str,
        size: f32,
        text: &str,
    ) -> Result<Rc<Shaped>, RenderError> {
        let face = self.family(family)?;
        let key = (face, size.to_bits());
        if let Some(found) = self.shaped.get(&key).and_then(|m| m.get(text)) {
            return Ok(found.clone());
        }
        let shaped = match self.plain_shape(face, size, text)? {
            Some(shaped) => shaped,
            None => self.layout_shape(family, size, text)?,
        };
        if self.shaped_bytes + shaped.bytes > SHAPE_CACHE_BYTES {
            self.shaped.clear();
            self.shaped_bytes = 0;
        }
        self.shaped_bytes += shaped.bytes;
        let shaped = Rc::new(shaped);
        self.shaped
            .entry(key)
            .or_default()
            .insert(text.to_owned(), shaped.clone());
        Ok(shaped)
    }
    fn plain_shape(&self, face: u32, size: f32, text: &str) -> Result<Option<Shaped>, RenderError> {
        let entry = &self.faces[face as usize];
        if !entry.is_monospaced || !text.bytes().all(|b| (0x20..0x7f).contains(&b)) {
            return Ok(None);
        }
        let points: Vec<u32> = text.bytes().map(u32::from).collect();
        let mut ids = vec![0u16; points.len()];
        let mut metrics = vec![DWRITE_GLYPH_METRICS::default(); points.len()];
        if !points.is_empty() {
            unsafe {
                entry
                    .face
                    .GetGlyphIndices(points.as_ptr(), points.len() as u32, ids.as_mut_ptr())
                    .map_err(error)?;
                if ids.contains(&0) {
                    return Ok(None);
                }
                entry
                    .face
                    .GetDesignGlyphMetrics(
                        ids.as_ptr(),
                        ids.len() as u32,
                        metrics.as_mut_ptr(),
                        false,
                    )
                    .map_err(error)?;
            }
        }
        let advances: Vec<Dip> = metrics
            .iter()
            .map(|m| Dip(m.advanceWidth as f32 * size / entry.units_per_em))
            .collect();
        let offsets = vec![Point::default(); ids.len()];
        let bytes = text.len() + ids.len() * 14 + 128;
        Ok(Some(Shaped {
            runs: vec![ShapedRun {
                face,
                em: size,
                bidi: 0,
                x: 0.0,
                y: 0.0,
                ids,
                advances,
                offsets,
            }],
            bytes,
        }))
    }
    fn layout_shape(&mut self, family: &str, size: f32, text: &str) -> Result<Shaped, RenderError> {
        let units: Vec<u16> = text.encode_utf16().collect();
        if units.is_empty() {
            return Ok(Shaped {
                runs: Vec::new(),
                bytes: 128,
            });
        }
        let (family_name, locale) = (wide(family), wide("en-US"));
        let layout = unsafe {
            let format = self
                .factory
                .CreateTextFormat(
                    PCWSTR(family_name.as_ptr()),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    PCWSTR(locale.as_ptr()),
                )
                .map_err(error)?;
            self.factory
                .CreateTextLayout(&units, &format, 1_000_000.0, 1_000_000.0)
                .map_err(error)?
        };
        let mut line = [DWRITE_LINE_METRICS::default(); 1];
        let mut count = 0;
        // A text of many lines reports that the array is short. Only the first
        // baseline is needed, and DirectWrite fills it in before it reports that.
        let lines = unsafe { layout.GetLineMetrics(Some(&mut line), &mut count) };
        if let Err(e) = lines
            && count == 0
        {
            return Err(error(e));
        }
        let collected = Arc::new(Mutex::new(Vec::new()));
        let collector: IDWriteTextRenderer = RunCollector {
            runs: collected.clone(),
        }
        .into();
        unsafe {
            layout.Draw(None, &collector, 0.0, 0.0).map_err(error)?;
        }
        let raw = std::mem::take(
            &mut *collected
                .lock()
                .map_err(|_| error("the text layout collector failed"))?,
        );
        let mut bytes = text.len() + 128;
        let mut runs = Vec::with_capacity(raw.len());
        for run in raw {
            bytes += run.ids.len() * 14 + 64;
            runs.push(ShapedRun {
                face: self.intern(&run.face),
                em: run.em,
                bidi: run.bidi,
                x: run.x,
                y: run.y - line[0].baseline,
                ids: run.ids,
                advances: run.advances,
                offsets: run.offsets,
            });
        }
        Ok(Shaped { runs, bytes })
    }

    fn ink(&mut self, c: Color) -> Rc<RefCell<Ink>> {
        let key = u32::from_le_bytes([c.r, c.g, c.b, c.a]);
        if let Some(found) = self.inks.get(&key) {
            return found.clone();
        }
        if self.inks.len() >= MOST_INKS {
            self.inks.clear();
        }
        let ink = Rc::new(RefCell::new(make_ink(c, self.settings, self.gamma.clone())));
        self.inks.insert(key, ink.clone());
        ink
    }

    /// Draws a shaped text with its start at `at`, in device pixels.
    pub fn draw_shaped(
        &mut self,
        surface: Surface,
        clip: Area,
        scale: f64,
        at: (f64, f64),
        shaped: &Shaped,
        color: Color,
    ) -> Result<(), RenderError> {
        let mut first_error = Ok(());
        for run in &shaped.runs {
            let result = self.draw_run(
                surface,
                clip,
                scale,
                (
                    at.0 + f64::from(run.x) * scale,
                    at.1 + f64::from(run.y) * scale,
                ),
                RunRef {
                    face: run.face,
                    em: run.em,
                    bidi: run.bidi,
                    ids: &run.ids,
                    advances: &run.advances,
                    offsets: &run.offsets,
                },
                color,
            );
            if first_error.is_ok() {
                first_error = result;
            }
        }
        first_error
    }

    /// Draws one glyph run whose start is at `at`, in device pixels. A glyph that
    /// fails does not stop the others. The first failure comes back at the end.
    pub fn draw_run(
        &mut self,
        surface: Surface,
        clip: Area,
        scale: f64,
        at: (f64, f64),
        run: RunRef<'_>,
        color: Color,
    ) -> Result<(), RenderError> {
        if color.a == 0 || run.ids.is_empty() || !clip.valid() {
            return Ok(());
        }
        if !run.em.is_finite() || run.em <= 0.0 {
            return Err(error(format!("bad font size {}", run.em)));
        }
        let em_device = f64::from(run.em) * scale;
        let em_units = (em_device * 64.0).round() as u64;
        if em_units == 0 {
            return Ok(());
        }
        if em_units >= 1 << 24 {
            return Err(error(format!("font size {em_device} pixels is too large")));
        }
        let entry = &self.faces[run.face as usize];
        // Real extents. Ink stays within the font's box of each glyph's origin.
        let (mut most_x, mut most_up, mut most_down) = (0f32, 0f32, 0f32);
        for o in run.offsets {
            most_x = most_x.max(o.x.0.abs());
            most_up = most_up.max(o.y.0);
            most_down = most_down.max(-o.y.0);
        }
        let reach_left =
            f64::from(-entry.left) * em_device + f64::from(most_x) * scale + FILTER_REACH;
        let reach_right =
            f64::from(entry.right) * em_device + f64::from(most_x) * scale + FILTER_REACH;
        let above = f64::from(entry.up) * em_device + f64::from(most_up) * scale + FILTER_REACH;
        let below = f64::from(entry.down) * em_device + f64::from(most_down) * scale + FILTER_REACH;
        if at.1 + below < f64::from(clip.y) || at.1 - above > f64::from(clip.b) {
            return Ok(());
        }
        let is_color = entry.is_color;
        let rtl = run.bidi & 1 == 1;
        let direction = if rtl { -1.0 } else { 1.0 };
        let (clip_left, clip_right) = (f64::from(clip.x), f64::from(clip.r));
        let ink = self.ink(color);
        let mut ink = ink.borrow_mut();
        let ink = &mut *ink;
        let prefix = (u64::from(run.face) << 42) | (em_units << 18);
        let tick = self.tick;
        let mut pen = at.0;
        let mut first_error = Ok(());
        let em_key = em_units as f32 / 64.0;
        for i in 0..run.ids.len() {
            let advance = f64::from(run.advances[i].0) * scale;
            if rtl {
                pen -= advance;
                if pen + reach_right < clip_left {
                    break;
                }
            } else if pen - reach_left > clip_right {
                break;
            }
            let glyph_pen = pen;
            if !rtl {
                pen += advance;
            }
            if glyph_pen + reach_right < clip_left || glyph_pen - reach_left > clip_right {
                continue;
            }
            let offset = run.offsets[i];
            let x = glyph_pen + direction * f64::from(offset.x.0) * scale;
            let y = at.1 - f64::from(offset.y.0) * scale;
            let id = run.ids[i];
            self.counts.drawn += 1;
            if is_color {
                let drew = self.draw_color_glyph(
                    surface,
                    clip,
                    (x, y),
                    (run.face, em_key, em_units),
                    id,
                    color,
                );
                match drew {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(e) => {
                        if first_error.is_ok() {
                            first_error = Err(e);
                        }
                    }
                }
            }
            let (pixel_x, position) = place(x);
            let key = prefix | (u64::from(id) << 2) | u64::from(position);
            let pixel_y = snap(y);
            if let Some(image) = self.glyphs.get_mut(&key) {
                image.used = tick;
                blit(surface, clip, pixel_x, pixel_y, image, ink);
                continue;
            }
            match self.rasterize(run.face, em_key, id, position) {
                Ok(image) => {
                    let bytes = image.bytes();
                    if self.glyph_bytes + bytes > self.glyph_limit {
                        self.evict_to(self.glyph_limit.saturating_sub(bytes));
                    }
                    blit(surface, clip, pixel_x, pixel_y, &image, ink);
                    self.glyph_bytes += bytes;
                    self.glyphs.insert(key, image);
                }
                Err(e) => {
                    if first_error.is_ok() {
                        first_error = Err(e);
                    }
                }
            }
        }
        first_error
    }

    /// Draws the color layers of a glyph. `Ok(false)` means the glyph has none, so it
    /// is plain text.
    fn draw_color_glyph(
        &mut self,
        surface: Surface,
        clip: Area,
        at: (f64, f64),
        font: (u32, f32, u64),
        glyph: u16,
        color: Color,
    ) -> Result<bool, RenderError> {
        let (face, em, em_units) = font;
        let key = (u64::from(face) << 42) | (em_units << 18) | (u64::from(glyph) << 2);
        let layers = match self.color_layers.get(&key) {
            Some(found) => found.clone(),
            None => {
                let found = self.translate_color(face, em, glyph)?;
                if self.color_layers.len() >= MOST_COLOR_GLYPHS {
                    self.color_layers.clear();
                }
                self.color_layers.insert(key, found.clone());
                found
            }
        };
        let Some(layers) = layers else {
            return Ok(false);
        };
        let mut first_error = Ok(true);
        for layer in layers.iter() {
            let layer_color = match layer.color {
                None => color,
                Some(own) => Color {
                    a: ((u32::from(own.a) * u32::from(color.a) + 127) / 255) as u8,
                    ..own
                },
            };
            if layer_color.a == 0 {
                continue;
            }
            let ink = self.ink(layer_color);
            let mut ink = ink.borrow_mut();
            let ink = &mut *ink;
            let (pixel_x, position) = place(at.0 + f64::from(layer.x));
            let pixel_y = snap(at.1 + f64::from(layer.y));
            let key = (u64::from(face) << 42)
                | (em_units << 18)
                | (u64::from(layer.glyph) << 2)
                | u64::from(position);
            if let Some(image) = self.glyphs.get_mut(&key) {
                image.used = self.tick;
                blit(surface, clip, pixel_x, pixel_y, image, ink);
                continue;
            }
            match self.rasterize(face, em, layer.glyph, position) {
                Ok(image) => {
                    let bytes = image.bytes();
                    if self.glyph_bytes + bytes > self.glyph_limit {
                        self.evict_to(self.glyph_limit.saturating_sub(bytes));
                    }
                    blit(surface, clip, pixel_x, pixel_y, &image, ink);
                    self.glyph_bytes += bytes;
                    self.glyphs.insert(key, image);
                }
                Err(e) => {
                    if first_error.is_ok() {
                        first_error = Err(e);
                    }
                }
            }
        }
        first_error
    }
    fn translate_color(
        &self,
        face: u32,
        em: f32,
        glyph: u16,
    ) -> Result<Option<Rc<[Layer]>>, RenderError> {
        let factory: IDWriteFactory2 = self.factory.cast().map_err(error)?;
        let (advance, offset) = (0f32, DWRITE_GLYPH_OFFSET::default());
        let mut run = glyph_run(
            &self.faces[face as usize].face,
            em,
            &glyph,
            &advance,
            &offset,
        );
        let translated = unsafe {
            factory.TranslateColorGlyphRun(
                0.0,
                0.0,
                &run,
                None,
                DWRITE_MEASURING_MODE_NATURAL,
                None,
                0,
            )
        };
        unsafe { ManuallyDrop::drop(&mut run.fontFace) };
        let layers = match translated {
            Ok(layers) => layers,
            // DirectWrite's name for "this run has no colored glyph".
            Err(e) if e.code() == DWRITE_E_NOCOLOR => return Ok(None),
            Err(e) => return Err(error(e)),
        };
        let mut found = Vec::new();
        while unsafe { layers.MoveNext().map_err(error)? }.as_bool() {
            let layer = unsafe { &*layers.GetCurrentRun().map_err(error)? };
            let color = (layer.paletteIndex != u16::MAX).then(|| {
                let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                Color {
                    r: channel(layer.runColor.r),
                    g: channel(layer.runColor.g),
                    b: channel(layer.runColor.b),
                    a: channel(layer.runColor.a),
                }
            });
            let count = layer.glyphRun.glyphCount as usize;
            let mut pen = layer.baselineOriginX;
            for i in 0..count {
                // SAFETY: DirectWrite gives arrays of `glyphCount` values. The advance
                // and offset arrays may be missing.
                let id = unsafe { *layer.glyphRun.glyphIndices.add(i) };
                let shift = if layer.glyphRun.glyphOffsets.is_null() {
                    DWRITE_GLYPH_OFFSET::default()
                } else {
                    unsafe { *layer.glyphRun.glyphOffsets.add(i) }
                };
                found.push(Layer {
                    glyph: id,
                    x: pen + shift.advanceOffset,
                    y: layer.baselineOriginY - shift.ascenderOffset,
                    color,
                });
                if !layer.glyphRun.glyphAdvances.is_null() {
                    pen += unsafe { *layer.glyphRun.glyphAdvances.add(i) };
                }
            }
        }
        Ok(Some(found.into()))
    }
    fn rasterize(
        &mut self,
        face: u32,
        em: f32,
        glyph: u16,
        position: u8,
    ) -> Result<GlyphImage, RenderError> {
        self.counts.rasterized += 1;
        let entry = &self.faces[face as usize];
        let mut mode = unsafe {
            entry
                .face
                .GetRecommendedRenderingMode(em, 1.0, DWRITE_MEASURING_MODE_NATURAL, &self.params)
                .map_err(error)?
        };
        // The analysis cannot make outlines, and its nearest mode gives the same ink.
        if mode == DWRITE_RENDERING_MODE_OUTLINE || mode == DWRITE_RENDERING_MODE_DEFAULT {
            mode = DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC;
        }
        let (advance, offset) = (0f32, DWRITE_GLYPH_OFFSET::default());
        let mut run = glyph_run(&entry.face, em, &glyph, &advance, &offset);
        let analysis = unsafe {
            self.factory.CreateGlyphRunAnalysis(
                &run,
                1.0,
                None,
                mode,
                DWRITE_MEASURING_MODE_NATURAL,
                f32::from(position) / SUBPIXEL_POSITIONS as f32,
                0.0,
            )
        };
        unsafe { ManuallyDrop::drop(&mut run.fontFace) };
        let analysis = analysis.map_err(error)?;
        let (bounds, aliased) = unsafe {
            let bounds = analysis
                .GetAlphaTextureBounds(DWRITE_TEXTURE_CLEARTYPE_3x1)
                .map_err(error)?;
            if bounds.left < bounds.right && bounds.top < bounds.bottom {
                (bounds, false)
            } else {
                let aliased = analysis
                    .GetAlphaTextureBounds(DWRITE_TEXTURE_ALIASED_1x1)
                    .map_err(error)?;
                (aliased, true)
            }
        };
        let width = (bounds.right - bounds.left).max(0) as usize;
        let height = (bounds.bottom - bounds.top).max(0) as usize;
        let mut coverage = vec![0u8; width * height * 3];
        if width > 0 && height > 0 {
            unsafe {
                if aliased {
                    let mut plain = vec![0u8; width * height];
                    analysis
                        .CreateAlphaTexture(DWRITE_TEXTURE_ALIASED_1x1, &bounds, &mut plain)
                        .map_err(error)?;
                    for (out, v) in coverage.as_chunks_mut::<3>().0.iter_mut().zip(&plain) {
                        *out = [*v; 3];
                    }
                } else {
                    analysis
                        .CreateAlphaTexture(DWRITE_TEXTURE_CLEARTYPE_3x1, &bounds, &mut coverage)
                        .map_err(error)?;
                }
            }
            adjust_coverage(&mut coverage, self.settings);
        }
        Ok(GlyphImage {
            left: bounds.left,
            top: bounds.top,
            width: width as u32,
            height: height as u32,
            coverage: coverage.into_boxed_slice(),
            used: self.tick,
        })
    }
}

fn missing_family(name: &str) -> RenderError {
    error(format!("font not installed: {name}"))
}
/// The whole pixel and the subpixel position for a glyph at `x`.
#[inline]
fn place(x: f64) -> (i32, u8) {
    let nudged = x + 0.5 / SUBPIXEL_POSITIONS;
    let whole = nudged.floor();
    let position = ((nudged - whole) * SUBPIXEL_POSITIONS) as u8;
    (whole as i32, position.min(SUBPIXEL_POSITIONS as u8 - 1))
}
/// The face stays alive through a reference that the caller must drop.
fn glyph_run(
    face: &IDWriteFontFace,
    em: f32,
    glyph: &u16,
    advance: &f32,
    offset: &DWRITE_GLYPH_OFFSET,
) -> DWRITE_GLYPH_RUN {
    DWRITE_GLYPH_RUN {
        fontFace: ManuallyDrop::new(Some(face.clone())),
        fontEmSize: em,
        glyphCount: 1,
        glyphIndices: glyph,
        glyphAdvances: advance,
        glyphOffsets: offset,
        isSideways: BOOL(0),
        bidiLevel: 0,
    }
}
fn adjust_coverage(coverage: &mut [u8], settings: TextSettings) {
    let flat = settings.geometry == DWRITE_PIXEL_GEOMETRY_FLAT;
    let reversed = settings.geometry == DWRITE_PIXEL_GEOMETRY_BGR;
    let level = if flat {
        0.0
    } else {
        settings.cleartype_level.clamp(0.0, 1.0)
    };
    for p in coverage.as_chunks_mut::<3>().0 {
        if reversed {
            p.swap(0, 2);
        }
        if level < 1.0 {
            let average = (f32::from(p[0]) + f32::from(p[1]) + f32::from(p[2])) / 3.0;
            for v in p.iter_mut() {
                *v = (average + (f32::from(*v) - average) * level).round() as u8;
            }
        }
    }
}

/// DirectWrite's own blend, which measurements of `IDWriteBitmapRenderTarget` on this
/// PC match to within rounding, with both system and custom settings:
///
/// 1. Dark ink gets contrast: `a = c * (k + 1) / (c * k + 1)`, where `k` is the enhanced
///    contrast, reduced to 0 as the ink's lightness goes from 0.5 to 0.75.
/// 2. Each channel blends in linear light: `out^g = a * ink^g + (1 - a) * old^g`.
///
/// So white text on black gets more ink than black text on white. That is the weight
/// people see from other Windows programs that draw with DirectWrite.
fn make_ink(c: Color, settings: TextSettings, gamma: Rc<Gamma>) -> Ink {
    let channels = [c.r, c.g, c.b].map(|v| f32::from(v) / 255.0);
    let lightness = 0.30 * channels[0] + 0.59 * channels[1] + 0.11 * channels[2];
    let contrast = settings.enhanced_contrast.max(0.0) * (4.0 * (0.75 - lightness)).clamp(0.0, 1.0);
    let mut table = [0u8; 256];
    for (i, out) in table.iter_mut().enumerate() {
        let a = i as f32 / 255.0;
        let a = a * (contrast + 1.0) / (a * contrast + 1.0);
        *out = (a.clamp(0.0, 1.0) * f32::from(c.a)).round() as u8;
    }
    Ink {
        linear: [c.r, c.g, c.b].map(|v| u32::from(gamma.to_linear[v as usize])),
        alpha: [table; 3],
        backgrounds: Vec::new(),
        next_slot: 0,
        // No pixel is ever this value: the bitmap's alpha byte is always 255.
        candidate: 0,
        gamma,
    }
}

#[inline]
fn blit(surface: Surface, clip: Area, x: i32, y: i32, image: &GlyphImage, ink: &mut Ink) {
    let (width, height) = (image.width as i32, image.height as i32);
    let (left, top) = (x.saturating_add(image.left), y.saturating_add(image.top));
    let (x0, x1) = (left.max(clip.x), left.saturating_add(width).min(clip.r));
    let (y0, y1) = (top.max(clip.y), top.saturating_add(height).min(clip.b));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let count = (x1 - x0) as usize;
    let corner = surface.span(y0, x0, x0 + 1)[0];
    let table = ink.table(corner);
    let ink = &*ink;
    let flat = table.map(|t| &ink.backgrounds[t]);
    let empty = [[0u8; 256]; 3];
    let (flat_pixel, [red, green, blue]) = match flat {
        Some(t) => (t.pixel, &t.out),
        // No pixel has an alpha byte of 0, so this never matches.
        None => (0, &empty),
    };
    for row in y0..y1 {
        let start = ((row - top) as usize * width as usize + (x0 - left) as usize) * 3;
        let source = &image.coverage[start..start + count * 3];
        let target = surface.span(row, x0, x1);
        for (p, c) in target.iter_mut().zip(source.as_chunks::<3>().0) {
            if c[0] | c[1] | c[2] == 0 {
                continue;
            }
            if *p == flat_pixel {
                *p = u32::from(blue[c[2] as usize])
                    | (u32::from(green[c[1] as usize]) << 8)
                    | (u32::from(red[c[0] as usize]) << 16)
                    | 0xFF00_0000;
            } else {
                *p = ink.blend(*p, c);
            }
        }
    }
}

struct RawRun {
    face: IDWriteFontFace,
    em: f32,
    ids: Vec<u16>,
    advances: Vec<Dip>,
    offsets: Vec<Point>,
    bidi: u8,
    x: f32,
    y: f32,
}
#[implement(IDWriteTextRenderer)]
struct RunCollector {
    runs: Arc<Mutex<Vec<RawRun>>>,
}
impl IDWritePixelSnapping_Impl for RunCollector_Impl {
    fn IsPixelSnappingDisabled(&self, _: *const core::ffi::c_void) -> windows::core::Result<BOOL> {
        Ok(BOOL(1))
    }
    fn GetCurrentTransform(
        &self,
        _: *const core::ffi::c_void,
        out: *mut DWRITE_MATRIX,
    ) -> windows::core::Result<()> {
        unsafe {
            *out = DWRITE_MATRIX {
                m11: 1.0,
                m12: 0.0,
                m21: 0.0,
                m22: 1.0,
                dx: 0.0,
                dy: 0.0,
            };
        }
        Ok(())
    }
    fn GetPixelsPerDip(&self, _: *const core::ffi::c_void) -> windows::core::Result<f32> {
        Ok(1.0)
    }
}
impl IDWriteTextRenderer_Impl for RunCollector_Impl {
    fn DrawGlyphRun(
        &self,
        _: *const core::ffi::c_void,
        x: f32,
        y: f32,
        _: DWRITE_MEASURING_MODE,
        run: *const DWRITE_GLYPH_RUN,
        _: *const DWRITE_GLYPH_RUN_DESCRIPTION,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        let run = unsafe { &*run };
        let n = run.glyphCount as usize;
        let face = (*run.fontFace)
            .clone()
            .ok_or_else(|| windows::core::Error::from_hresult(HRESULT(0x80004005u32 as i32)))?;
        // SAFETY: DirectWrite gives arrays of `glyphCount` values. Offsets may be
        // missing. The y offset stays DirectWrite's ascenderOffset, as in qe-layout.
        let (ids, advances, offsets) = unsafe {
            (
                std::slice::from_raw_parts(run.glyphIndices, n).to_vec(),
                std::slice::from_raw_parts(run.glyphAdvances, n)
                    .iter()
                    .copied()
                    .map(Dip)
                    .collect(),
                if run.glyphOffsets.is_null() {
                    vec![Point::default(); n]
                } else {
                    std::slice::from_raw_parts(run.glyphOffsets, n)
                        .iter()
                        .map(|o| Point {
                            x: Dip(o.advanceOffset),
                            y: Dip(o.ascenderOffset),
                        })
                        .collect()
                },
            )
        };
        self.this
            .runs
            .lock()
            .map_err(|_| windows::core::Error::from_hresult(HRESULT(0x80004005u32 as i32)))?
            .push(RawRun {
                face,
                em: run.fontEmSize,
                ids,
                advances,
                offsets,
                bidi: run.bidiLevel as u8,
                x,
                y,
            });
        Ok(())
    }
    fn DrawUnderline(
        &self,
        _: *const core::ffi::c_void,
        _: f32,
        _: f32,
        _: *const DWRITE_UNDERLINE,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn DrawStrikethrough(
        &self,
        _: *const core::ffi::c_void,
        _: f32,
        _: f32,
        _: *const DWRITE_STRIKETHROUGH,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn DrawInlineObject(
        &self,
        _: *const core::ffi::c_void,
        _: f32,
        _: f32,
        _: windows::core::Ref<IDWriteInlineObject>,
        _: BOOL,
        _: BOOL,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}
