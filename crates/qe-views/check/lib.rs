//! What the tests here share: a window's worth of editor and frame, the real renderer, and a
//! small PNG writer, so that a picture can be looked at.
use qe_api::{
    Dip, DisplayList, DrawCommand, InputEvent, InputKind, Rect, Renderer, Theme, ThemeKind,
    ViewContext, ViewRequest,
};
use qe_editor::EditorView;
use qe_layout::DirectWriteLayout;
use qe_pdf::deflate::Compressor;
use qe_render::BitmapRenderer;
use qe_views::{Paper, ViewFrame};
use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;

pub const FAMILY: &str = "Cascadia Mono";

pub struct Scene {
    pub editor: EditorView,
    pub frame: ViewFrame,
    pub measure: DirectWriteLayout,
    pub clock: Rc<Cell<f64>>,
    pub bounds: Rect,
}

pub fn rect(width: f32, height: f32) -> Rect {
    Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(width),
        height: Dip(height),
    }
}

impl Scene {
    pub fn new(editor: EditorView, width: f32, height: f32, paper: Paper) -> Self {
        let mut scene = Self::unsettled(editor, width, height, paper, None);
        scene.settle(&mut Vec::new());
        scene
    }
    /// The way a host opens a file: it knows the file's size first. It does not wait for
    /// the layout, because a large file's index takes a while.
    pub fn with_file_bytes(
        editor: EditorView,
        width: f32,
        height: f32,
        bytes: Option<u64>,
    ) -> Self {
        Self::unsettled(editor, width, height, Paper::LETTER, bytes)
    }
    fn unsettled(
        editor: EditorView,
        width: f32,
        height: f32,
        paper: Paper,
        bytes: Option<u64>,
    ) -> Self {
        let measure =
            DirectWriteLayout::with_font(FAMILY, 10.0, DWRITE_FONT_WEIGHT_NORMAL).unwrap();
        let mut editor = editor;
        editor
            .apply_font(FAMILY, 10.0, &mut ViewContext::default())
            .unwrap();
        let mut frame = ViewFrame::new(paper, FAMILY, 10.0);
        frame.set_reduce_motion(false);
        frame.set_file_bytes(bytes);
        let clock = Rc::new(Cell::new(10.0));
        let shared = clock.clone();
        frame.set_clock(Box::new(move || shared.get()));
        let mut scene = Self {
            editor,
            frame,
            measure,
            clock,
            bounds: rect(width, height),
        };
        let mut ctx = ViewContext::default();
        scene
            .frame
            .layout(&mut scene.editor, scene.bounds, &scene.measure, &mut ctx);
        scene
    }
    pub fn from_text(text: &str, width: f32, height: f32) -> Self {
        Self::new(
            EditorView::from_text(text).unwrap(),
            width,
            height,
            Paper::LETTER,
        )
    }
    /// Runs layout and the map's worker until both rest, and keeps what they ask to repaint.
    pub fn settle(&mut self, repaint: &mut Vec<Rect>) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            self.clock.set(self.clock.get() + 0.2);
            let mut ctx = ViewContext::default();
            self.editor.poll_layout(&mut ctx);
            self.frame.sync(&mut self.editor, &self.measure, &mut ctx);
            take_repaints(&ctx, repaint);
            if self.editor.visible_rows().complete && !self.frame.map_job_running() {
                let mut ctx = ViewContext::default();
                self.editor.poll_layout(&mut ctx);
                self.frame.sync(&mut self.editor, &self.measure, &mut ctx);
                take_repaints(&ctx, repaint);
                if self.editor.visible_rows().complete
                    && !self.frame.map_job_running()
                    && !ctx
                        .requests
                        .iter()
                        .any(|r| matches!(r, ViewRequest::RequestTimer(_)))
                {
                    return;
                }
            }
            assert!(Instant::now() < deadline, "the scene never settled");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    pub fn send(&mut self, kind: InputKind, repaint: &mut Vec<Rect>) {
        let mut ctx = ViewContext::default();
        self.frame.handle(
            &mut self.editor,
            &InputEvent { time_us: 0, kind },
            &self.measure,
            &mut ctx,
        );
        take_repaints(&ctx, repaint);
    }
    pub fn list(&self, kind: ThemeKind) -> DisplayList {
        let mut list = DisplayList::default();
        self.frame
            .draw(&self.editor, &mut list, &Theme::new(kind), &self.measure);
        list
    }
}

