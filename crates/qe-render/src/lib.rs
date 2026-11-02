//! DirectWrite draws glyphs into a GDI bitmap. Shapes use the same bitmap.
use qe_api::{Bitmap, Color, Dip, DisplayList, DrawCommand, Point, Rect, RenderError, Renderer};
use std::{collections::HashMap, mem, ptr, sync::Arc};
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND},
        Graphics::{DirectWrite::*, Gdi::*},
    },
    core::{BOOL, Interface, PCWSTR},
};

fn error(e: impl std::fmt::Display) -> RenderError {
    RenderError::new(e.to_string())
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
#[derive(Clone, Copy, Debug)]
struct Area {
    x: i32,
    y: i32,
    r: i32,
    b: i32,
}
impl Area {
    fn full(w: u32, h: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            r: w as i32,
            b: h as i32,
        }
    }
    fn new(v: Rect, scale: f32) -> Self {
        Self {
            x: (v.x.0 * scale).floor() as i32,
            y: (v.y.0 * scale).floor() as i32,
            r: ((v.x.0 + v.width.0) * scale).ceil() as i32,
            b: ((v.y.0 + v.height.0) * scale).ceil() as i32,
        }
    }
    fn clip(self, other: Self) -> Self {
        Self {
            x: self.x.max(other.x),
            y: self.y.max(other.y),
            r: self.r.min(other.r),
            b: self.b.min(other.b),
        }
    }
    fn valid(self) -> bool {
        self.x < self.r && self.y < self.b
    }
}

/// M9 can register its layout font IDs through this small interface.
pub trait FontRegistry {
    fn font(&self, id: u32) -> Option<(&str, f32)>;
}

struct GlyphDraw<'a> {
    at: Point,
    face: IDWriteFontFace,
    em: f32,
    ids: &'a [u16],
    advances: &'a [f32],
    offsets: &'a [DWRITE_GLYPH_OFFSET],
    bidi: u8,
    color: Color,
    clip: Area,
    color_glyphs: bool,
}
#[derive(Clone, Hash, PartialEq, Eq)]
struct GlyphKey {
    family: String,
    glyph: u16,
    dpi: u32,
    color: Color,
}
struct GlyphImage {
    left: i32,
    top: i32,
    width: usize,
    height: usize,
    coverage: Vec<u8>,
    on_white: Vec<u8>,
}

