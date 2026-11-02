//! Text layout in device-independent pixels.
#![allow(hidden_glob_reexports)]
use qe_api::{
    ByteOffset, Dip, Layout, LayoutGlyphRun, LayoutRow, LineIndex, LineLayout, Point, Rect,
    TextChange, TextMeasurer, TextMetrics, TextSnapshot,
};
extern crate self as windows_core;
use std::ffi::c_void;
use std::result::Result;
use std::{
    collections::{HashMap, VecDeque},
    hash::{Hash, Hasher},
    ops::Range,
    sync::{Arc, Mutex, OnceLock},
};
pub use windows::core::*;
use windows::{
    Win32::Graphics::DirectWrite::*,
    core::{BOOL, PCWSTR, implement},
};

type CollectedRun = (u32, u32, u32, Vec<u16>, Vec<Dip>, Vec<Point>, f32);
static FACES: OnceLock<Mutex<Vec<IDWriteFontFace>>> = OnceLock::new();
fn register_face(face: &IDWriteFontFace) -> u32 {
    let mut faces = FACES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .expect("font face registry poisoned");
    if let Some(at) = faces.iter().position(|f| f.as_raw() == face.as_raw()) {
        return at as u32 + 1;
    }
    faces.push(face.clone());
    faces.len() as u32
}
/// Resolve the face of a DirectWrite glyph run.
pub fn font_face(font_id: u32) -> Option<IDWriteFontFace> {
    FACES
        .get()?
        .lock()
        .ok()?
        .get(font_id.checked_sub(1)? as usize)
        .cloned()
}
#[implement(IDWriteTextRenderer)]
struct RunCollector {
    runs: Arc<Mutex<Vec<CollectedRun>>>,
}
impl IDWritePixelSnapping_Impl for RunCollector_Impl {
    fn IsPixelSnappingDisabled(&self, _: *const c_void) -> windows::core::Result<BOOL> {
        Ok(BOOL(1))
    }
    fn GetCurrentTransform(
        &self,
        _: *const c_void,
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
    fn GetPixelsPerDip(&self, _: *const c_void) -> windows::core::Result<f32> {
        Ok(1.0)
    }
}
impl IDWriteTextRenderer_Impl for RunCollector_Impl {
    fn DrawGlyphRun(
        &self,
        _: *const c_void,
        _: f32,
        y: f32,
        _: DWRITE_MEASURING_MODE,
        run: *const DWRITE_GLYPH_RUN,
        desc: *const DWRITE_GLYPH_RUN_DESCRIPTION,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        let run = unsafe { &*run };
        let desc = unsafe { &*desc };
        let n = run.glyphCount as usize;
        let ids = unsafe { std::slice::from_raw_parts(run.glyphIndices, n) }.to_vec();
        let advances = unsafe { std::slice::from_raw_parts(run.glyphAdvances, n) }
            .iter()
            .copied()
            .map(Dip)
            .collect();
        let offsets = if run.glyphOffsets.is_null() {
            vec![Point::default(); n]
        } else {
            unsafe { std::slice::from_raw_parts(run.glyphOffsets, n) }
                .iter()
                .map(|o| Point {
                    x: Dip(o.advanceOffset),
                    y: Dip(o.ascenderOffset),
                })
                .collect()
        };
        let font_id = (*run.fontFace).as_ref().map(register_face).unwrap_or(0);
        self.this
            .runs
            .lock()
            .map_err(|_| {
                windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32))
            })?
            .push((
                desc.textPosition,
                desc.stringLength,
                font_id,
                ids,
                advances,
                offsets,
                y,
            ));
        Ok(())
    }
    fn DrawUnderline(
        &self,
        _: *const c_void,
        _: f32,
        _: f32,
        _: *const DWRITE_UNDERLINE,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn DrawStrikethrough(
        &self,
        _: *const c_void,
        _: f32,
        _: f32,
        _: *const DWRITE_STRIKETHROUGH,
        _: windows::core::Ref<windows::core::IUnknown>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn DrawInlineObject(
        &self,
        _: *const c_void,
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

static FACTORY: OnceLock<Result<IDWriteFactory, String>> = OnceLock::new();
fn factory() -> Result<&'static IDWriteFactory, String> {
    FACTORY
        .get_or_init(|| unsafe {
            DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

struct Font {
    format: IDWriteTextFormat,
    id: u32,
    ascii: [u16; 128],
    fixed_width: bool,
    advance: f32,
    height: f32,
    baseline: f32,
    family: String,
}
#[derive(Clone, Eq, PartialEq, Hash)]
struct FontKey {
    family: String,
    size: u32,
    weight: i32,
}
static FONTS: OnceLock<Mutex<HashMap<FontKey, Arc<Font>>>> = OnceLock::new();
fn font(family: &str, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<Arc<Font>, String> {
    let key = FontKey {
        family: family.into(),
        size: size.to_bits(),
        weight: weight.0,
    };
    let cache = FONTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(f) = cache.lock().map_err(|e| e.to_string())?.get(&key) {
        return Ok(f.clone());
    }
    let f = factory()?;
    let mut collection = None;
    unsafe {
        f.GetSystemFontCollection(&mut collection, false)
            .map_err(|e| e.to_string())?;
    }
    let collection = collection.ok_or("no font collection")?;
    let mut index = 0;
    let mut exists = BOOL(0);
    let mut chosen = family.to_owned();
    unsafe {
        collection
            .FindFamilyName(PCWSTR(wide(&chosen).as_ptr()), &mut index, &mut exists)
            .map_err(|e| e.to_string())?;
    }
    if !exists.as_bool() {
        chosen = "Consolas".into();
        unsafe {
            collection
                .FindFamilyName(PCWSTR(wide(&chosen).as_ptr()), &mut index, &mut exists)
                .map_err(|e| e.to_string())?;
        }
        if !exists.as_bool() {
            return Err("requested font and Consolas missing".into());
        }
    }
    let font_obj = unsafe {
        collection
            .GetFontFamily(index)
            .map_err(|e| e.to_string())?
            .GetFirstMatchingFont(weight, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL)
            .map_err(|e| e.to_string())?
    };
    let face = unsafe { font_obj.CreateFontFace().map_err(|e| e.to_string())? };
    let fixed_width = font_obj
        .cast::<IDWriteFont1>()
        .ok()
        .is_some_and(|f| unsafe { f.IsMonospacedFont().as_bool() });
    let format = unsafe {
        f.CreateTextFormat(
            PCWSTR(wide(&chosen).as_ptr()),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            PCWSTR(wide("en-US").as_ptr()),
        )
        .map_err(|e| e.to_string())?
    };
    let points: Vec<u32> = (0..128).collect();
    let mut ascii = [0; 128];
    unsafe {
        face.GetGlyphIndices(points.as_ptr(), 128, ascii.as_mut_ptr())
            .map_err(|e| e.to_string())?;
    }
    let mut fm = DWRITE_FONT_METRICS::default();
    let mut gm = DWRITE_GLYPH_METRICS::default();
    unsafe {
        face.GetMetrics(&mut fm);
        face.GetDesignGlyphMetrics(&ascii[32], 1, &mut gm, false)
            .map_err(|e| e.to_string())?;
    }
    let scale = size / fm.designUnitsPerEm as f32;
    let result = Arc::new(Font {
        format,
        id: register_face(&face),
        ascii,
        fixed_width,
        advance: gm.advanceWidth as f32 * scale,
        height: (fm.ascent as f32 + fm.descent as f32 + fm.lineGap as f32) * scale,
        baseline: fm.ascent as f32 * scale,
        family: chosen,
    });
    cache
        .lock()
        .map_err(|e| e.to_string())?
        .insert(key, result.clone());
    Ok(result)
}

#[derive(Clone)]
struct Stop {
    byte: ByteOffset,
    x: f32,
    row: usize,
}
#[derive(Default)]
struct HitMap {
    map: HashMap<u64, Vec<Stop>>,
    order: VecDeque<u64>,
}
pub struct DirectWriteLayout {
    font: Arc<Font>,
    wrap: bool,
    force_shaping: bool,
    tab_width: usize,
    hits: Mutex<HitMap>,
}
impl DirectWriteLayout {
    pub fn new() -> Result<Self, String> {
        Self::with_font("Cascadia Mono", 10.0, DWRITE_FONT_WEIGHT_NORMAL)
    }
    pub fn with_font(
        family: &str,
        points: f32,
        weight: DWRITE_FONT_WEIGHT,
    ) -> Result<Self, String> {
        Ok(Self {
            font: font(family, points * 96.0 / 72.0, weight)?,
            wrap: true,
            force_shaping: false,
            tab_width: 8,
            hits: Mutex::new(HitMap::default()),
        })
    }
    pub fn with_wrap(mut self, value: bool) -> Self {
        self.wrap = value;
        self
    }
    pub fn with_shaping(mut self, value: bool) -> Self {
        self.force_shaping = value;
        self
    }
    pub fn with_tab_width(mut self, value: usize) -> Self {
        self.tab_width = value.max(1);
        self
    }
    pub fn font_family(&self) -> &str {
        &self.font.family
    }
    pub fn cell_width(&self) -> Dip {
        Dip(self.font.advance)
    }
    fn remember(&self, layout: &LineLayout, stops: Vec<Stop>) {
        if let Ok(mut h) = self.hits.lock() {
            let key = signature(layout);
            if !h.map.contains_key(&key) {
                h.order.push_back(key);
            }
            h.map.insert(key, stops);
            while h.order.len() > 128 {
                if let Some(k) = h.order.pop_front() {
                    h.map.remove(&k);
                }
            }
        }
    }
    fn rebase_hits(&self, old: &LineLayout, new: &LineLayout, delta: isize) {
        if let Ok(mut hits) = self.hits.lock()
            && let Some(mut stops) = hits.map.get(&signature(old)).cloned()
        {
            for stop in &mut stops {
                stop.byte = ByteOffset(
                    stop.byte
                        .0
                        .checked_add_signed(delta)
                        .expect("valid cached byte shift"),
                );
            }
            let key = signature(new);
            if !hits.map.contains_key(&key) {
                hits.order.push_back(key);
            }
            hits.map.insert(key, stops);
            while hits.order.len() > 128 {
                if let Some(k) = hits.order.pop_front() {
                    hits.map.remove(&k);
                }
            }
        }
    }
    fn content(
        &self,
        s: &str,
        start: ByteOffset,
        line: LineIndex,
        width: Dip,
    ) -> Option<LineLayout> {
        let width_needed = s.as_bytes().iter().fold(0.0, |x, ch| {
            if *ch == b'\t' {
                let step = self.font.advance * self.tab_width as f32;
                ((x / step).floor() + 1.0) * step
            } else {
                x + self.font.advance
            }
        });
        if !self.force_shaping
            && self.font.fixed_width
            && s.is_ascii()
            && (!self.wrap || width_needed <= width.0)
        {
            self.fast(s.as_bytes(), start, line)
        } else {
            self.slow(s, start, line, width)
        }
    }
    fn fast(&self, s: &[u8], start: ByteOffset, line: LineIndex) -> Option<LineLayout> {
        let mut ids = Vec::with_capacity(s.len());
        let mut advances = Vec::with_capacity(s.len());
        let mut stops = Vec::with_capacity(s.len() + 1);
        let mut bytes = Vec::with_capacity(s.len() + 1);
        let mut x = 0.0;
        for (i, ch) in s.iter().copied().enumerate() {
            let byte = ByteOffset(start.0 + i);
            stops.push(Stop { byte, x, row: 0 });
            bytes.push(byte);
            ids.push(self.font.ascii[ch as usize]);
            let a = if ch == b'\t' {
                let step = self.font.advance * self.tab_width as f32;
                ((x / step).floor() + 1.0) * step - x
            } else {
                self.font.advance
            };
            advances.push(Dip(a));
            x += a;
        }
        let end = ByteOffset(start.0 + s.len());
        stops.push(Stop {
            byte: end,
            x,
            row: 0,
        });
        bytes.push(end);
        let result = LineLayout {
            line,
            height: Dip(self.font.height),
            rows: vec![LayoutRow {
                range: start..end,
                y: Dip(0.0),
                height: Dip(self.font.height),
                glyph_runs: vec![LayoutGlyphRun {
                    range: start..end,
                    font_id: self.font.id,
                    glyph_ids: ids,
                    advances,
                    offsets: vec![Point::default(); s.len()],
                }],
                caret_stops: bytes,
            }],
        };
        self.remember(&result, stops);
        Some(result)
    }
    fn slow(&self, s: &str, start: ByteOffset, line: LineIndex, width: Dip) -> Option<LineLayout> {
        let utf16: Vec<u16> = s.encode_utf16().collect();
        let l = unsafe {
            factory()
                .ok()?
                .CreateTextLayout(
                    &utf16,
                    &self.font.format,
                    if self.wrap {
                        width.0.max(1.0)
                    } else {
                        1_000_000.0
                    },
                    1_000_000.0,
                )
                .ok()?
        };
        unsafe {
            l.SetWordWrapping(if self.wrap {
                DWRITE_WORD_WRAPPING_WRAP
            } else {
                DWRITE_WORD_WRAPPING_NO_WRAP
            })
            .ok()?;
            l.SetIncrementalTabStop(self.font.advance * self.tab_width as f32)
                .ok()?;
        }
        let mut tm = DWRITE_TEXT_METRICS::default();
        unsafe {
            l.GetMetrics(&mut tm).ok()?;
        }
        let mut count = 0;
        let mut lm = vec![DWRITE_LINE_METRICS::default(); tm.lineCount.max(1) as usize];
        unsafe {
            l.GetLineMetrics(Some(&mut lm), &mut count).ok()?;
        }
        lm.truncate(count as usize);
        let mut cc = 0;
        let mut cm = vec![DWRITE_CLUSTER_METRICS::default(); utf16.len().max(1)];
        unsafe {
            l.GetClusterMetrics(Some(&mut cm), &mut cc).ok()?;
        }
        cm.truncate(cc as usize);
        let mut clusters = vec![0usize];
        let mut unit = 0;
        for c in &cm {
            unit += c.length as usize;
            clusters.push(unit);
        }
        let mut boundaries = vec![(0usize, start)];
        unit = 0;
        for (at, ch) in s.char_indices() {
            unit += ch.len_utf16();
            boundaries.push((unit, ByteOffset(start.0 + at + ch.len_utf8())));
        }
        let mut rows = Vec::new();
        let mut stops = Vec::new();
        let mut first_unit = 0;
        let mut y = 0.0;
        for (ri, m) in lm.iter().enumerate() {
            let last_unit = first_unit + m.length as usize;
            let first = boundaries.iter().find(|b| b.0 == first_unit)?.1;
            let last = boundaries.iter().find(|b| b.0 == last_unit)?.1;
            let mut caret_stops = Vec::new();
            for &(u, byte) in &boundaries {
                if u < first_unit || u > last_unit || !clusters.contains(&u) {
                    continue;
                }
                let mut x = 0.0;
                let mut py = 0.0;
                let mut hit = DWRITE_HIT_TEST_METRICS::default();
                let trailing = u == utf16.len() && u > 0;
                let at = if trailing { u - 1 } else { u };
                if unsafe { l.HitTestTextPosition(at as u32, trailing, &mut x, &mut py, &mut hit) }
                    .is_ok()
                {
                    caret_stops.push(byte);
                    stops.push(Stop { byte, x, row: ri });
                }
            }
            if !caret_stops.contains(&first) {
                caret_stops.insert(0, first);
            }
            if !caret_stops.contains(&last) {
                caret_stops.push(last);
            }
            rows.push(LayoutRow {
                range: first..last,
                y: Dip(y),
                height: Dip(m.height),
                glyph_runs: Vec::new(),
                caret_stops,
            });
            first_unit = last_unit;
            y += m.height;
        }
        if rows.is_empty() {
            return self.fast(b"", start, line);
        }
        let collected = Arc::new(Mutex::new(Vec::new()));
        let renderer: IDWriteTextRenderer = RunCollector {
            runs: collected.clone(),
        }
        .into();
        unsafe {
            l.Draw(None, &renderer, 0.0, 0.0).ok()?;
        }
        for (at, len, font_id, ids, advances, offsets, baseline_y) in collected.lock().ok()?.iter()
        {
            if let Some(row) = rows
                .iter_mut()
                .find(|r| *baseline_y >= r.y.0 && *baseline_y <= r.y.0 + r.height.0)
            {
                let first = boundaries
                    .iter()
                    .rev()
                    .find(|b| b.0 <= *at as usize)
                    .map(|b| b.1)
                    .unwrap_or(start);
                let last = boundaries
                    .iter()
                    .find(|b| b.0 >= (*at + *len) as usize)
                    .map(|b| b.1)
                    .unwrap_or(row.range.end);
                row.glyph_runs.push(LayoutGlyphRun {
                    range: first..last,
                    font_id: *font_id,
                    glyph_ids: ids.clone(),
                    advances: advances.clone(),
                    offsets: offsets.clone(),
                });
            }
        }
        let result = LineLayout {
            line,
            rows,
            height: Dip(y),
        };
        self.remember(&result, stops);
        Some(result)
    }
}
fn signature(l: &LineLayout) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for r in &l.rows {
        r.range.start.hash(&mut h);
        r.range.end.hash(&mut h);
        r.caret_stops.hash(&mut h);
        r.height.0.to_bits().hash(&mut h);
        for run in &r.glyph_runs {
            run.glyph_ids.hash(&mut h);
            for advance in &run.advances {
                advance.0.to_bits().hash(&mut h);
            }
        }
    }
    h.finish()
}

/// Pick a bounded slice around a byte offset using DirectWrite cluster edges.
/// Returns `None` if one cluster is wider than the requested window.
pub fn slice_around(
    text: &dyn TextSnapshot,
    byte: ByteOffset,
    max_bytes: usize,
) -> Option<Range<ByteOffset>> {
    if max_bytes == 0 {
        return None;
    }
    let line = text.byte_to_position(byte)?.line;
    let line_range = text.line_range(line)?;
    if byte < line_range.start || byte > line_range.end {
        return None;
    }
    let context = max_bytes.max(64);
    let mut left = byte.0.saturating_sub(context).max(line_range.start.0);
    let mut right = byte.0.saturating_add(context).min(line_range.end.0);
    while left < byte.0 && text.byte_to_utf16(ByteOffset(left)).is_none() {
        left += 1;
    }
    while right > byte.0 && text.byte_to_utf16(ByteOffset(right)).is_none() {
        right -= 1;
    }
    let mut s = String::new();
    if !text.visit_chunks(ByteOffset(left)..ByteOffset(right), &mut |part| {
        s.push_str(part)
    }) {
        return None;
    }
    let trimmed = s.trim_end_matches(['\r', '\n', '\u{85}', '\u{2028}', '\u{2029}']);
    right = left + trimmed.len();
    if byte.0 > right {
        return None;
    }
    let utf16: Vec<u16> = trimmed.encode_utf16().collect();
    if utf16.is_empty() {
        return Some(ByteOffset(left)..ByteOffset(right));
    }
    let layout = unsafe {
        factory()
            .ok()?
            .CreateTextLayout(
                &utf16,
                &font(
                    "Cascadia Mono",
                    10.0 * 96.0 / 72.0,
                    DWRITE_FONT_WEIGHT_NORMAL,
                )
                .ok()?
                .format,
                1_000_000.0,
                1_000_000.0,
            )
            .ok()?
    };
    let mut count = 0;
    let mut clusters = vec![DWRITE_CLUSTER_METRICS::default(); utf16.len()];
    unsafe {
        layout
            .GetClusterMetrics(Some(&mut clusters), &mut count)
            .ok()?;
    }
    clusters.truncate(count as usize);
    let mut utf16_to_byte = HashMap::new();
    let mut unit = 0usize;
    utf16_to_byte.insert(0, left);
    for (at, ch) in trimmed.char_indices() {
        unit += ch.len_utf16();
        utf16_to_byte.insert(unit, left + at + ch.len_utf8());
    }
    let mut boundaries = Vec::new();
    unit = 0;
    if left == line_range.start.0 {
        boundaries.push(left);
    }
    for cluster in &clusters {
        unit += cluster.length as usize;
        if let Some(&b) = utf16_to_byte.get(&unit) {
            boundaries.push(b);
        }
    }
    if right < line_range.end.0 {
        boundaries.pop();
    }
    let desired_left = byte.0.saturating_sub(max_bytes / 2);
    let start = *boundaries
        .iter()
        .filter(|&&b| b <= byte.0 && b >= desired_left)
        .min()?;
    let end = *boundaries
        .iter()
        .filter(|&&b| b >= byte.0 && b <= start.saturating_add(max_bytes))
        .max()?;
    Some(ByteOffset(start)..ByteOffset(end))
}
impl Layout for DirectWriteLayout {
    fn layout_line(
        &self,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Option<LineLayout> {
        let range = text.line_range(line)?;
        let mut s = String::new();
        if !text.visit_chunks(range.clone(), &mut |c| s.push_str(c)) {
            return None;
        }
        self.content(
            s.trim_end_matches(['\r', '\n', '\u{85}', '\u{2028}', '\u{2029}']),
            range.start,
            line,
            width,
        )
    }
    fn layout_slice(
        &self,
        text: &dyn TextSnapshot,
        range: Range<ByteOffset>,
        width: Dip,
    ) -> Option<LineLayout> {
        if range.start > range.end || range.end > text.len_bytes() {
            return None;
        }
        let line = text.byte_to_position(range.start)?.line;
        let mut s = String::new();
        if !text.visit_chunks(range.clone(), &mut |c| s.push_str(c))
            || s.contains(['\r', '\n', '\u{85}', '\u{2028}', '\u{2029}'])
        {
            return None;
        }
        self.content(&s, range.start, line, width)
    }
    fn hit_test_point(&self, l: &LineLayout, p: Point) -> ByteOffset {
        let ri = l
            .rows
            .iter()
            .position(|r| p.y.0 < r.y.0 + r.height.0)
            .unwrap_or(l.rows.len().saturating_sub(1));
        let Ok(h) = self.hits.lock() else {
            return l.rows[ri].range.start;
        };
        h.map
            .get(&signature(l))
            .and_then(|v| {
                v.iter()
                    .filter(|s| s.row == ri)
                    .min_by(|a, b| (a.x - p.x.0).abs().total_cmp(&(b.x - p.x.0).abs()))
                    .map(|s| s.byte)
            })
            .unwrap_or(l.rows[ri].range.start)
    }
    fn caret_rect(&self, l: &LineLayout, byte: ByteOffset) -> Option<Rect> {
        let h = self.hits.lock().ok()?;
        let s = h.map.get(&signature(l))?.iter().find(|s| s.byte == byte)?;
        let r = l.rows.get(s.row)?;
        Some(Rect {
            x: Dip(s.x),
            y: r.y,
            width: Dip(1.0),
            height: r.height,
        })
    }
}
impl TextMeasurer for DirectWriteLayout {
    fn measure(&self, text: &str, _font: &str) -> TextMetrics {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let width = factory()
            .ok()
            .and_then(|f| unsafe {
                f.CreateTextLayout(&utf16, &self.font.format, 1_000_000.0, 1_000_000.0)
                    .ok()
            })
            .and_then(|l| {
                let mut m = DWRITE_TEXT_METRICS::default();
                unsafe { l.GetMetrics(&mut m).ok()? };
                Some(m.widthIncludingTrailingWhitespace)
            })
            .unwrap_or(0.0);
        TextMetrics {
            width: Dip(width),
            height: Dip(self.font.height),
            baseline: Dip(self.font.baseline),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    line: usize,
    width: u32,
    font: usize,
    text: u64,
}
/// Bounded cache of displayed lines. A key only hashes one line.
pub struct LineCache {
    cap: usize,
    used: usize,
    entries: HashMap<CacheKey, (LineLayout, usize)>,
    order: VecDeque<CacheKey>,
}
impl LineCache {
    pub fn new(memory_cap: usize) -> Self {
        Self {
            cap: memory_cap,
            used: 0,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }
    pub fn memory_bytes(&self) -> usize {
        self.used
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.used = 0;
    }
    pub fn get_or_layout(
        &mut self,
        engine: &DirectWriteLayout,
        text: &dyn TextSnapshot,
        line: LineIndex,
        width: Dip,
    ) -> Option<LineLayout> {
        let range = text.line_range(line)?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        if !text.visit_chunks(range.clone(), &mut |part| part.hash(&mut h)) {
            return None;
        }
        let key = CacheKey {
            line: line.0,
            width: width.0.to_bits(),
            font: Arc::as_ptr(&engine.font) as usize,
            text: h.finish(),
        };
        if let Some((layout, _)) = self.entries.get_mut(&key) {
            let old = layout.clone();
            if let Some(first) = layout.rows.first()
                && first.range.start != range.start
            {
                let delta = range.start.0 as isize - first.range.start.0 as isize;
                for row in &mut layout.rows {
                    row.range.start.0 = row.range.start.0.checked_add_signed(delta)?;
                    row.range.end.0 = row.range.end.0.checked_add_signed(delta)?;
                    for byte in &mut row.caret_stops {
                        byte.0 = byte.0.checked_add_signed(delta)?;
                    }
                    for run in &mut row.glyph_runs {
                        run.range.start.0 = run.range.start.0.checked_add_signed(delta)?;
                        run.range.end.0 = run.range.end.0.checked_add_signed(delta)?;
                    }
                }
                engine.rebase_hits(&old, layout, delta);
            }
            let result = layout.clone();
            self.touch(key);
            return Some(result);
        }
        let result = engine.layout_line(text, line, width)?;
        let size: usize = result
            .rows
            .iter()
            .map(|r| {
                128 + r.caret_stops.len() * 8
                    + r.glyph_runs
                        .iter()
                        .map(|g| g.glyph_ids.len() * 18)
                        .sum::<usize>()
            })
            .sum();
        if size <= self.cap {
            self.used += size;
            self.entries.insert(key, (result.clone(), size));
            self.touch(key);
            self.trim();
        }
        Some(result)
    }
    fn touch(&mut self, key: CacheKey) {
        self.order.retain(|k| *k != key);
        self.order.push_back(key);
    }
    fn trim(&mut self) {
        while self.used > self.cap {
            if let Some(k) = self.order.pop_front() {
                if let Some((_, n)) = self.entries.remove(&k) {
                    self.used -= n;
                }
            } else {
                break;
            }
        }
    }
    /// The old snapshot identifies the touched lines; later keys move by the line delta.
    pub fn invalidate(&mut self, old: &dyn TextSnapshot, change: &TextChange) {
        let Some(first) = old
            .byte_to_position(change.replaced.start)
            .map(|p| p.line.0)
        else {
            self.clear();
            return;
        };
        let Some(last) = old.byte_to_position(change.replaced.end).map(|p| p.line.0) else {
            self.clear();
            return;
        };
        let mut moved = HashMap::new();
        let mut order = VecDeque::new();
        let mut used = 0;
        for key in &self.order {
            if key.line >= first && key.line <= last {
                continue;
            }
            let Some((mut layout, n)) = self.entries.remove(key) else {
                continue;
            };
            let mut k = *key;
            if k.line > last {
                let next = k.line as isize + change.line_count_delta;
                if next < 0 {
                    continue;
                }
                k.line = next as usize;
                layout.line = LineIndex(k.line);
            }
            used += n;
            order.push_back(k);
            moved.insert(k, (layout, n));
        }
        self.entries = moved;
        self.order = order;
        self.used = used;
    }
}

/// Measured line heights are stored as changes from one estimated height.
pub struct HeightMap {
    estimate: f32,
    heights: Vec<Option<f32>>,
    tree: Vec<f64>,
}
impl HeightMap {
    pub fn new(lines: usize, estimate: Dip) -> Self {
        Self {
            estimate: estimate.0,
            heights: vec![None; lines],
            tree: vec![0.0; lines + 1],
        }
    }
    pub fn len(&self) -> usize {
        self.heights.len()
    }
    pub fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }
    pub fn set(&mut self, line: LineIndex, height: Dip) -> bool {
        let Some(slot) = self.heights.get_mut(line.0) else {
            return false;
        };
        let previous = slot.unwrap_or(self.estimate);
        *slot = Some(height.0);
        let mut i = line.0 + 1;
        while i < self.tree.len() {
            self.tree[i] += (height.0 - previous) as f64;
            i += i & (!i + 1);
        }
        true
    }
    fn extra(&self, mut count: usize) -> f64 {
        let mut sum = 0.0;
        while count > 0 {
            sum += self.tree[count];
            count &= count - 1;
        }
        sum
    }
    pub fn line_to_scroll(&self, line: LineIndex) -> Dip {
        let n = line.0.min(self.len());
        Dip((n as f64 * self.estimate as f64 + self.extra(n)) as f32)
    }
    pub fn scroll_to_line(&self, scroll: Dip) -> LineIndex {
        let mut at = 0;
        let mut delta = 0.0;
        let mut bit = self.len().next_power_of_two();
        while bit != 0 {
            let next = at + bit;
            if next <= self.len() {
                let candidate = next as f64 * self.estimate as f64 + delta + self.tree[next];
                if candidate <= scroll.0 as f64 {
                    at = next;
                    delta += self.tree[next];
                }
            }
            bit >>= 1;
        }
        LineIndex(at.min(self.len().saturating_sub(1)))
    }
}
