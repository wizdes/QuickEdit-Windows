//! A warm frame must not allocate once per glyph or once per run. This binary has
//! one test, so nothing else moves the allocation counter.
mod support;
use qe_api::{Dip, DisplayList, DrawCommand};
use qe_render::BitmapRenderer;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
};
use support::*;

struct Counting;
static COUNT: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNT.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn a_warm_frame_does_not_allocate_per_glyph_or_run() {
    let mut renderer = BitmapRenderer::new(800, 600).unwrap();
    renderer.register_font(1, MONO).unwrap();
    let mut list = DisplayList::default();
    fill(&mut list, rect(0.0, 0.0, 800.0, 600.0), WHITE);
    list.push(DrawCommand::RoundedRect {
        rect: rect(8.0, 8.0, 160.0, 32.0),
        radius: Dip(8.0),
        color: BLACK,
    });
    for row in 0..30 {
        let y = 15.0 + row as f32 * 17.0;
        let line = "The quick brown fox jumps over the lazy dog 0123456789";
        list.push(text_run(MONO, 13.0, line, point(2.0, y), BLACK));
        list.push(glyph_run(1, MONO, 13.0, line, point(2.0, y + 8.0), BLACK));
    }
    frame(&mut renderer, &list);
    renderer.invalidate_all();
    frame(&mut renderer, &list);
    renderer.invalidate_all();
    let before = COUNT.load(Ordering::Relaxed);
    frame(&mut renderer, &list);
    let allocations = COUNT.load(Ordering::Relaxed) - before;
    assert!(allocations <= 8, "{allocations} allocations for 60 runs");
}