pub struct BitmapRenderer {
    factory: IDWriteFactory,
    target: IDWriteBitmapRenderTarget,
    params: IDWriteRenderingParams,
    width: u32,
    height: u32,
    capacity_width: u32,
    capacity_height: u32,
    dpi: u32,
    pending: Vec<Area>,
    damage: Vec<Area>,
    scrolled: Vec<Area>,
    fonts: HashMap<String, IDWriteFontFace>,
    ids: HashMap<u32, (String, f32)>,
    shaped: HashMap<(String, String), (Vec<u16>, Vec<f32>)>,
    glyph_cache: HashMap<GlyphKey, Arc<GlyphImage>>,
    white_background: bool,
}
impl BitmapRenderer {
    pub fn new(width: u32, height: u32) -> Result<Self, RenderError> {
        if width == 0 || height == 0 {
            return Err(error("empty bitmap"));
        }
        let factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).map_err(error)? };
        let interop = unsafe { factory.GetGdiInterop().map_err(error)? };
        let capacity_width = width.next_multiple_of(256) + 64;
        let capacity_height = height.next_multiple_of(128);
        let target = unsafe {
            interop
                .CreateBitmapRenderTarget(None, capacity_width, capacity_height)
                .map_err(error)?
        };
        let params = unsafe { factory.CreateRenderingParams().map_err(error)? };
        let result = Self {
            factory,
            target,
            params,
            width,
            height,
            capacity_width,
            capacity_height,
            dpi: 96,
            pending: Vec::new(),
            damage: vec![Area::full(width, height)],
            scrolled: Vec::new(),
            fonts: HashMap::new(),
            ids: HashMap::new(),
            shaped: HashMap::new(),
            glyph_cache: HashMap::new(),
            white_background: false,
        };
        result.bits()?;
        Ok(result)
    }
    fn bits(&self) -> Result<(*mut u8, isize), RenderError> {
        let dc = unsafe { self.target.GetMemoryDC() };
        let obj = unsafe { GetCurrentObject(dc, OBJ_BITMAP) };
        let mut dib: DIBSECTION = unsafe { mem::zeroed() };
        let n = unsafe {
            GetObjectW(
                obj,
                mem::size_of::<DIBSECTION>() as i32,
                Some((&mut dib as *mut DIBSECTION).cast()),
            )
        };
        if n < mem::size_of::<DIBSECTION>() as i32
            || dib.dsBm.bmBits.is_null()
            || dib.dsBm.bmBitsPixel != 32
        {
            return Err(error("DirectWrite did not give a 32-bit DIB"));
        }
        Ok((dib.dsBm.bmBits.cast(), dib.dsBm.bmWidthBytes as isize))
    }
    pub fn bitmap(&self) -> Result<Bitmap, RenderError> {
        if !unsafe { GdiFlush() }.as_bool() {
            return Err(error("GdiFlush failed"));
        }
        let (bits, stride) = self.bits()?;
        let mut bgra = vec![0; self.width as usize * self.height as usize * 4];
        for y in 0..self.height as usize {
            unsafe {
                ptr::copy_nonoverlapping(
                    bits.offset(y as isize * stride),
                    bgra.as_mut_ptr().add(y * self.width as usize * 4),
                    self.width as usize * 4,
                );
            }
        }
        Ok(Bitmap {
            width: self.width,
            height: self.height,
            bgra,
        })
    }
    pub fn invalidate(&mut self, rect: Rect) {
        let a = Area::new(rect, self.scale()).clip(Area::full(self.width, self.height));
        if a.valid() {
            self.pending.push(a);
        }
    }
    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }
    pub fn back_buffer_bytes(&self) -> usize {
        self.capacity_width as usize * self.capacity_height as usize * 4
    }
    pub fn present(&self, hwnd: HWND) -> Result<(), RenderError> {
        let dc = unsafe { GetDC(Some(hwnd)) };
        if dc.0.is_null() {
            return Err(error("GetDC failed"));
        }
        let src = unsafe { self.target.GetMemoryDC() };
        let full = Area::full(self.width, self.height);
        let result = self
            .damage
            .iter()
            .copied()
            .chain(if self.damage.is_empty() {
                Some(full)
            } else {
                None
            })
            .try_for_each(|a| unsafe {
                BitBlt(
                    dc,
                    a.x,
                    a.y,
                    a.r - a.x,
                    a.b - a.y,
                    Some(src),
                    a.x,
                    a.y,
                    SRCCOPY,
                )
                .map_err(error)
            });
        let flushed = unsafe { GdiFlush() }.as_bool();
        unsafe {
            ReleaseDC(Some(hwnd), dc);
        }
        result?;
        if !flushed {
            return Err(error("GdiFlush failed"));
        }
        Ok(())
    }
    pub fn scroll(&mut self, rect: Rect, dy: Dip) -> Result<(), RenderError> {
        let a = Area::new(rect, self.scale()).clip(Area::full(self.width, self.height));
        let shift = (dy.0 * self.scale()).round() as i32;
        if !a.valid() || shift == 0 {
            return Ok(());
        }
        let amount = shift.unsigned_abs().min((a.b - a.y) as u32) as i32;
        if !unsafe { GdiFlush() }.as_bool() {
            return Err(error("GdiFlush failed before scroll"));
        }
        let (bits, stride) = self.bits()?;
        if amount < a.b - a.y {
            let rows = a.b - a.y - amount;
            for i in 0..rows {
                let row = if shift > 0 { rows - 1 - i } else { i };
                let src = if shift > 0 {
                    a.y + row
                } else {
                    a.y + amount + row
                };
                let dst = if shift > 0 {
                    a.y + amount + row
                } else {
                    a.y + row
                };
                unsafe {
                    ptr::copy(
                        bits.offset(src as isize * stride + a.x as isize * 4),
                        bits.offset(dst as isize * stride + a.x as isize * 4),
                        (a.r - a.x) as usize * 4,
                    );
                }
            }
        }
        self.pending.push(if shift > 0 {
            Area {
                b: a.y + amount,
                ..a
            }
        } else {
            Area {
                y: a.b - amount,
                ..a
            }
        });
        self.scrolled.push(a);
        Ok(())
    }
    fn font(&mut self, name: &str) -> Result<IDWriteFontFace, RenderError> {
        if let Some(face) = self.fonts.get(name) {
            return Ok(face.clone());
        }
        let mut collection = None;
        unsafe {
            self.factory
                .GetSystemFontCollection(&mut collection, false)
                .map_err(error)?;
        }
        let collection = collection.ok_or_else(|| error("font collection missing"))?;
        let mut index = 0;
        let mut exists = BOOL(0);
        let name_w = wide(name);
        unsafe {
            collection
                .FindFamilyName(PCWSTR(name_w.as_ptr()), &mut index, &mut exists)
                .map_err(error)?;
        }
        if !exists.as_bool() {
            return Err(error(format!("font not installed: {name}")));
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
        self.fonts.insert(name.to_owned(), face.clone());
        Ok(face)
    }
    pub fn register_font(
        &mut self,
        id: u32,
        family: &str,
        size_pt: f32,
    ) -> Result<(), RenderError> {
        self.font(family)?;
        self.ids.insert(id, (family.to_owned(), size_pt));
        Ok(())
    }
    pub fn register_fonts(
        &mut self,
        registry: &dyn FontRegistry,
        ids: &[u32],
    ) -> Result<(), RenderError> {
        for &id in ids {
            let (family, size) = registry
                .font(id)
                .ok_or_else(|| error(format!("font ID {id} missing")))?;
            self.register_font(id, family, size)?;
        }
        Ok(())
    }
    fn blend(
        &self,
        x: i32,
        y: i32,
        c: Color,
        coverage: f32,
        clip: Area,
    ) -> Result<(), RenderError> {
        if x < clip.x || x >= clip.r || y < clip.y || y >= clip.b {
            return Ok(());
        }
        let (bits, stride) = self.bits()?;
        let p = unsafe { bits.offset(y as isize * stride + x as isize * 4) };
        let a = (c.a as f32 * coverage).round().clamp(0.0, 255.0) as u32;
        unsafe {
            for (i, v) in [c.b, c.g, c.r].iter().enumerate() {
                *p.add(i) =
                    ((u32::from(*v) * a + u32::from(*p.add(i)) * (255 - a) + 127) / 255) as u8;
            }
            *p.add(3) = 255;
        }
        Ok(())
    }
    fn fill(&self, r: Rect, c: Color, clip: Area) -> Result<(), RenderError> {
        let a = Area::new(r, self.scale()).clip(clip);
        if !a.valid() {
            return Ok(());
        }
        let (bits, stride) = self.bits()?;
        let alpha = u32::from(c.a);
        if alpha == 255 && a.r - a.x >= 32 {
            let packed = u32::from_le_bytes([c.b, c.g, c.r, 255]);
            let row = vec![packed; (a.r - a.x) as usize];
            for y in a.y..a.b {
                unsafe {
                    ptr::copy_nonoverlapping(
                        row.as_ptr().cast::<u8>(),
                        bits.offset(y as isize * stride + a.x as isize * 4),
                        row.len() * 4,
                    );
                }
            }
            return Ok(());
        }
        for y in a.y..a.b {
            for x in a.x..a.r {
                let p = unsafe { bits.offset(y as isize * stride + x as isize * 4) };
                unsafe {
                    if alpha == 255 {
                        *p = c.b;
                        *p.add(1) = c.g;
                        *p.add(2) = c.r;
                        *p.add(3) = 255;
                    } else {
                        for (i, v) in [c.b, c.g, c.r].iter().enumerate() {
                            *p.add(i) = ((u32::from(*v) * alpha
                                + u32::from(*p.add(i)) * (255 - alpha)
                                + 127)
                                / 255) as u8;
                        }
                        *p.add(3) = 255;
                    }
                }
            }
        }
        Ok(())
    }
    fn round(
        &self,
        r: Rect,
        radius: f32,
        stroke: f32,
        c: Color,
        clip: Area,
    ) -> Result<(), RenderError> {
        let s = self.scale();
        let a = Area::new(r, s).clip(clip);
        let x = r.x.0 * s;
        let y = r.y.0 * s;
        let right = (r.x.0 + r.width.0) * s;
        let bottom = (r.y.0 + r.height.0) * s;
        let rad = (radius * s).max(0.0).min((right - x).min(bottom - y) / 2.0);
        let w = stroke * s;
        for py in a.y..a.b {
            for px in a.x..a.r {
                let mut hits = 0;
                for oy in [0.25, 0.75] {
                    for ox in [0.25, 0.75] {
                        let fx = px as f32 + ox;
                        let fy = py as f32 + oy;
                        if inside(fx, fy, x, y, right, bottom, rad)
                            && (stroke == 0.0
                                || !inside(
                                    fx,
                                    fy,
                                    x + w,
                                    y + w,
                                    right - w,
                                    bottom - w,
                                    (rad - w).max(0.0),
                                ))
                        {
                            hits += 1;
                        }
                    }
                }
                if hits > 0 {
                    self.blend(px, py, c, hits as f32 / 4.0, clip)?;
                }
            }
        }
        Ok(())
    }
    fn line(
        &self,
        from: Point,
        to: Point,
        width: f32,
        c: Color,
        clip: Area,
    ) -> Result<(), RenderError> {
        let s = self.scale();
        let (x0, y0, x1, y1) = (from.x.0 * s, from.y.0 * s, to.x.0 * s, to.y.0 * s);
        let half = (width * s).max(1.0) / 2.0;
        let a = Area {
            x: (x0.min(x1) - half - 1.0).floor() as i32,
            y: (y0.min(y1) - half - 1.0).floor() as i32,
            r: (x0.max(x1) + half + 1.0).ceil() as i32,
            b: (y0.max(y1) + half + 1.0).ceil() as i32,
        }
        .clip(clip);
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = dx * dx + dy * dy;
        for py in a.y..a.b {
            for px in a.x..a.r {
                let mut hits = 0;
                for oy in [0.25, 0.75] {
                    for ox in [0.25, 0.75] {
                        let x = px as f32 + ox;
                        let y = py as f32 + oy;
                        let t = if len > 0.0 {
                            ((x - x0) * dx + (y - y0) * dy) / len
                        } else {
                            0.0
                        }
                        .clamp(0.0, 1.0);
                        let ex = x - x0 - t * dx;
                        let ey = y - y0 - t * dy;
                        if ex * ex + ey * ey <= half * half {
                            hits += 1;
                        }
                    }
                }
                if hits > 0 {
                    self.blend(px, py, c, hits as f32 / 4.0, clip)?;
                }
            }
        }
        Ok(())
    }
    fn glyphs(&self, args: GlyphDraw<'_>) -> Result<(), RenderError> {
        let GlyphDraw {
            at,
            face,
            em,
            ids,
            advances,
            offsets,
            bidi,
            color,
            clip,
            color_glyphs,
        } = args;
        if ids.is_empty() {
            return Ok(());
        }
        let dc = unsafe { self.target.GetMemoryDC() };
        let saved = unsafe { SaveDC(dc) };
        if saved == 0 {
            return Err(error("SaveDC failed"));
        }
        let result = (|| {
            if unsafe { IntersectClipRect(dc, clip.x, clip.y, clip.r, clip.b) }.0 == ERROR {
                return Err(error("IntersectClipRect failed"));
            }
            let mut run = DWRITE_GLYPH_RUN {
                fontFace: mem::ManuallyDrop::new(Some(face)),
                fontEmSize: em,
                glyphCount: ids.len() as u32,
                glyphIndices: ids.as_ptr(),
                glyphAdvances: advances.as_ptr(),
                glyphOffsets: offsets.as_ptr(),
                isSideways: BOOL(0),
                bidiLevel: u32::from(bidi),
            };
            let ink = COLORREF(
                u32::from(color.r) | (u32::from(color.g) << 8) | (u32::from(color.b) << 16),
            );
            let drawn = (|| {
                if color_glyphs {
                    let f2: IDWriteFactory2 = self.factory.cast().map_err(error)?;
                    let layers = unsafe {
                        f2.TranslateColorGlyphRun(
                            at.x.0,
                            at.y.0,
                            &run,
                            None,
                            DWRITE_MEASURING_MODE_NATURAL,
                            None,
                            0,
                        )
                        .map_err(error)?
                    };
                    while unsafe { layers.MoveNext().map_err(error)? }.as_bool() {
                        let layer = unsafe { &*layers.GetCurrentRun().map_err(error)? };
                        let rgb = if layer.paletteIndex == u16::MAX {
                            ink
                        } else {
                            COLORREF(
                                ((layer.runColor.r * 255.0) as u32)
                                    | (((layer.runColor.g * 255.0) as u32) << 8)
                                    | (((layer.runColor.b * 255.0) as u32) << 16),
                            )
                        };
                        unsafe {
                            self.target
                                .DrawGlyphRun(
                                    layer.baselineOriginX,
                                    layer.baselineOriginY,
                                    DWRITE_MEASURING_MODE_NATURAL,
                                    &layer.glyphRun,
                                    &self.params,
                                    rgb,
                                    None,
                                )
                                .map_err(error)?;
                        }
                    }
                    Ok(())
                } else {
                    unsafe {
                        self.target
                            .DrawGlyphRun(
                                at.x.0,
                                at.y.0,
                                DWRITE_MEASURING_MODE_NATURAL,
                                &run,
                                &self.params,
                                ink,
                                None,
                            )
                            .map_err(error)
                    }
                }
            })();
            unsafe { mem::ManuallyDrop::drop(&mut run.fontFace) };
            drawn
        })();
        let restored = unsafe { RestoreDC(dc, saved) };
        if !restored.as_bool() {
            return Err(error("RestoreDC failed"));
        }
        result
    }
    fn cached_glyph(
        &mut self,
        family: &str,
        face: &IDWriteFontFace,
        glyph: u16,
        color: Color,
    ) -> Result<Arc<GlyphImage>, RenderError> {
        let key = GlyphKey {
            family: family.to_owned(),
            glyph,
            dpi: self.dpi,
            color,
        };
        if let Some(image) = self.glyph_cache.get(&key) {
            return Ok(image.clone());
        }
        let dc = unsafe { self.target.GetMemoryDC() };
        let x = self.width as i32;
        if !unsafe { PatBlt(dc, x, 0, 64, 64, WHITENESS) }.as_bool() {
            return Err(error("glyph cache clear failed"));
        }
        let advance = 0.0f32;
        let offset = DWRITE_GLYPH_OFFSET::default();
        let run = DWRITE_GLYPH_RUN {
            fontFace: mem::ManuallyDrop::new(Some(face.clone())),
            fontEmSize: 10.0 * 96.0 / 72.0,
            glyphCount: 1,
            glyphIndices: &glyph,
            glyphAdvances: &advance,
            glyphOffsets: &offset,
            isSideways: BOOL(0),
            bidiLevel: 0,
        };
        let s = self.scale();
        let draw = unsafe {
            self.target
                .DrawGlyphRun(
                    (x as f32 + 16.0) / s,
                    48.0 / s,
                    DWRITE_MEASURING_MODE_NATURAL,
                    &run,
                    &self.params,
                    COLORREF(0),
                    None,
                )
                .map_err(error)
        };
        let mut run = run;
        unsafe { mem::ManuallyDrop::drop(&mut run.fontFace) };
        draw?;
        if !unsafe { GdiFlush() }.as_bool() {
            return Err(error("glyph cache flush failed"));
        }
        let (bits, stride) = self.bits()?;
        let mut coverage = vec![0u8; 64 * 64 * 3];
        let mut min_x = 64;
        let mut min_y = 64;
        let mut max_x = 0;
        let mut max_y = 0;
        for py in 0..64 {
            for px in 0..64 {
                let p = unsafe { bits.offset(py as isize * stride + (x + px) as isize * 4) };
                let i = (py * 64 + px) as usize * 3;
                for channel in 0..3 {
                    coverage[i + channel] = 255 - unsafe { *p.add(channel) };
                }
                if coverage[i..i + 3].iter().any(|v| *v > 0) {
                    min_x = min_x.min(px);
                    min_y = min_y.min(py);
                    max_x = max_x.max(px + 1);
                    max_y = max_y.max(py + 1);
                }
            }
        }
        let (width, height) = (
            (max_x - min_x).max(0) as usize,
            (max_y - min_y).max(0) as usize,
        );
        let mut cropped = Vec::with_capacity(width * height * 3);
        for py in min_y..max_y {
            let start = (py * 64 + min_x) as usize * 3;
            cropped.extend_from_slice(&coverage[start..start + width * 3]);
        }
        let mut on_white = Vec::with_capacity(width * height * 4);
        for rgb in cropped.as_chunks::<3>().0 {
            on_white.extend_from_slice(&[255 - rgb[0], 255 - rgb[1], 255 - rgb[2], 255]);
        }
        let image = Arc::new(GlyphImage {
            left: min_x - 16,
            top: min_y - 48,
            width,
            height,
            coverage: cropped,
            on_white,
        });
        if self.glyph_cache.len() >= 4096 {
            self.glyph_cache.clear();
        }
        self.glyph_cache.insert(key, image.clone());
        Ok(image)
    }
    fn draw_cached_text(
        &mut self,
        at: Point,
        font: (&str, &IDWriteFontFace),
        ids: &[u16],
        advances: &[f32],
        color: Color,
        clip: Area,
    ) -> Result<(), RenderError> {
        let (family, face) = font;
        let (bits, stride) = self.bits()?;
        let s = self.scale();
        let mut pen = at.x.0 * s;
        let base = (at.y.0 * s).round() as i32;
        for (&glyph, &advance) in ids.iter().zip(advances) {
            let image = self.cached_glyph(family, face, glyph, color)?;
            let left = pen.round() as i32 + image.left;
            let top = base + image.top;
            let fast = self.white_background
                && color == Color::rgb(0, 0, 0)
                && image.left >= 0
                && image.left + image.width as i32 <= (advance * s).round() as i32;
            if fast {
                for y in 0..image.height {
                    let dst_y = top + y as i32;
                    if dst_y < clip.y || dst_y >= clip.b {
                        continue;
                    }
                    let start = left.max(clip.x);
                    let end = (left + image.width as i32).min(clip.r);
                    if start >= end {
                        continue;
                    }
                    let source = (y * image.width + (start - left) as usize) * 4;
                    unsafe {
                        ptr::copy_nonoverlapping(
                            image.on_white.as_ptr().add(source),
                            bits.offset(dst_y as isize * stride + start as isize * 4),
                            (end - start) as usize * 4,
                        );
                    }
                }
                pen += advance * s;
                continue;
            }
            for y in 0..image.height {
                let dst_y = top + y as i32;
                if dst_y < clip.y || dst_y >= clip.b {
                    continue;
                }
                for x in 0..image.width {
                    let dst_x = left + x as i32;
                    if dst_x < clip.x || dst_x >= clip.r {
                        continue;
                    }
                    let src = (y * image.width + x) * 3;
                    let p = unsafe { bits.offset(dst_y as isize * stride + dst_x as isize * 4) };
                    for channel in 0..3 {
                        let alpha = (u32::from(image.coverage[src + channel]) * u32::from(color.a)
                            + 127)
                            / 255;
                        let ink = [color.b, color.g, color.r][channel];
                        unsafe {
                            *p.add(channel) = ((u32::from(ink) * alpha
                                + u32::from(*p.add(channel)) * (255 - alpha)
                                + 127)
                                / 255) as u8;
                            *p.add(3) = 255;
                        }
                    }
                }
            }
            pen += advance * s;
        }
        Ok(())
    }
    fn command(&mut self, cmd: &DrawCommand, clip: Area, opacity: f32) -> Result<(), RenderError> {
        let fade = |mut c: Color| {
            c.a = (c.a as f32 * opacity).round().clamp(0.0, 255.0) as u8;
            c
        };
        match cmd {
            DrawCommand::FillRect { rect, color } => self.fill(*rect, fade(*color), clip),
            DrawCommand::FillRects { rects, color } => {
                for r in rects {
                    self.fill(*r, fade(*color), clip)?;
                }
                Ok(())
            }
            DrawCommand::RoundedRect {
                rect,
                radius,
                color,
            } => self.round(*rect, radius.0, 0.0, fade(*color), clip),
            DrawCommand::StrokeRect {
                rect,
                radius,
                width,
                color,
            } => self.round(*rect, radius.0, width.0, fade(*color), clip),
            DrawCommand::Line {
                from,
                to,
                width,
                color,
            } => self.line(*from, *to, width.0, fade(*color), clip),
            DrawCommand::WavyUnderline { from, to, color } => {
                let n = ((to.x.0 - from.x.0).abs() / 2.0).ceil() as usize;
                for i in 0..n {
                    let point = |j: usize| {
                        let t = j as f32 / n.max(1) as f32;
                        Point {
                            x: Dip(from.x.0 + (to.x.0 - from.x.0) * t),
                            y: Dip(from.y.0
                                + (to.y.0 - from.y.0) * t
                                + (t * (to.x.0 - from.x.0) * 1.5).sin() * 1.25),
                        }
                    };
                    self.line(point(i), point(i + 1), 1.0, fade(*color), clip)?;
                }
                Ok(())
            }
            DrawCommand::GlyphRun {
                at,
                bidi_level,
                font_id,
                glyph_ids,
                advances,
                offsets,
                color,
            } => {
                let (name, size) = self
                    .ids
                    .get(font_id)
                    .ok_or_else(|| error(format!("font ID {font_id} missing")))?
                    .clone();
                let face = self.font(&name)?;
                if advances.len() != glyph_ids.len() || offsets.len() != glyph_ids.len() {
                    return Err(error("glyph run lengths differ"));
                }
                let adv: Vec<f32> = advances.iter().map(|d| d.0).collect();
                let off: Vec<DWRITE_GLYPH_OFFSET> = offsets
                    .iter()
                    .map(|p| DWRITE_GLYPH_OFFSET {
                        advanceOffset: p.x.0,
                        ascenderOffset: -p.y.0,
                    })
                    .collect();
                self.glyphs(GlyphDraw {
                    at: *at,
                    face,
                    em: size * 96.0 / 72.0,
                    ids: glyph_ids,
                    advances: &adv,
                    offsets: &off,
                    bidi: *bidi_level,
                    color: fade(*color),
                    clip,
                    color_glyphs: name == "Segoe UI Emoji",
                })
            }
            DrawCommand::TextRun {
                at,
                text,
                font,
                color,
            } => {
                let face = self.font(font)?;
                let key = (font.clone(), text.clone());
                if !self.shaped.contains_key(&key) {
                    let cps: Vec<u32> = text.chars().map(|c| c as u32).collect();
                    let mut ids = vec![0; cps.len()];
                    if !ids.is_empty() {
                        unsafe {
                            face.GetGlyphIndices(cps.as_ptr(), cps.len() as u32, ids.as_mut_ptr())
                                .map_err(error)?;
                        }
                    }
                    let mut fm = DWRITE_FONT_METRICS::default();
                    unsafe { face.GetMetrics(&mut fm) };
                    let mut gm = vec![DWRITE_GLYPH_METRICS::default(); ids.len()];
                    if !ids.is_empty() {
                        unsafe {
                            face.GetDesignGlyphMetrics(
                                ids.as_ptr(),
                                ids.len() as u32,
                                gm.as_mut_ptr(),
                                false,
                            )
                            .map_err(error)?;
                        }
                    }
                    let em = 10.0 * 96.0 / 72.0;
                    let adv: Vec<f32> = gm
                        .iter()
                        .map(|g| g.advanceWidth as f32 * em / fm.designUnitsPerEm as f32)
                        .collect();
                    if self.shaped.len() >= 2048 {
                        self.shaped.clear();
                    }
                    self.shaped.insert(key.clone(), (ids, adv));
                }
                let (ids, adv) = self
                    .shaped
                    .get(&key)
                    .ok_or_else(|| error("shape cache missing"))?
                    .clone();
                let ink = fade(*color);
                if font == "Segoe UI Emoji" {
                    let em = 10.0 * 96.0 / 72.0;
                    let off = vec![DWRITE_GLYPH_OFFSET::default(); ids.len()];
                    self.glyphs(GlyphDraw {
                        at: *at,
                        face,
                        em,
                        ids: &ids,
                        advances: &adv,
                        offsets: &off,
                        bidi: 0,
                        color: ink,
                        clip,
                        color_glyphs: true,
                    })
                } else {
                    self.draw_cached_text(*at, (font, &face), &ids, &adv, ink, clip)
                }
            }
            DrawCommand::Icon { at, icon_id, color } => {
                let value = u32::from_str_radix(
                    icon_id.trim_start_matches("U+").trim_start_matches("0x"),
                    16,
                )
                .map_err(error)?;
                let ch = char::from_u32(value).ok_or_else(|| error("invalid icon"))?;
                self.command(
                    &DrawCommand::TextRun {
                        at: Point {
                            x: at.x,
                            y: Dip(at.y.0 + at.height.0 * 0.8),
                        },
                        text: ch.to_string(),
                        font: "Segoe Fluent Icons".into(),
                        color: *color,
                    },
                    clip,
                    opacity,
                )
            }
            _ => Ok(()),
        }
    }
    fn intersects(&self, cmd: &DrawCommand, clip: Area) -> bool {
        let s = self.scale();
        let bounds = match cmd {
            DrawCommand::FillRect { rect, .. }
            | DrawCommand::RoundedRect { rect, .. }
            | DrawCommand::StrokeRect { rect, .. } => Some(Area::new(*rect, s)),
            DrawCommand::TextRun { at, .. } | DrawCommand::GlyphRun { at, .. } => Some(Area {
                x: (at.x.0 * s).floor() as i32,
                y: ((at.y.0 - 15.0) * s).floor() as i32,
                r: self.width as i32,
                b: ((at.y.0 + 5.0) * s).ceil() as i32,
            }),
            DrawCommand::Icon { at, .. } => Some(Area::new(*at, s)),
            DrawCommand::Line { from, to, .. } | DrawCommand::WavyUnderline { from, to, .. } => {
                Some(Area {
                    x: (from.x.0.min(to.x.0) * s - 4.0).floor() as i32,
                    y: (from.y.0.min(to.y.0) * s - 4.0).floor() as i32,
                    r: (from.x.0.max(to.x.0) * s + 4.0).ceil() as i32,
                    b: (from.y.0.max(to.y.0) * s + 4.0).ceil() as i32,
                })
            }
            _ => None,
        };
        bounds.is_none_or(|b| b.clip(clip).valid())
    }
}
fn inside(x: f32, y: f32, left: f32, top: f32, right: f32, bottom: f32, r: f32) -> bool {
    if x < left || x >= right || y < top || y >= bottom {
        return false;
    }
    let dx = x - x.clamp(left + r, right - r);
    let dy = y - y.clamp(top + r, bottom - r);
    dx * dx + dy * dy <= r * r
}

