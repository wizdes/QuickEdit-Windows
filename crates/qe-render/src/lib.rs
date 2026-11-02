//! One 32-bit bitmap holds the window's pixels. Shapes and text write to it directly,
//! and GDI copies it to the window. No GPU DLL loads. See README.md for the rules.
#![allow(hidden_glob_reexports)]

// `#[implement]` writes paths that start at `windows_core`. This crate stands in
// for that crate, inside this crate only, so that it needs no extra dependency.
extern crate self as windows_core;
#[allow(unused_imports)]
pub(crate) use windows::core::*;

mod geom;
mod shapes;
mod text;

use geom::{Area, Regions};
use qe_api::{
    Bitmap, Color, Dip, DisplayList, DrawCommand, PixelRect, Point, Rect, RenderError, Renderer,
};
use shapes::{Corners, Surface};
use std::{mem, result::Result};
use text::{RunRef, Text};
use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::{DirectWrite::*, Gdi::*},
};

pub(crate) fn error(e: impl std::fmt::Display) -> RenderError {
    RenderError::new(e.to_string())
}

/// Names a font for each font ID that `GlyphRun` commands use. A `GlyphRun` carries
/// its own size. For faces that have no family name to look up, such as fallback
/// fonts from qe-layout, use `BitmapRenderer::register_font_face`.
pub trait FontRegistry {
    fn font(&self, id: u32) -> Option<&str>;
}

/// How text turns into coverage and blends. `BitmapRenderer` starts with the system's
/// values for the primary monitor. Tests with saved images use `reference`, because
/// the system's values can differ between accounts and PCs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextSettings {
    pub gamma: f32,
    pub enhanced_contrast: f32,
    pub cleartype_level: f32,
    pub geometry: DWRITE_PIXEL_GEOMETRY,
    pub mode: DWRITE_RENDERING_MODE,
}
impl TextSettings {
    /// Fixed values that do not depend on this PC.
    pub fn reference() -> Self {
        Self {
            gamma: 2.2,
            enhanced_contrast: 1.0,
            cleartype_level: 1.0,
            geometry: DWRITE_PIXEL_GEOMETRY_RGB,
            mode: DWRITE_RENDERING_MODE_NATURAL,
        }
    }
}

/// What the current frame did, for tests and benchmarks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    pub regions: usize,
    pub pixels: u64,
    pub glyphs_rasterized: u64,
    pub glyphs_drawn: u64,
}

pub struct BitmapRenderer {
    target: IDWriteBitmapRenderTarget,
    surface: Surface,
    text: Text,
    corners: Corners,
    width: u32,
    height: u32,
    capacity_width: u32,
    capacity_height: u32,
    dpi: u32,
    /// Waiting for the next frame.
    pending: Regions,
    /// What each draw of this frame paints.
    frame: Regions,
    /// Changed pixels that the window has not received yet.
    damage: Regions,
    errors: Vec<RenderError>,
    stats: FrameStats,
    clips: Vec<Area>,
    opacities: Vec<f32>,
}

fn surface_of(
    target: &IDWriteBitmapRenderTarget,
    width: u32,
    height: u32,
) -> Result<Surface, RenderError> {
    let dc = unsafe { target.GetMemoryDC() };
    let object = unsafe { GetCurrentObject(dc, OBJ_BITMAP) };
    let mut dib: DIBSECTION = unsafe { mem::zeroed() };
    let size = unsafe {
        GetObjectW(
            object,
            mem::size_of::<DIBSECTION>() as i32,
            Some((&mut dib as *mut DIBSECTION).cast()),
        )
    };
    if size < mem::size_of::<DIBSECTION>() as i32
        || dib.dsBm.bmBits.is_null()
        || dib.dsBm.bmBitsPixel != 32
    {
        return Err(error("DirectWrite did not give a 32-bit DIB"));
    }
    Ok(Surface {
        bits: dib.dsBm.bmBits.cast(),
        stride: dib.dsBm.bmWidthBytes as isize,
        width: width as i32,
        height: height as i32,
    })
}
fn to_rect(a: Area) -> RECT {
    RECT {
        left: a.x,
        top: a.y,
        right: a.r,
        bottom: a.b,
    }
}
fn fade(mut c: Color, opacity: f32) -> Color {
    c.a = (f32::from(c.a) * opacity).round().clamp(0.0, 255.0) as u8;
    c
}
fn parse_icon(id: &str) -> Option<u32> {
    let digits = id
        .strip_prefix("U+")
        .or_else(|| id.strip_prefix("0x"))
        .unwrap_or(id);
    u32::from_str_radix(digits, 16).ok()
}

