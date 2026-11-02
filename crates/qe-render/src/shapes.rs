//! Shapes write straight to the pixels of the back buffer.
use crate::geom::{Area, snap};
use qe_api::Color;
use std::{collections::HashMap, rc::Rc};

/// The pixels of the back buffer. Every caller passes an area already clipped to
/// the bitmap, and `resize` refreshes this value whenever the pixels move.
#[derive(Clone, Copy)]
pub(crate) struct Surface {
    pub bits: *mut u8,
    pub stride: isize,
    pub width: i32,
    pub height: i32,
}
impl Surface {
    /// The pixels from `x0` up to `x1` in row `y`, as 32-bit BGRA values.
    // The DIB belongs to DirectWrite, not to this value, and spans never overlap in
    // use. See the safety note below.
    #[allow(clippy::mut_from_ref)]
    #[inline]
    pub fn span(&self, y: i32, x0: i32, x1: i32) -> &mut [u32] {
        assert!(y >= 0 && y < self.height && x0 >= 0 && x0 <= x1 && x1 <= self.width);
        // SAFETY: the assert keeps the span inside the 32-bit DIB, whose rows are
        // `stride` bytes apart. Rows are 4-byte aligned. Spans are used one at a
        // time on the window thread, and DirectWrite and GDI do not touch the DIB
        // while one is alive.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.bits
                    .offset(y as isize * self.stride + x0 as isize * 4)
                    .cast::<u32>(),
                (x1 - x0) as usize,
            )
        }
    }
    pub fn copy_rows(&self, area: Area, shift: i32) {
        let rows = area.b - area.y - shift.abs();
        let bytes = (area.r - area.x) as usize * 4;
        for i in 0..rows {
            let row = if shift > 0 { rows - 1 - i } else { i };
            let (from, to) = if shift > 0 {
                (area.y + row, area.y + shift + row)
            } else {
                (area.y - shift + row, area.y + row)
            };
            assert!(from >= 0 && to >= 0 && from.max(to) < self.height);
            assert!(area.x >= 0 && area.r <= self.width);
            // SAFETY: the asserts keep both rows inside the DIB.
            unsafe {
                let start = self.bits.offset(area.x as isize * 4);
                std::ptr::copy(
                    start.offset(from as isize * self.stride),
                    start.offset(to as isize * self.stride),
                    bytes,
                );
            }
        }
    }
}