pub fn take_repaints(ctx: &ViewContext, into: &mut Vec<Rect>) {
    for request in &ctx.requests {
        if let ViewRequest::Invalidate(rect) = request {
            into.push(*rect);
        }
    }
}

/// A renderer at this size and DPI, in dips.
pub fn renderer(width: f32, height: f32, dpi: u32) -> BitmapRenderer {
    let scale = dpi as f32 / 96.0;
    let mut renderer = BitmapRenderer::new(
        (width * scale).ceil() as u32,
        (height * scale).ceil() as u32,
    )
    .unwrap();
    renderer.set_dpi(dpi).unwrap();
    renderer
}

/// Gives the renderer every font the list names, as the app does.
pub fn register_fonts(renderer: &mut BitmapRenderer, list: &DisplayList, seen: &mut HashSet<u32>) {
    for command in &list.commands {
        if let DrawCommand::GlyphRun { font_id, .. } = command
            && seen.insert(*font_id)
        {
            let face = qe_layout::font_face(*font_id).unwrap();
            renderer.register_font_face(*font_id, &face);
        }
    }
}

/// Paints the frame's pending regions.
pub fn paint(renderer: &mut BitmapRenderer, list: &DisplayList, seen: &mut HashSet<u32>) {
    register_fonts(renderer, list, seen);
    renderer.begin_frame().unwrap();
    renderer.draw(list).unwrap();
    renderer.end_frame().unwrap();
    assert!(
        renderer.draw_errors().is_empty(),
        "{:?}",
        renderer.draw_errors()
    );
}

/// The folder for pictures and sample PDFs, outside the repo.
pub fn output_folder() -> PathBuf {
    let folder = std::env::var_os("QE_CHECK_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("qe-views-check"));
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn crc32(data: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for part in data {
        for &byte in *part {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
    }
    !crc
}

/// Writes 32-bit BGRA pixels as a PNG.
pub fn write_png(path: &std::path::Path, width: u32, height: u32, bgra: &[u8]) {
    let mut raw = Vec::with_capacity((width as usize * 3 + 1) * height as usize);
    for row in bgra.chunks_exact(width as usize * 4) {
        raw.push(0);
        for pixel in row.as_chunks::<4>().0 {
            raw.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
    }
    let packed = Compressor::default().compress(&raw);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |name: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&[name, data]).to_be_bytes());
    };
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &packed);
    chunk(b"IEND", &[]);
    std::fs::write(path, out).unwrap();
}

/// A repeatable random number, so that a failure can be run again.
pub struct Steps(pub u64);
impl Steps {
    pub fn next(&mut self, below: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % below
    }
}

pub fn mixed_document() -> String {
    let mut text = String::new();
    for n in 0..120 {
        match n % 8 {
            0 => text.push_str(&format!("fn line_{n}() {{ return {n}; }}\n")),
            1 => text.push_str(
                "The quick brown fox jumps over the lazy dog, and then it wraps around the \
                 right edge of the page because this line is much longer than one row.\n",
            ),
            2 => text.push_str("\tindented with a tab\tand another\n"),
            3 => text.push_str(
                "\u{645}\u{631}\u{62d}\u{628}\u{627} \u{628}\u{627}\u{644}\u{639}\u{627}\u{644}\u{645} \
                 mixed with english \u{5e9}\u{5dc}\u{5d5}\u{5dd}\n",
            ),
            4 => text.push_str(
                "\u{6f22}\u{5b57}\u{304b}\u{306a}\u{30ab}\u{30ca}\u{d55c}\u{ae00} \u{6f22}\u{5b57}\u{304b}\u{306a}\u{30ab}\u{30ca}\u{d55c}\u{ae00} \u{6f22}\u{5b57}\u{304b}\u{306a}\u{30ab}\u{30ca}\u{d55c}\u{ae00} \u{6f22}\u{5b57}\u{304b}\u{306a}\u{30ab}\u{30ca}\u{d55c}\u{ae00} \u{6f22}\u{5b57}\u{304b}\u{306a}\u{30ab}\u{30ca}\u{d55c}\u{ae00}\n",
            ),
            5 => text.push_str("e\u{301}a\u{308} combining marks q\u{301}\u{302} and emoji \u{1f600}\u{1f44d}\u{1f3fd}\n"),
            6 => text.push('\n'),
            _ => text.push_str(&"x".repeat(200 + n)),
        }
        if n % 8 == 7 {
            text.push('\n');
        }
    }
    text
}