const ICON_FONT: &str = "Segoe Fluent Icons";

impl BitmapRenderer {
    pub fn new(width: u32, height: u32) -> Result<Self, RenderError> {
        if width == 0 || height == 0 {
            return Err(error("empty bitmap"));
        }
        let text = Text::new()?;
        let capacity_width = width.next_multiple_of(256);
        let capacity_height = height.next_multiple_of(128);
        let target = unsafe {
            text.gdi_interop()?
                .CreateBitmapRenderTarget(None, capacity_width, capacity_height)
                .map_err(error)?
        };
        let surface = surface_of(&target, capacity_width, capacity_height)?;
        let mut pending = Regions::default();
        pending.add(Area::full(width, height));
        Ok(Self {
            target,
            surface,
            text,
            corners: Corners::default(),
            width,
            height,
            capacity_width,
            capacity_height,
            dpi: 96,
            pending,
            frame: Regions::default(),
            damage: Regions::default(),
            errors: Vec::new(),
            stats: FrameStats::default(),
            clips: Vec::new(),
            opacities: Vec::new(),
        })
    }
    fn full(&self) -> Area {
        Area::full(self.width, self.height)
    }
    fn scale64(&self) -> f64 {
        f64::from(self.dpi) / 96.0
    }
    /// Device pixels per dip.
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }
    pub fn text_settings(&self) -> TextSettings {
        self.text.settings()
    }
    /// Changes how text is made. All text draws through the same path, whatever the
    /// settings are. This does not ask for a repaint.
    pub fn set_text_settings(&mut self, settings: TextSettings) -> Result<(), RenderError> {
        self.text.set_settings(settings)
    }
    pub fn bitmap(&self) -> Result<Bitmap, RenderError> {
        let mut bgra = vec![0; self.width as usize * self.height as usize * 4];
        for (y, row) in bgra.chunks_exact_mut(self.width as usize * 4).enumerate() {
            let pixels = self.surface.span(y as i32, 0, self.width as i32);
            for (out, p) in row.as_chunks_mut::<4>().0.iter_mut().zip(pixels) {
                *out = p.to_le_bytes();
            }
        }
        Ok(Bitmap {
            width: self.width,
            height: self.height,
            bgra,
        })
    }
    /// Asks the next frame to repaint this rectangle. An empty or off-screen
    /// rectangle asks for nothing.
    pub fn invalidate(&mut self, rect: Rect) {
        let area = Area::outward(rect, self.scale64()).clip(self.full());
        self.pending.add(area);
    }
    /// Asks the next frame to repaint the whole bitmap. With nothing pending, a frame
    /// does nothing.
    pub fn invalidate_all(&mut self) {
        self.pending.clear();
        self.pending.add(self.full());
    }
    pub fn back_buffer_bytes(&self) -> usize {
        self.capacity_width as usize * self.capacity_height as usize * 4
    }
    /// Copies everything changed since the last successful present to the window. A
    /// failure keeps the changes, so the next present still copies them.
    pub fn present(&mut self, hwnd: HWND) -> Result<(), RenderError> {
        let dc = unsafe { GetDC(Some(hwnd)) };
        if dc.0.is_null() {
            return Err(error("GetDC failed"));
        }
        let result = self.present_to_dc(dc);
        unsafe {
            ReleaseDC(Some(hwnd), dc);
        }
        result
    }
    /// `present` for a device context that the caller already holds.
    pub fn present_to_dc(&mut self, dc: HDC) -> Result<(), RenderError> {
        for i in 0..self.damage.list.len() {
            self.copy_area(dc, self.damage.list[i])?;
        }
        if !unsafe { GdiFlush() }.as_bool() {
            return Err(error("GdiFlush failed"));
        }
        self.damage.clear();
        Ok(())
    }
    /// Copies any rectangle of device pixels to the window, with no new draw. WM_PAINT
    /// uses this with the rectangle from `BeginPaint`. Pending changes stay pending.
    pub fn present_rect(&self, hwnd: HWND, rect: RECT) -> Result<(), RenderError> {
        let dc = unsafe { GetDC(Some(hwnd)) };
        if dc.0.is_null() {
            return Err(error("GetDC failed"));
        }
        let result = self.present_rect_to_dc(dc, rect);
        unsafe {
            ReleaseDC(Some(hwnd), dc);
        }
        result
    }
    /// `present_rect` for a device context that the caller already holds, such as the
    /// one from `BeginPaint`.
    pub fn present_rect_to_dc(&self, dc: HDC, rect: RECT) -> Result<(), RenderError> {
        let area = Area {
            x: rect.left,
            y: rect.top,
            r: rect.right,
            b: rect.bottom,
        }
        .clip(self.full());
        self.copy_area(dc, area)?;
        if !unsafe { GdiFlush() }.as_bool() {
            return Err(error("GdiFlush failed"));
        }
        Ok(())
    }
    fn copy_area(&self, dc: HDC, a: Area) -> Result<(), RenderError> {
        if !a.valid() {
            return Ok(());
        }
        let source = unsafe { self.target.GetMemoryDC() };
        unsafe {
            BitBlt(
                dc,
                a.x,
                a.y,
                a.r - a.x,
                a.b - a.y,
                Some(source),
                a.x,
                a.y,
                SRCCOPY,
            )
            .map_err(error)
        }
    }
    /// The rectangles of device pixels that `present` would copy now.
    pub fn damage_rects(&self) -> Vec<RECT> {
        self.damage.list.iter().copied().map(to_rect).collect()
    }
    /// Moves the pixels of `rect` by `dy` dips.
    ///
    /// Rule: pixels can only move by whole device pixels, and the list drawn next has
    /// positions that may round differently after any other shift. So a `dy` that is a
    /// whole number of device pixels copies rows and repaints only the revealed strip.
    /// Any other `dy` copies nothing and repaints all of `rect`. That is always right
    /// and never drifts, but it costs a repaint. For speed, keep the scroll position on
    /// whole device pixels: `dy = pixels / scale()`, or use `scroll_pixels`.
    pub fn scroll(&mut self, rect: Rect, dy: Dip) -> Result<(), RenderError> {
        let device = f64::from(dy.0) * self.scale64();
        if device == 0.0 {
            return Ok(());
        }
        let whole = device.round();
        if whole != 0.0 && (device - whole).abs() < 1.0 / 256.0 {
            return self.scroll_pixels(rect, whole as i32);
        }
        let area = Area::snapped(rect, self.scale64()).clip(self.full());
        self.pending.add(area);
        Ok(())
    }
    /// Moves the pixels of `rect` by `dy` device pixels. It repaints only the part that
    /// the move reveals, and moves any rectangles already waiting for a repaint.
    pub fn scroll_pixels(&mut self, rect: Rect, dy: i32) -> Result<(), RenderError> {
        let area = Area::snapped(rect, self.scale64()).clip(self.full());
        if !area.valid() || dy == 0 {
            return Ok(());
        }
        let height = area.b - area.y;
        let amount = dy.saturating_abs().min(height);
        let shift = if dy > 0 { amount } else { -amount };
        if amount < height {
            self.surface.copy_rows(area, shift);
        }
        self.pending.scroll(area, shift);
        self.frame.scroll(area, shift);
        self.pending.add(if shift > 0 {
            Area {
                b: area.y + amount,
                ..area
            }
        } else {
            Area {
                y: area.b - amount,
                ..area
            }
        });
        self.damage.add(area);
        Ok(())
    }
    /// Gives a font ID the first matching face of an installed family.
    pub fn register_font(&mut self, id: u32, family: &str) -> Result<(), RenderError> {
        let face = self.text.family(family)?;
        self.text.register_id(id, face);
        Ok(())
    }
    /// Gives a font ID this exact face. qe-layout's `font_face(font_id)` returns such a
    /// face, including fonts that fallback chose.
    pub fn register_font_face(&mut self, id: u32, face: &IDWriteFontFace) {
        let face = self.text.intern(face);
        self.text.register_id(id, face);
    }
    pub fn register_fonts(
        &mut self,
        registry: &dyn FontRegistry,
        ids: &[u32],
    ) -> Result<(), RenderError> {
        for &id in ids {
            let family = registry
                .font(id)
                .ok_or_else(|| error(format!("font ID {id} is missing from the registry")))?;
            self.register_font(id, family)?;
        }
        Ok(())
    }
    /// Problems from the last `draw`. Each one is listed once, and everything else in
    /// the list was still drawn.
    pub fn draw_errors(&self) -> &[RenderError] {
        &self.errors
    }
    pub fn frame_stats(&self) -> FrameStats {
        self.stats
    }
    pub fn glyph_cache_bytes(&self) -> usize {
        self.text.glyph_bytes()
    }
    pub fn glyph_cache_entries(&self) -> usize {
        self.text.glyph_entries()
    }
    /// The most bytes of glyph coverage to keep. The default is 32 MiB. Over the limit,
    /// the glyphs unused for longest leave first.
    pub fn set_glyph_cache_limit(&mut self, bytes: usize) {
        self.text.set_glyph_limit(bytes);
    }
    fn report(&mut self, problem: RenderError) {
        if !self.errors.contains(&problem) {
            self.errors.push(problem);
        }
    }
    fn device(&self, p: Point) -> (f64, f64) {
        let s = self.scale64();
        (f64::from(p.x.0) * s, f64::from(p.y.0) * s)
    }
    fn wavy(&mut self, from: Point, to: Point, c: Color, clip: Area) {
        let dx = to.x.0 - from.x.0;
        let count = ((dx.abs() / 2.0).ceil() as usize).min(1 << 20);
        let s = self.scale64();
        let half = s.max(1.0) / 2.0;
        let start = self.device(from);
        let delta = (f64::from(dx) * s, f64::from(to.y.0 - from.y.0) * s);
        let point = |j: usize| {
            let t = j as f64 / count.max(1) as f64;
            let wave = (t * f64::from(dx) * 1.5).sin() * 1.25 * s;
            (start.0 + delta.0 * t, start.1 + delta.1 * t + wave)
        };
        // Coverage reaches half a line width past the wave, plus a pixel.
        let reach = 1.25 * s + half + 1.0;
        let (top, bottom) = (
            start.1.min(start.1 + delta.1) - reach,
            start.1.max(start.1 + delta.1) + reach,
        );
        if bottom < f64::from(clip.y) || top > f64::from(clip.b) {
            return;
        }
        for i in 0..count {
            let (a, b) = (point(i), point(i + 1));
            if a.0.max(b.0) + reach < f64::from(clip.x) || a.0.min(b.0) - reach > f64::from(clip.r)
            {
                continue;
            }
            shapes::soft_line(self.surface, a, b, half, c, clip);
        }
    }
    /// Draws one command. `DrawCommand` has no catch-all arm here, so a new command
    /// cannot be dropped silently: it stops this file from compiling.
    fn command(&mut self, cmd: &DrawCommand, clip: Area, opacity: f32) -> Result<(), RenderError> {
        let s = self.scale64();
        match cmd {
            DrawCommand::FillRect { rect, color } => {
                shapes::fill(
                    self.surface,
                    Area::snapped(*rect, s),
                    fade(*color, opacity),
                    clip,
                );
                Ok(())
            }
            DrawCommand::FillRects { rects, color } => {
                let c = fade(*color, opacity);
                for r in rects {
                    shapes::fill(self.surface, Area::snapped(*r, s), c, clip);
                }
                Ok(())
            }
            DrawCommand::RoundedRect {
                rect,
                radius,
                color,
            } => {
                self.corners.rounded(
                    self.surface,
                    Area::snapped(*rect, s),
                    (f64::from(radius.0) * s) as f32,
                    0,
                    fade(*color, opacity),
                    clip,
                );
                Ok(())
            }
            DrawCommand::StrokeRect {
                rect,
                radius,
                width,
                color,
            } => {
                if width.0 > 0.0 {
                    let stroke = ((f64::from(width.0) * s).round() as i32).max(1);
                    self.corners.rounded(
                        self.surface,
                        Area::snapped(*rect, s),
                        (f64::from(radius.0) * s) as f32,
                        stroke,
                        fade(*color, opacity),
                        clip,
                    );
                }
                Ok(())
            }
            DrawCommand::Line {
                from,
                to,
                width,
                color,
            } => {
                if width.0 > 0.0 {
                    let (a, b) = (self.device(*from), self.device(*to));
                    let half = (f64::from(width.0) * s).max(1.0) / 2.0;
                    let c = fade(*color, opacity);
                    if !shapes::axis_line(self.surface, a, b, half, c, clip) {
                        shapes::soft_line(self.surface, a, b, half, c, clip);
                    }
                }
                Ok(())
            }
            DrawCommand::WavyUnderline { from, to, color } => {
                self.wavy(*from, *to, fade(*color, opacity), clip);
                Ok(())
            }
            DrawCommand::GlyphRun {
                at,
                bidi_level,
                font_id,
                font_em_size,
                glyph_ids,
                advances,
                offsets,
                color,
            } => {
                let face = self
                    .text
                    .face_of_id(*font_id)
                    .ok_or_else(|| error(format!("font ID {font_id} is not registered")))?;
                if advances.len() != glyph_ids.len() || offsets.len() != glyph_ids.len() {
                    return Err(error("the array lengths of a glyph run differ"));
                }
                let origin = self.device(*at);
                self.text.draw_run(
                    self.surface,
                    clip,
                    s,
                    origin,
                    RunRef {
                        face,
                        em: font_em_size.0,
                        bidi: *bidi_level,
                        ids: glyph_ids,
                        advances,
                        offsets,
                    },
                    fade(*color, opacity),
                )
            }
            DrawCommand::TextRun {
                at,
                text,
                font,
                font_size,
                color,
            } => {
                let shaped = self.text.shape(font, font_size.0, text)?;
                let origin = self.device(*at);
                self.text.draw_shaped(
                    self.surface,
                    clip,
                    s,
                    origin,
                    &shaped,
                    fade(*color, opacity),
                )
            }
            DrawCommand::Icon {
                at,
                icon_id,
                font_size,
                color,
            } => {
                let point =
                    parse_icon(icon_id).ok_or_else(|| error(format!("bad icon id {icon_id:?}")))?;
                let face = self.text.family(ICON_FONT)?;
                let glyph = self.text.icon_glyph(face, point)?;
                if glyph == 0 {
                    return Err(error(format!("icon {icon_id:?} is not in {ICON_FONT}")));
                }
                let origin = self.device(*at);
                self.text.draw_run(
                    self.surface,
                    clip,
                    s,
                    origin,
                    RunRef {
                        face,
                        em: font_size.0,
                        bidi: 0,
                        ids: &[glyph],
                        advances: &[Dip(0.0)],
                        offsets: &[Point {
                            x: Dip(0.0),
                            y: Dip(0.0),
                        }],
                    },
                    fade(*color, opacity),
                )
            }
            DrawCommand::PushClip(_)
            | DrawCommand::PopClip
            | DrawCommand::PushOpacity(_)
            | DrawCommand::PopOpacity => Ok(()),
        }
    }
    fn paint(&mut self, list: &DisplayList, area: Area) {
        let (mut clips, mut opacities) =
            (mem::take(&mut self.clips), mem::take(&mut self.opacities));
        clips.clear();
        opacities.clear();
        clips.push(area);
        opacities.push(1.0f32);
        let s = self.scale64();
        for cmd in &list.commands {
            let clip = clips[clips.len() - 1];
            let opacity = opacities[opacities.len() - 1];
            match cmd {
                DrawCommand::PushClip(r) => clips.push(clip.clip(Area::snapped(*r, s))),
                DrawCommand::PopClip => {
                    if clips.len() == 1 {
                        self.report(error("clip stack underflow"));
                    } else {
                        clips.pop();
                    }
                }
                DrawCommand::PushOpacity(a) => opacities.push(opacity * a.clamp(0.0, 1.0)),
                DrawCommand::PopOpacity => {
                    if opacities.len() == 1 {
                        self.report(error("opacity stack underflow"));
                    } else {
                        opacities.pop();
                    }
                }
                _ => {
                    if clip.valid()
                        && opacity > 0.0
                        && let Err(problem) = self.command(cmd, clip, opacity)
                    {
                        self.report(problem);
                    }
                }
            }
        }
        if clips.len() != 1 {
            self.report(error("the clip stack does not balance"));
        }
        if opacities.len() != 1 {
            self.report(error("the opacity stack does not balance"));
        }
        self.clips = clips;
        self.opacities = opacities;
    }
    /// Moves what is pending into this frame's regions.
    fn take_pending(&mut self) {
        for i in 0..self.pending.list.len() {
            let area = self.pending.list[i];
            self.frame.add(area);
        }
        self.pending.clear();
    }
}