#[inline]
fn pack(c: Color) -> u32 {
    u32::from_le_bytes([c.b, c.g, c.r, 255])
}
#[inline]
fn mix(ink: u32, destination: u32, alpha: u32) -> u32 {
    (ink * alpha + destination * (255 - alpha) + 127) / 255
}
#[inline]
fn blend(p: &mut u32, c: Color, alpha: u32) {
    let d = *p;
    let b = mix(u32::from(c.b), d & 255, alpha);
    let g = mix(u32::from(c.g), (d >> 8) & 255, alpha);
    let r = mix(u32::from(c.r), (d >> 16) & 255, alpha);
    *p = b | (g << 8) | (r << 16) | 0xFF00_0000;
}
fn fill_span(surface: Surface, y: i32, x0: i32, x1: i32, c: Color) {
    if x0 >= x1 || c.a == 0 {
        return;
    }
    let span = surface.span(y, x0, x1);
    if c.a == 255 {
        span.fill(pack(c));
    } else {
        let alpha = u32::from(c.a);
        for p in span {
            blend(p, c, alpha);
        }
    }
}
#[inline]
fn cover_pixel(surface: Surface, x: i32, y: i32, c: Color, coverage: u8) {
    if coverage == 0 {
        return;
    }
    let alpha = (u32::from(c.a) * u32::from(coverage) + 127) / 255;
    blend(&mut surface.span(y, x, x + 1)[0], c, alpha);
}
pub(crate) fn fill(surface: Surface, area: Area, c: Color, clip: Area) {
    let a = area.clip(clip);
    if a.valid() {
        for y in a.y..a.b {
            fill_span(surface, y, a.x, a.r, c);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct CornerKey {
    radius: u32,
    stroke: i32,
    columns: i32,
    rows: i32,
    width: i32,
    height: i32,
}
/// How much of each pixel the top-left corner covers. The other three corners are
/// its mirror images, because the shape's edges lie on whole pixels.
struct CornerMask {
    columns: i32,
    rows: i32,
    coverage: Vec<u8>,
}
#[derive(Default)]
pub(crate) struct Corners {
    masks: HashMap<CornerKey, Rc<CornerMask>>,
    bytes: usize,
}
/// One mask costs a byte per pixel of its corner. A mask over this limit is not kept.
const MOST_CORNER_BYTES: usize = 1024 * 1024;

fn inside(x: f32, y: f32, width: f32, height: f32, inset: f32, radius: f32) -> bool {
    let (left, top, right, bottom) = (inset, inset, width - inset, height - inset);
    if x < left || x >= right || y < top || y >= bottom {
        return false;
    }
    let r = (radius - inset).max(0.0);
    let dx = x - x.clamp(left + r, right - r);
    let dy = y - y.clamp(top + r, bottom - r);
    dx * dx + dy * dy <= r * r
}
fn make_mask(key: CornerKey) -> CornerMask {
    let radius = key.radius as f32 / 16.0;
    let mut coverage = Vec::with_capacity((key.columns * key.rows) as usize);
    let (w, h) = (key.width as f32, key.height as f32);
    for row in 0..key.rows {
        for column in 0..key.columns {
            let mut hits = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = column as f32 + (sx as f32 + 0.5) / 4.0;
                    let y = row as f32 + (sy as f32 + 0.5) / 4.0;
                    if inside(x, y, w, h, 0.0, radius)
                        && (key.stroke == 0 || !inside(x, y, w, h, key.stroke as f32, radius))
                    {
                        hits += 1;
                    }
                }
            }
            coverage.push(((hits * 255 + 8) / 16) as u8);
        }
    }
    CornerMask {
        columns: key.columns,
        rows: key.rows,
        coverage,
    }
}
impl Corners {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    /// A rounded rectangle whose edges lie on whole pixels. `stroke` of 0 fills it;
    /// otherwise only a border of that many pixels is drawn.
    pub fn rounded(
        &mut self,
        surface: Surface,
        shape: Area,
        radius: f32,
        stroke: i32,
        c: Color,
        clip: Area,
    ) {
        let visible = shape.clip(clip);
        if !visible.valid() || c.a == 0 {
            return;
        }
        let (width, height) = (shape.r - shape.x, shape.b - shape.y);
        let least = width.min(height);
        let stroke = if stroke * 2 >= least { 0 } else { stroke };
        let radius = radius.max(0.0).min(least as f32 / 2.0);
        // A sixteenth of a pixel is finer than anyone can see, and it keeps the
        // number of masks small.
        let radius_key = (radius * 16.0).round() as u32;
        let reach = (radius_key.div_ceil(16) as i32).max(stroke);
        if reach == 0 {
            fill(surface, shape, c, clip);
            return;
        }
        let columns = reach.min((width + 1) / 2);
        let rows = reach.min((height + 1) / 2);
        let key = CornerKey {
            radius: radius_key,
            stroke,
            columns,
            rows,
            width: width.min(reach * 2),
            height: height.min(reach * 2),
        };
        let mask = match self.masks.get(&key) {
            Some(mask) => mask.clone(),
            None => {
                let mask = Rc::new(make_mask(key));
                let bytes = mask.coverage.len();
                if bytes <= MOST_CORNER_BYTES {
                    if self.bytes + bytes > MOST_CORNER_BYTES {
                        self.masks.clear();
                        self.bytes = 0;
                    }
                    self.bytes += bytes;
                    self.masks.insert(key, mask.clone());
                }
                mask
            }
        };
        let middle_left = shape.x + mask.columns;
        let middle_right = (shape.r - mask.columns).max(middle_left);
        for y in visible.y..visible.b {
            let from_edge = (y - shape.y).min(shape.b - 1 - y);
            if from_edge >= mask.rows {
                if stroke == 0 {
                    fill_span(surface, y, visible.x, visible.r, c);
                } else {
                    let left_end = (shape.x + stroke).min(visible.r);
                    let right_start = (shape.r - stroke).max(visible.x);
                    fill_span(surface, y, visible.x, left_end, c);
                    fill_span(surface, y, right_start, visible.r, c);
                }
                continue;
            }
            let row =
                &mask.coverage[(from_edge * mask.columns) as usize..][..mask.columns as usize];
            for x in visible.x..middle_left.min(visible.r) {
                cover_pixel(surface, x, y, c, row[(x - shape.x) as usize]);
            }
            if stroke == 0 || from_edge < stroke {
                fill_span(
                    surface,
                    y,
                    middle_left.max(visible.x),
                    middle_right.min(visible.r),
                    c,
                );
            }
            for x in middle_right.max(visible.x)..visible.r {
                cover_pixel(surface, x, y, c, row[(shape.r - 1 - x) as usize]);
            }
        }
    }
}

/// A straight line that is exactly level or upright is a filled rectangle on whole
/// pixels. It returns `false` for any other line.
pub(crate) fn axis_line(
    surface: Surface,
    from: (f64, f64),
    to: (f64, f64),
    half: f64,
    c: Color,
    clip: Area,
) -> bool {
    let snapped_pair = |low: f64, high: f64| {
        let (a, b) = (snap(low), snap(high));
        (a, if a == b { a + 1 } else { b })
    };
    let area = if from.1 == to.1 && from.0 != to.0 {
        let (x, r) = snapped_pair(from.0.min(to.0), from.0.max(to.0));
        let (y, b) = snapped_pair(from.1 - half, from.1 + half);
        Area { x, y, r, b }
    } else if from.0 == to.0 && from.1 != to.1 {
        let (y, b) = snapped_pair(from.1.min(to.1), from.1.max(to.1));
        let (x, r) = snapped_pair(from.0 - half, from.0 + half);
        Area { x, y, r, b }
    } else {
        return false;
    };
    fill(surface, area, c, clip);
    true
}

/// A soft line with round ends. It works in numbers relative to whole pixels near
/// its start, so a whole-pixel scroll gives exactly the same coverage.
pub(crate) fn soft_line(
    surface: Surface,
    from: (f64, f64),
    to: (f64, f64),
    half: f64,
    c: Color,
    clip: Area,
) {
    let (home_x, home_y) = (from.0.floor(), from.1.floor());
    let quantize = |v: f64| ((v * 256.0).round() / 256.0) as f32;
    let (x0, y0) = (quantize(from.0 - home_x), quantize(from.1 - home_y));
    let (x1, y1) = (quantize(to.0 - home_x), quantize(to.1 - home_y));
    let half = half as f32;
    let (home_x, home_y) = (home_x as i32, home_y as i32);
    let reach = half + 1.0;
    let a = Area {
        x: home_x.saturating_add((x0.min(x1) - reach).floor() as i32),
        y: home_y.saturating_add((y0.min(y1) - reach).floor() as i32),
        r: home_x.saturating_add((x0.max(x1) + reach).ceil() as i32),
        b: home_y.saturating_add((y0.max(y1) + reach).ceil() as i32),
    }
    .clip(clip);
    if !a.valid() {
        return;
    }
    let (dx, dy) = (x1 - x0, y1 - y0);
    let length = dx * dx + dy * dy;
    // For each row, only the columns near the line can have any coverage.
    let columns_per_row = if dy != 0.0 { dx / dy } else { 0.0 };
    let row_reach = if dy != 0.0 {
        reach * (length.sqrt() / dy.abs()) + 1.0
    } else {
        f32::INFINITY
    };
    for py in a.y..a.b {
        let local_y = (py - home_y) as f32;
        let (left, right) = if row_reach.is_finite() {
            let t = (local_y + 0.5 - y0).clamp(dy.min(0.0), dy.max(0.0));
            let center = x0 + t * columns_per_row;
            (
                home_x.saturating_add((center - row_reach).floor() as i32),
                home_x.saturating_add((center + row_reach).ceil() as i32),
            )
        } else {
            (a.x, a.r)
        };
        for px in left.max(a.x)..right.min(a.r) {
            let local_x = (px - home_x) as f32;
            let mut hits = 0u32;
            for oy in [0.25, 0.75] {
                for ox in [0.25, 0.75] {
                    let (x, y) = (local_x + ox, local_y + oy);
                    let t = if length > 0.0 {
                        (((x - x0) * dx + (y - y0) * dy) / length).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let (ex, ey) = (x - x0 - t * dx, y - y0 - t * dy);
                    if ex * ex + ey * ey <= half * half {
                        hits += 1;
                    }
                }
            }
            cover_pixel(surface, px, py, c, (hits * 255 / 4) as u8);
        }
    }
}
