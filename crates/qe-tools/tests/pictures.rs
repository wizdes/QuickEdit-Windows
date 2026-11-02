//! The find bar, Select Line and Complete's list through the real `qe-render`, at 96 DPI, in
//! light and dark. Saved images are compared by pixels. Update them with
//! `QE_TOOLS_UPDATE_IMAGES=1`, then look at each one.
#[path = "../../qe-ui/tests/support/png.rs"]
mod png;

use qe_api::{
    ByteOffset, CommandId, Dip, DisplayList, InputEvent, InputKind, Modifiers, Rect, Renderer,
    Theme, ThemeKind, View, ViewContext,
};
use qe_editor::EditorView;
use qe_layout::DirectWriteLayout;
use qe_render::{BitmapRenderer, TextSettings};
use qe_tools::ToolsHost;
use qe_ui::MemoryClipboard;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const WIDTH: u32 = 760;
const HEIGHT: u32 = 300;
const TEXT: &str = "The fox ran past the barn.\nA second fox hid in the hedge,\nand hello was all the farmer said.\nHelp came from the helmet maker.\nSay hel";

fn area() -> Rect {
    Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(WIDTH as f32),
        height: Dip(HEIGHT as f32),
    }
}

struct Scene {
    view: EditorView,
    tools: ToolsHost,
    ctx: ViewContext,
    measure: DirectWriteLayout,
}
impl Scene {
    fn new() -> Self {
        let measure = DirectWriteLayout::new().unwrap();
        let mut view = EditorView::from_text(TEXT).unwrap();
        let mut tools = ToolsHost::with_clipboard(MemoryClipboard::shared());
        let rest = tools.layout(area(), &measure);
        view.layout(rest, &measure);
        Self {
            view,
            tools,
            ctx: ViewContext::default(),
            measure,
        }
    }
    fn command(&mut self, command: CommandId) {
        self.tools
            .run_command(command, &mut self.view, &self.measure, &mut self.ctx);
        let rest = self.tools.layout(area(), &self.measure);
        self.view.layout(rest, &self.measure);
    }
    fn send(&mut self, kind: InputKind) {
        let event = InputEvent { time_us: 0, kind };
        self.tools
            .handle_input(&event, &mut self.view, &self.measure, &mut self.ctx);
    }
    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            self.send(InputKind::Char(c));
        }
    }
    fn key(&mut self, virtual_key: u16) {
        self.send(InputKind::KeyDown {
            virtual_key,
            modifiers: Modifiers::default(),
            repeat: false,
        });
    }
    fn settle(&mut self) {
        let start = Instant::now();
        loop {
            self.view.poll_layout(&mut self.ctx);
            self.tools
                .poll(&mut self.view, &self.measure, &mut self.ctx);
            self.ctx.requests.clear();
            if self.tools.is_quiet() && !self.view.pending_layout() {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "it did not settle"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn image(&mut self, theme: &Theme) -> qe_api::Bitmap {
        self.settle();
        let mut list = DisplayList::default();
        self.view.draw(&mut list, theme, &self.measure);
        self.tools.draw(&mut list, theme, &self.measure);
        let mut renderer = BitmapRenderer::new(WIDTH, HEIGHT).unwrap();
        renderer
            .set_text_settings(TextSettings::reference())
            .unwrap();
        for command in &list.commands {
            if let qe_api::DrawCommand::GlyphRun { font_id, .. } = command {
                renderer.register_font_face(*font_id, &qe_layout::font_face(*font_id).unwrap());
            }
        }
        renderer.render_to_bitmap(&list, WIDTH, HEIGHT).unwrap()
    }
}

fn find_bar() -> Scene {
    let mut s = Scene::new();
    s.command(CommandId::Find);
    s.typed("fox");
    s
}
fn find_replace_bar() -> Scene {
    let mut s = Scene::new();
    s.command(CommandId::FindReplace);
    s.typed("wolf");
    s.key(0x09);
    s.key(0x09);
    s.tools_focus_find();
    s.typed("fox");
    s
}
impl Scene {
    fn tools_focus_find(&mut self) {
        let bounds = self.tools.bar().find.bounds;
        let at = qe_api::Point {
            x: Dip(bounds.x.0 + 4.0),
            y: Dip(bounds.y.0 + 4.0),
        };
        self.send(InputKind::MouseDown {
            at,
            button: qe_api::MouseButton::Left,
            modifiers: Modifiers::default(),
        });
        self.send(InputKind::MouseUp {
            at,
            button: qe_api::MouseButton::Left,
            modifiers: Modifiers::default(),
        });
    }
}
fn find_options() -> Scene {
    let mut s = find_bar();
    s.settle();
    let bounds = s.tools.bar().options.bounds;
    let at = qe_api::Point {
        x: Dip(bounds.x.0 + 4.0),
        y: Dip(bounds.y.0 + 4.0),
    };
    s.send(InputKind::MouseDown {
        at,
        button: qe_api::MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    s.send(InputKind::MouseUp {
        at,
        button: qe_api::MouseButton::Left,
        modifiers: Modifiers::default(),
    });
    s
}
fn select_line() -> Scene {
    let mut s = Scene::new();
    s.command(CommandId::SelectLine);
    s.typed("10-20");
    s
}
fn select_line_error() -> Scene {
    let mut s = Scene::new();
    s.command(CommandId::SelectLine);
    s.typed("99");
    s.key(0x0d);
    s
}
fn complete_list() -> Scene {
    let mut s = Scene::new();
    let end = TEXT.len();
    s.view.select(ByteOffset(end)..ByteOffset(end), &mut s.ctx);
    s.settle();
    s.command(CommandId::Complete);
    s
}

type Build = fn() -> Scene;
const SCENES: [(&str, Build); 6] = [
    ("find-bar", find_bar),
    ("find-replace-bar", find_replace_bar),
    ("find-options", find_options),
    ("select-line", select_line),
    ("select-line-error", select_line_error),
    ("complete-list", complete_list),
];

fn image_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/images")
}

#[test]
fn saved_images_match_in_both_themes() {
    for (name, build) in SCENES {
        for (theme_name, kind) in [("light", ThemeKind::Light), ("dark", ThemeKind::Dark)] {
            let theme = Theme::new(kind);
            let image = build().image(&theme);
            let path = image_dir().join(format!("{name}-{theme_name}.png"));
            if std::env::var_os("QE_TOOLS_UPDATE_IMAGES").is_some() {
                std::fs::create_dir_all(image_dir()).unwrap();
                std::fs::write(&path, png::encode(&image)).unwrap();
            }
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let (w, h, saved) = png::decode(&bytes).unwrap();
            assert_eq!((w, h), (WIDTH, HEIGHT), "{}", path.display());
            let current = png::raw_rows(&image);
            let different = saved
                .iter()
                .zip(&current)
                .filter(|(a, b)| a.abs_diff(**b) > 3)
                .count();
            assert_eq!(
                different,
                0,
                "{} differs in {different} channels",
                path.display()
            );
        }
    }
}

/// True if the part of the picture in `r` holds this color, within a small margin.
fn has_color(image: &qe_api::Bitmap, r: Rect, color: qe_api::Color) -> bool {
    for y in r.y.0 as u32..(r.y.0 + r.height.0) as u32 {
        for x in r.x.0 as u32..(r.x.0 + r.width.0) as u32 {
            let at = (y * image.width + x) as usize * 4;
            let px = &image.bgra[at..at + 3];
            if px[0].abs_diff(color.b) < 40
                && px[1].abs_diff(color.g) < 40
                && px[2].abs_diff(color.r) < 40
            {
                return true;
            }
        }
    }
    false
}

#[test]
fn matches_paint_in_their_colors_and_the_panel_has_the_macs_size() {
    let theme = Theme::new(ThemeKind::Light);
    let mut s = find_bar();
    let image = s.image(&theme);
    let below = Rect {
        x: Dip(0.0),
        y: Dip(32.0),
        width: Dip(WIDTH as f32),
        height: Dip(HEIGHT as f32 - 32.0),
    };
    assert!(has_color(
        &image,
        below,
        qe_ui::over(theme.current_find_match, theme.text_background)
    ));
    assert!(has_color(
        &image,
        below,
        qe_ui::over(theme.find_match, theme.text_background)
    ));
    let s = select_line();
    assert_eq!(s.tools.panel().bounds.width.0, 253.0);
    assert_eq!(s.tools.panel().bounds.height.0, 62.0);
}
