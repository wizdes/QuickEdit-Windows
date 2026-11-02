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
    collections::HashMap,
    hash::{BuildHasherDefault, Hasher},
    mem::ManuallyDrop,
    rc::Rc,
    sync::{Arc, Mutex},
};
use windows::{
    Win32::{
        Foundation::{DWRITE_E_NOCOLOR, RECT},
        Graphics::{DirectWrite::*, Gdi::HMONITOR},
    },
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
/// A screen of emoji uses over 64 layer colors. With too few slots, each frame would
/// rebuild inks in turn. One ink costs at most about 4 KiB.
const MOST_INKS: usize = 256;
const MOST_COLOR_GLYPHS: usize = 4096;
/// Coverage is made for this many positions across one pixel.
const SUBPIXEL_POSITIONS: f64 = 4.0;
/// Glyph edges spread past the outline by the ClearType filter.
const FILTER_REACH: f64 = 3.0;
/// Past this size, a rendering mode can stop giving coverage.
const LARGE_EM_PIXELS: f32 = 256.0;
/// Keeps an icon's glyph apart from a ClearType glyph of the same font, size and id.
const GRAY_KEY_BIT: u32 = 63;
/// A run stops at the clip's far edge plus this many font sizes, so that its cost is
/// its visible part. A glyph offset further sideways than this past the edge is not
/// drawn. Shaped marks move far less than this.
const MOST_SIDEWAYS_EMS: f64 = 8.0;
/// The largest font size that a glyph can have, in device pixels.
const MOST_EM_PIXELS: f64 = 16384.0;
/// The most memory for the coverage of one glyph.
const MOST_GLYPH_BYTES: usize = 256 * 1024 * 1024;

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
        // Near-black bytes can share one light value at a high gamma. No light must
        // still mean level 0, and full light level 255.
        from_linear[0] = 0;
        from_linear[65535] = 255;
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
    key: u32,
    bytes: [u32; 3],
    linear: [u32; 3],
    /// The blend alpha for each coverage value and channel.
    alpha: [[u8; 256]; 3],
    backgrounds: Vec<OnBackground>,
    next_slot: usize,
    candidate: Option<u32>,
    gamma: Rc<Gamma>,
}
impl Ink {
    #[inline]
    fn channel(&self, c: usize, coverage: u8, destination: u32) -> u32 {
        let alpha = u32::from(self.alpha[c][coverage as usize]);
        // The ends are exact: nothing changes with no alpha, and full alpha gives the
        // ink itself. Light values alone cannot promise that for near-black bytes.
        if alpha == 0 {
            return destination;
        }
        if alpha == 255 {
            return self.bytes[c];
        }
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
        if self.candidate != Some(pixel) {
            self.candidate = Some(pixel);
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
#[derive(Clone, Copy)]
pub(crate) struct RunRef<'a> {
    pub face: u32,
    pub em: f32,
    pub bidi: u8,
    pub ids: &'a [u16],
    pub advances: &'a [Dip],
    pub offsets: &'a [Point],
    /// Icons: grayscale coverage with no color fringes, on a whole pixel. Its glyphs have
    /// their own cache entries.
    pub gray: bool,
}

/// A face at one size. `em` is the size that `em_units` stands for, in pixels.
#[derive(Clone, Copy)]
struct GlyphFont {
    face: u32,
    em: f32,
    em_units: u64,
    gray: bool,
}

struct Reach {
    left: f64,
    right: f64,
    above: f64,
    below: f64,
}

impl RunRef<'_> {
    fn without_offsets(&self) -> Self {
        Self {
            offsets: &[],
            ..*self
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Counts {
    pub rasterized: u64,
    pub drawn: u64,
    pub walked: u64,
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
    /// A fixed list of inks. A draw holds only a slot number, and borrows the ink for
    /// one blit at a time, so a nested use of the same ink cannot conflict.
    inks: Vec<Ink>,
    ink_slots: Map<u32, usize>,
    next_ink: usize,
    gamma: Rc<Gamma>,
    no_ligatures: Option<IDWriteTypography>,
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
            inks: Vec::new(),
            ink_slots: Map::default(),
            next_ink: 0,
            gamma: Rc::new(Gamma::new(settings.gamma)),
            no_ligatures: None,
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
        self.ink_slots.clear();
        self.next_ink = 0;
        self.gamma = Rc::new(Gamma::new(v.gamma));
        Ok(())
    }
    /// Reads the settings that Windows has now for this monitor. It returns whether
    /// they differ from the ones in use, and uses the new ones if so.
    pub fn refresh_settings(&mut self, monitor: HMONITOR) -> Result<bool, RenderError> {
        let params = unsafe {
            self.factory
                .CreateMonitorRenderingParams(monitor)
                .map_err(error)?
        };
        let now = system_settings(&params);
        if now == self.settings {
            return Ok(false);
        }
        self.set_settings(now)?;
        Ok(true)
    }
    pub fn cache_sizes(&self) -> crate::CacheSizes {
        crate::CacheSizes {
            shape_bytes: self.shaped_bytes,
            inks: self.inks.len(),
            color_glyphs: self.color_layers.len(),
            corner_bytes: 0,
        }
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
            None => self.layout_shape(family, face, size, text)?,
        };
        let shaped = Rc::new(shaped);
        // A string over the whole limit is drawn and not kept.
        if shaped.bytes > SHAPE_CACHE_BYTES {
            return Ok(shaped);
        }
        if self.shaped_bytes + shaped.bytes > SHAPE_CACHE_BYTES {
            self.shaped.clear();
            self.shaped_bytes = 0;
        }
        self.shaped_bytes += shaped.bytes;
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
    fn layout_shape(
        &mut self,
        family: &str,
        face: u32,
        size: f32,
        text: &str,
    ) -> Result<Shaped, RenderError> {
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
        if self.faces[face as usize].is_monospaced {
            self.turn_off_ascii_ligatures(&layout, &units)?;
        }
        // DirectWrite fills nothing in when the array is too short, so ask with the
        // size that the layout itself reports.
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe { layout.GetMetrics(&mut metrics).map_err(error)? };
        let mut lines = vec![DWRITE_LINE_METRICS::default(); metrics.lineCount.max(1) as usize];
        let mut count = 0;
        unsafe {
            layout
                .GetLineMetrics(Some(&mut lines), &mut count)
                .map_err(error)?;
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
                y: run.y - lines[0].baseline,
                ids: run.ids,
                advances: run.advances,
                offsets: run.offsets,
            });
        }
        Ok(Shaped { runs, bytes })
    }

    /// qe-layout's rule: a fixed-pitch font has no ligatures in ASCII, on either path.
    /// Text in other scripts keeps its own features.
    fn turn_off_ascii_ligatures(
        &mut self,
        layout: &IDWriteTextLayout,
        units: &[u16],
    ) -> Result<(), RenderError> {
        let typography = match &self.no_ligatures {
            Some(typography) => typography.clone(),
            None => {
                let typography = unsafe { self.factory.CreateTypography().map_err(error)? };
                for tag in [
                    DWRITE_FONT_FEATURE_TAG_STANDARD_LIGATURES,
                    DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_LIGATURES,
                    DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_ALTERNATES,
                ] {
                    unsafe {
                        typography
                            .AddFontFeature(DWRITE_FONT_FEATURE {
                                nameTag: tag,
                                parameter: 0,
                            })
                            .map_err(error)?;
                    }
                }
                self.no_ligatures = Some(typography.clone());
                typography
            }
        };
        let mut at = 0;
        while at < units.len() {
            if units[at] >= 0x80 {
                at += 1;
                continue;
            }
            let begin = at;
            while at < units.len() && units[at] < 0x80 {
                at += 1;
            }
            let range = DWRITE_TEXT_RANGE {
                startPosition: begin as u32,
                length: (at - begin) as u32,
            };
            unsafe { layout.SetTypography(&typography, range).map_err(error)? };
        }
        Ok(())
    }

    /// The slot of an ink. The list stays at a fixed limit, so a new color can take the
    /// slot of an old one. A slot number is good until the next call of this function.
    fn ink_slot(&mut self, c: Color) -> usize {
        let key = u32::from_le_bytes([c.r, c.g, c.b, c.a]);
        if let Some(slot) = self.ink_slots.get(&key) {
            return *slot;
        }
        let ink = make_ink(key, c, self.settings, self.gamma.clone());
        let slot = if self.inks.len() < MOST_INKS {
            self.inks.push(ink);
            self.inks.len() - 1
        } else {
            let slot = self.next_ink;
            self.next_ink = (slot + 1) % MOST_INKS;
            self.ink_slots.remove(&self.inks[slot].key);
            self.inks[slot] = ink;
            slot
        };
        self.ink_slots.insert(key, slot);
        slot
    }

    /// The rectangle that one shaped text can ink, around its start, in device pixels:
    /// left, top, right and bottom.
    pub fn shaped_extents(&self, shaped: &Shaped, scale: f64) -> Option<(f64, f64, f64, f64)> {
        let mut all = None::<(f64, f64, f64, f64)>;
        for run in &shaped.runs {
            let (x, y) = (f64::from(run.x) * scale, f64::from(run.y) * scale);
            let one = self.run_extents(
                &RunRef {
                    face: run.face,
                    em: run.em,
                    bidi: run.bidi,
                    ids: &run.ids,
                    advances: &run.advances,
                    offsets: &run.offsets,
                    gray: false,
                },
                scale,
            );
            let one = (one.0 + x, one.1 + y, one.2 + x, one.3 + y);
            all = Some(match all {
                None => one,
                Some(b) => (
                    b.0.min(one.0),
                    b.1.min(one.1),
                    b.2.max(one.2),
                    b.3.max(one.3),
                ),
            });
        }
        all
    }
    /// The rectangle that one glyph run can ink, around its start, in device pixels:
    /// left, top, right and bottom. Glyphs lie along the advances, and ink stays within
    /// the font's box around each glyph's origin.
    pub fn run_extents(&self, run: &RunRef<'_>, scale: f64) -> (f64, f64, f64, f64) {
        let reach = self.reach(run, scale);
        let total: f64 = run.advances.iter().map(|a| f64::from(a.0)).sum::<f64>() * scale;
        if run.bidi & 1 == 1 {
            (-total - reach.left, -reach.above, reach.right, reach.below)
        } else {
            (-reach.left, -reach.above, total + reach.right, reach.below)
        }
    }
    fn reach(&self, run: &RunRef<'_>, scale: f64) -> Reach {
        let entry = &self.faces[run.face as usize];
        let em_device = f64::from(run.em) * scale;
        let (mut most_x, mut most_up, mut most_down) = (0f32, 0f32, 0f32);
        for o in run.offsets {
            most_x = most_x.max(o.x.0.abs());
            most_up = most_up.max(o.y.0);
            most_down = most_down.max(-o.y.0);
        }
        Reach {
            left: f64::from(-entry.left) * em_device + f64::from(most_x) * scale + FILTER_REACH,
            right: f64::from(entry.right) * em_device + f64::from(most_x) * scale + FILTER_REACH,
            above: f64::from(entry.up) * em_device + f64::from(most_up) * scale + FILTER_REACH,
            below: f64::from(entry.down) * em_device + f64::from(most_down) * scale + FILTER_REACH,
        }
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
                    gray: false,
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
        if em_device > MOST_EM_PIXELS {
            return Err(too_large(em_device));
        }
        // No scan of the whole run before this: each glyph below is tested with its own
        // offset, and the walk stops past the clip.
        let reach = self.reach(&run.without_offsets(), scale);
        let stop_past = MOST_SIDEWAYS_EMS * em_device;
        let is_color = self.faces[run.face as usize].is_color;
        let rtl = run.bidi & 1 == 1;
        let direction = if rtl { -1.0 } else { 1.0 };
        let (clip_left, clip_right) = (f64::from(clip.x), f64::from(clip.r));
        let mut slot = self.ink_slot(color);
        let font = GlyphFont {
            face: run.face,
            em: em_units as f32 / 64.0,
            em_units,
            gray: run.gray,
        };
        let mut pen = at.0;
        let mut first_error = Ok(());
        for i in 0..run.ids.len() {
            let offset = run.offsets[i];
            if !(run.advances[i].0.is_finite() && offset.x.0.is_finite() && offset.y.0.is_finite())
            {
                return Err(error(
                    "a GlyphRun has a value that is not a finite number, so the rest of it was skipped",
                ));
            }
            let advance = f64::from(run.advances[i].0) * scale;
            if rtl {
                pen -= advance;
                if pen + reach.right + stop_past < clip_left {
                    break;
                }
            } else if pen - reach.left - stop_past > clip_right {
                break;
            }
            self.counts.walked += 1;
            let glyph_pen = pen;
            if !rtl {
                pen += advance;
            }
            let x = glyph_pen + direction * f64::from(offset.x.0) * scale;
            let y = at.1 - f64::from(offset.y.0) * scale;
            if x + reach.right < clip_left
                || x - reach.left > clip_right
                || y + reach.below < f64::from(clip.y)
                || y - reach.above > f64::from(clip.b)
            {
                continue;
            }
            let id = run.ids[i];
            self.counts.drawn += 1;
            if is_color {
                let drew = self.draw_color_glyph(surface, clip, (x, y), font, id, color);
                // The layers may have taken the slot of this run's ink.
                slot = self.ink_slot(color);
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
            let stamped = self.stamp(surface, clip, (x, y), font, id, slot);
            if first_error.is_ok() {
                first_error = stamped;
            }
        }
        first_error
    }

    /// Blends one glyph with the ink in `slot`, through the glyph cache.
    #[inline]
    fn stamp(
        &mut self,
        surface: Surface,
        clip: Area,
        at: (f64, f64),
        font: GlyphFont,
        glyph: u16,
        slot: usize,
    ) -> Result<(), RenderError> {
        // An icon's stems stay even only on a whole pixel.
        let (pixel_x, position) = if font.gray {
            (snap(at.0), 0)
        } else {
            place(at.0)
        };
        let pixel_y = snap(at.1);
        let key = (u64::from(font.gray) << GRAY_KEY_BIT)
            | (u64::from(font.face) << 42)
            | (font.em_units << 18)
            | (u64::from(glyph) << 2)
            | u64::from(position);
        if let Some(image) = self.glyphs.get_mut(&key) {
            image.used = self.tick;
            blit(surface, clip, pixel_x, pixel_y, image, &mut self.inks[slot]);
            return Ok(());
        }
        let image = self.rasterize(font, glyph, position)?;
        blit(
            surface,
            clip,
            pixel_x,
            pixel_y,
            &image,
            &mut self.inks[slot],
        );
        let bytes = image.bytes();
        // A glyph larger than the whole limit is drawn and not kept.
        if bytes <= self.glyph_limit {
            if self.glyph_bytes + bytes > self.glyph_limit {
                self.evict_to(self.glyph_limit - bytes);
            }
            self.glyph_bytes += bytes;
            self.glyphs.insert(key, image);
        }
        Ok(())
    }

    /// Draws the color layers of a glyph. `Ok(false)` means the glyph has none, so it
    /// is plain text.
    fn draw_color_glyph(
        &mut self,
        surface: Surface,
        clip: Area,
        at: (f64, f64),
        font: GlyphFont,
        glyph: u16,
        color: Color,
    ) -> Result<bool, RenderError> {
        let key = (u64::from(font.face) << 42) | (font.em_units << 18) | (u64::from(glyph) << 2);
        let layers = match self.color_layers.get(&key) {
            Some(found) => found.clone(),
            None => {
                let found = self.translate_color(font.face, font.em, glyph)?;
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
            let slot = self.ink_slot(layer_color);
            let at = (at.0 + f64::from(layer.x), at.1 + f64::from(layer.y));
            if let Err(e) = self.stamp(surface, clip, at, font, layer.glyph, slot)
                && first_error.is_ok()
            {
                first_error = Err(e);
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
        font: GlyphFont,
        glyph: u16,
        position: u8,
    ) -> Result<GlyphImage, RenderError> {
        let (face, em) = (font.face, font.em);
        self.counts.rasterized += 1;
        let entry = &self.faces[face as usize];
        let mut mode = unsafe {
            entry
                .face
                .GetRecommendedRenderingMode(em, 1.0, DWRITE_MEASURING_MODE_NATURAL, &self.params)
                .map_err(error)?
        };
        // The system's default asks for outlines even at ordinary sizes, and an analysis
        // cannot make them. Symmetric is the mode with smoothing in both directions, like
        // an outline. A test compares the result with DirectWrite's own drawing. Up to 300
        // pixels no pixel is far off. At 600 to 1200 pixels, 2% to 3% of the ink's pixels
        // differ by over 96 levels, all near edges, and total ink stays within 0.3%.
        if mode == DWRITE_RENDERING_MODE_OUTLINE || mode == DWRITE_RENDERING_MODE_DEFAULT {
            mode = DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC;
        }
        // Each mode gives empty coverage past its own size, with no error. Measured on
        // Windows build 26300: symmetric stops near 700 pixels, natural near 1500, and
        // aliased does not stop. So large glyphs step down the list. The last step has
        // hard edges, at sizes where one pixel of edge hardly shows.
        let large = em >= LARGE_EM_PIXELS;
        let steps = [
            Some(mode),
            large.then_some(DWRITE_RENDERING_MODE_NATURAL),
            large.then_some(DWRITE_RENDERING_MODE_ALIASED),
        ];
        let (advance, offset) = (0f32, DWRITE_GLYPH_OFFSET::default());
        let mut run = glyph_run(&entry.face, em, &glyph, &advance, &offset);
        let mut found = None;
        let mut failure = None;
        if font.gray {
            match unsafe { gray_bounds(&self.factory, &run) } {
                Ok(step) => {
                    if step.1.left < step.1.right && step.1.top < step.1.bottom {
                        found = Some(step);
                    }
                }
                Err(e) => failure = Some(e),
            }
        }
        let tried_gray = found.is_some() || failure.is_some();
        for mode in steps.into_iter().flatten().filter(|_| !tried_gray) {
            let step = unsafe {
                texture_bounds(
                    &self.factory,
                    &run,
                    mode,
                    f32::from(position) / SUBPIXEL_POSITIONS as f32,
                )
            };
            match step {
                Ok((analysis, bounds, aliased)) => {
                    let has_ink = bounds.left < bounds.right && bounds.top < bounds.bottom;
                    found = Some((analysis, bounds, aliased));
                    if has_ink {
                        break;
                    }
                }
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
        unsafe { ManuallyDrop::drop(&mut run.fontFace) };
        if let Some(e) = failure {
            return Err(e);
        }
        let Some((analysis, bounds, aliased)) = found else {
            return Err(error("no rendering mode to try"));
        };
        let width = (bounds.right - bounds.left).max(0) as usize;
        let height = (bounds.bottom - bounds.top).max(0) as usize;
        if width.saturating_mul(height).saturating_mul(3) > MOST_GLYPH_BYTES {
            return Err(too_large(f64::from(em)));
        }
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
            if font.gray {
                // Past the size where the grayscale mode stops, a ClearType mode made this.
                let flat = TextSettings {
                    cleartype_level: 0.0,
                    ..self.settings
                };
                adjust_coverage(&mut coverage, flat);
            } else {
                adjust_coverage(&mut coverage, self.settings);
            }
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

/// A new analysis for this mode, its coverage rectangle, and whether that coverage is
/// aliased.
///
/// # Safety
/// `run` must be a valid glyph run.
unsafe fn texture_bounds(
    factory: &IDWriteFactory,
    run: &DWRITE_GLYPH_RUN,
    mode: DWRITE_RENDERING_MODE,
    x: f32,
) -> Result<(IDWriteGlyphRunAnalysis, RECT, bool), RenderError> {
    let analysis = unsafe {
        factory
            .CreateGlyphRunAnalysis(run, 1.0, None, mode, DWRITE_MEASURING_MODE_NATURAL, x, 0.0)
            .map_err(error)?
    };
    let bounds = unsafe {
        analysis
            .GetAlphaTextureBounds(DWRITE_TEXTURE_CLEARTYPE_3x1)
            .map_err(error)?
    };
    if bounds.left < bounds.right && bounds.top < bounds.bottom {
        return Ok((analysis, bounds, false));
    }
    let aliased = unsafe {
        analysis
            .GetAlphaTextureBounds(DWRITE_TEXTURE_ALIASED_1x1)
            .map_err(error)?
    };
    Ok((analysis, aliased, true))
}
/// Grayscale coverage with smoothing in both directions, which is what gives an icon's
/// stems an even look. Measured for Segoe Fluent Icons at 12 pixels: the + is 11x11 with
/// this mode, and 11x10 with the natural, GDI natural or GDI classic modes. Measuring
/// mode and grid fit made no difference to the size.
///
/// # Safety
/// `run` must be a valid glyph run.
unsafe fn gray_bounds(
    factory: &IDWriteFactory,
    run: &DWRITE_GLYPH_RUN,
) -> Result<(IDWriteGlyphRunAnalysis, RECT, bool), RenderError> {
    let factory: IDWriteFactory2 = factory.cast().map_err(error)?;
    let analysis = unsafe {
        factory
            .CreateGlyphRunAnalysis(
                run,
                None,
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_DISABLED,
                DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                0.0,
                0.0,
            )
            .map_err(error)?
    };
    let bounds = unsafe {
        analysis
            .GetAlphaTextureBounds(DWRITE_TEXTURE_ALIASED_1x1)
            .map_err(error)?
    };
    Ok((analysis, bounds, true))
}
fn too_large(em_device: f64) -> RenderError {
    error(format!(
        "a glyph at {em_device:.0} pixels is too large to draw"
    ))
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
fn make_ink(key: u32, c: Color, settings: TextSettings, gamma: Rc<Gamma>) -> Ink {
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
        key,
        // With partial alpha in the color, full coverage is not the ink itself. The
        // alpha table then never reaches 255, so this stays unused.
        bytes: [c.r, c.g, c.b].map(u32::from),
        linear: [c.r, c.g, c.b].map(|v| u32::from(gamma.to_linear[v as usize])),
        alpha: [table; 3],
        backgrounds: Vec::new(),
        next_slot: 0,
        candidate: None,
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
    // Pixels that nothing has painted yet have alpha 0 and color 0. They blend as black
    // and come out opaque, the same as a pixel painted black, on either branch here.
    for row in y0..y1 {
        let start = ((row - top) as usize * width as usize + (x0 - left) as usize) * 3;
        let source = image.coverage[start..start + count * 3].as_chunks::<3>().0;
        let target = surface.span(row, x0, x1);
        match table {
            Some(slot) => {
                let flat = &ink.backgrounds[slot];
                let [red, green, blue] = &flat.out;
                for (p, c) in target.iter_mut().zip(source) {
                    if c[0] | c[1] | c[2] == 0 {
                        continue;
                    }
                    *p = if *p == flat.pixel {
                        u32::from(blue[c[2] as usize])
                            | (u32::from(green[c[1] as usize]) << 8)
                            | (u32::from(red[c[0] as usize]) << 16)
                            | 0xFF00_0000
                    } else {
                        ink.blend(*p, c)
                    };
                }
            }
            None => {
                for (p, c) in target.iter_mut().zip(source) {
                    if c[0] | c[1] | c[2] != 0 {
                        *p = ink.blend(*p, c);
                    }
                }
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