impl Renderer for BitmapRenderer {
    fn begin_frame(&mut self) -> Result<(), RenderError> {
        self.damage = mem::take(&mut self.scrolled);
        Ok(())
    }
    fn draw(&mut self, list: &DisplayList) -> Result<(), RenderError> {
        let regions = if self.pending.is_empty() {
            vec![Area::full(self.width, self.height)]
        } else {
            mem::take(&mut self.pending)
        };
        for area in regions {
            let mut clips = vec![area];
            let mut opacity = vec![1.0f32];
            self.white_background = false;
            for cmd in &list.commands {
                match cmd {
                    DrawCommand::PushClip(r) => {
                        let c = clips
                            .last()
                            .copied()
                            .unwrap_or(area)
                            .clip(Area::new(*r, self.scale()));
                        clips.push(c);
                    }
                    DrawCommand::PopClip => {
                        if clips.len() == 1 {
                            return Err(error("clip stack underflow"));
                        }
                        clips.pop();
                    }
                    DrawCommand::PushOpacity(a) => {
                        opacity.push(opacity.last().copied().unwrap_or(1.0) * a.clamp(0.0, 1.0))
                    }
                    DrawCommand::PopOpacity => {
                        if opacity.len() == 1 {
                            return Err(error("opacity stack underflow"));
                        }
                        opacity.pop();
                    }
                    _ => {
                        let clip = *clips.last().unwrap_or(&area);
                        if self.intersects(cmd, clip) {
                            self.command(cmd, clip, *opacity.last().unwrap_or(&1.0))?;
                            match cmd {
                                DrawCommand::FillRect { rect, color }
                                    if *color == Color::rgb(255, 255, 255) && {
                                        let a = Area::new(*rect, self.scale());
                                        a.x <= area.x
                                            && a.y <= area.y
                                            && a.r >= area.r
                                            && a.b >= area.b
                                    } =>
                                {
                                    self.white_background = true
                                }
                                DrawCommand::TextRun { .. } | DrawCommand::GlyphRun { .. } => {}
                                _ => self.white_background = false,
                            }
                        }
                    }
                }
            }
            if clips.len() != 1 || opacity.len() != 1 {
                return Err(error("unbalanced display list stacks"));
            }
            self.damage.push(area);
        }
        Ok(())
    }
    fn end_frame(&mut self) -> Result<(), RenderError> {
        if !unsafe { GdiFlush() }.as_bool() {
            return Err(error("GdiFlush failed"));
        }
        Ok(())
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        if width == 0 || height == 0 {
            return Err(error("empty bitmap"));
        }
        if width + 64 > self.capacity_width || height > self.capacity_height {
            let w = width.max(self.capacity_width).next_multiple_of(256) + 64;
            let h = height.max(self.capacity_height).next_multiple_of(128);
            unsafe {
                self.target.Resize(w, h).map_err(error)?;
            }
            self.capacity_width = w;
            self.capacity_height = h;
            self.bits()?;
        }
        self.width = width;
        self.height = height;
        self.pending.clear();
        self.scrolled.clear();
        self.damage = vec![Area::full(width, height)];
        Ok(())
    }
    fn set_dpi(&mut self, dpi: u32) -> Result<(), RenderError> {
        if dpi == 0 {
            return Err(error("zero DPI"));
        }
        self.dpi = dpi;
        self.scrolled.clear();
        unsafe {
            self.target
                .SetPixelsPerDip(dpi as f32 / 96.0)
                .map_err(error)?;
        }
        self.damage = vec![Area::full(self.width, self.height)];
        Ok(())
    }
    fn render_to_bitmap(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Bitmap, RenderError> {
        let mut other = Self::new(width, height)?;
        other.set_dpi(self.dpi)?;
        other.params = unsafe {
            other
                .factory
                .CreateCustomRenderingParams(
                    2.2,
                    1.0,
                    1.0,
                    DWRITE_PIXEL_GEOMETRY_RGB,
                    DWRITE_RENDERING_MODE_NATURAL,
                )
                .map_err(error)?
        };
        for (id, (name, size)) in &self.ids {
            other.register_font(*id, name, *size)?;
        }
        other.begin_frame()?;
        other.draw(list)?;
        other.end_frame()?;
        other.bitmap()
    }
}