impl Renderer for BitmapRenderer {
    /// Starts a frame. Each `draw` in it paints what was pending at this point, plus
    /// anything invalidated before that `draw`.
    fn begin_frame(&mut self) -> Result<(), RenderError> {
        self.stats = FrameStats::default();
        self.frame.clear();
        self.take_pending();
        Ok(())
    }
    /// Paints `list` inside the frame's regions, and does nothing when there are none.
    /// A bad command does not stop the others. All problems come back at the end as one
    /// `Err`, and `draw_errors` lists them. The pixels are still valid to present.
    ///
    /// The list must paint an opaque background over each region, because a translucent
    /// command blends with whatever the region holds.
    fn draw(&mut self, list: &DisplayList) -> Result<(), RenderError> {
        self.take_pending();
        self.errors.clear();
        self.text.next_draw();
        self.text.counts = Default::default();
        let regions = mem::take(&mut self.frame.list);
        for area in regions.iter().copied() {
            self.paint(list, area);
            self.damage.add(area);
        }
        self.stats.regions = regions.len();
        self.stats.pixels = regions.iter().map(|a| a.pixels()).sum();
        self.stats.glyphs_rasterized += self.text.counts.rasterized;
        self.stats.glyphs_drawn += self.text.counts.drawn;
        self.frame.list = regions;
        if self.errors.is_empty() {
            return Ok(());
        }
        let parts: Vec<&str> = self.errors.iter().map(|e| e.message.as_str()).collect();
        Err(RenderError::new(format!(
            "the rest of the list was drawn, except: {}",
            parts.join("; ")
        )))
    }
    fn end_frame(&mut self) -> Result<(), RenderError> {
        self.frame.clear();
        Ok(())
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        if width == 0 || height == 0 {
            return Err(error("empty bitmap"));
        }
        if width > self.capacity_width || height > self.capacity_height {
            let w = width.max(self.capacity_width).next_multiple_of(256);
            let h = height.max(self.capacity_height).next_multiple_of(128);
            unsafe {
                self.target.Resize(w, h).map_err(error)?;
            }
            self.capacity_width = w;
            self.capacity_height = h;
            self.surface = surface_of(&self.target, w, h)?;
        }
        self.width = width;
        self.height = height;
        self.damage.clear();
        self.frame.clear();
        self.invalidate_all();
        Ok(())
    }
    fn set_dpi(&mut self, dpi: u32) -> Result<(), RenderError> {
        if dpi == 0 {
            return Err(error("zero DPI"));
        }
        if dpi != self.dpi {
            self.dpi = dpi;
            self.frame.clear();
            self.invalidate_all();
        }
        Ok(())
    }
    /// For tests and export only: this builds a new renderer on each call, so its
    /// glyph cache starts cold. It draws through the same code as live drawing, with
    /// this renderer's text settings, DPI and fonts.
    fn render_to_bitmap(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Bitmap, RenderError> {
        let mut other = Self::new(width, height)?;
        other.set_text_settings(self.text.settings())?;
        other.set_dpi(self.dpi)?;
        for (id, face) in self.text.registered() {
            other.register_font_face(id, &face);
        }
        other.begin_frame()?;
        other.draw(list)?;
        other.end_frame()?;
        other.bitmap()
    }
    fn invalidate(&mut self, rect: Rect) {
        BitmapRenderer::invalidate(self, rect);
    }
    fn invalidate_all(&mut self) {
        BitmapRenderer::invalidate_all(self);
    }
    fn scroll(&mut self, rect: Rect, dy: Dip) -> Result<(), RenderError> {
        BitmapRenderer::scroll(self, rect, dy)
    }
    fn present(&mut self, window: *mut std::ffi::c_void) -> Result<(), RenderError> {
        BitmapRenderer::present(self, HWND(window))
    }
    fn present_rect(
        &mut self,
        dc: *mut std::ffi::c_void,
        rect: PixelRect,
    ) -> Result<(), RenderError> {
        self.present_rect_to_dc(
            HDC(dc),
            RECT {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            },
        )
    }
}
