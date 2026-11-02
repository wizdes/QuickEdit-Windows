//! The real renderer paints the bar at the three Windows scaling values.
mod support;
use qe_api::{
    Dip, DisplayList, DrawCommand, InputEvent, InputKind, Modifiers, Rect, Renderer, Theme,
    ThemeKind, View, ViewContext, WidgetId,
};
use qe_render::{BitmapRenderer, TextSettings};
use qe_ui::{MenuBar, SystemPalette};
use std::path::PathBuf;
use support::{RealMeasure, png};

#[test]
fn saved_bar_images_match_at_96_144_and_192_dpi() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/images");
    for (name, theme) in [
        ("light", Theme::new(ThemeKind::Light)),
        ("dark", Theme::new(ThemeKind::Dark)),
        ("contrast", SystemPalette::HIGH_CONTRAST_BLACK.theme()),
    ] {
        for dpi in [96, 144, 192] {
            let mut bar = MenuBar::new(WidgetId(700));
            bar.layout(
                Rect {
                    x: Dip(0.0),
                    y: Dip(0.0),
                    width: Dip(440.0),
                    height: theme.menu_bar_height,
                },
                &RealMeasure,
            );
            bar.handle(
                &InputEvent {
                    time_us: 0,
                    kind: InputKind::KeyDown {
                        virtual_key: 0x79,
                        modifiers: Modifiers::default(),
                        repeat: false,
                    },
                },
                &mut ViewContext::default(),
            );
            let mut list = DisplayList::default();
            list.push(DrawCommand::FillRect {
                rect: Rect {
                    x: Dip(0.0),
                    y: Dip(0.0),
                    width: Dip(440.0),
                    height: Dip(80.0),
                },
                color: theme.text_background,
            });
            bar.draw(&mut list, &theme, &RealMeasure);
            let (w, h) = (440 * dpi / 96, 80 * dpi / 96);
            let mut renderer = BitmapRenderer::new(w, h).unwrap();
            renderer
                .set_text_settings(TextSettings::reference())
                .unwrap();
            renderer.set_dpi(dpi).unwrap();
            let image = renderer.render_to_bitmap(&list, w, h).unwrap();
            let path = dir.join(format!("menu-bar-{name}-{dpi}.png"));
            if std::env::var_os("QE_UI_UPDATE_IMAGES").is_some() {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(&path, png::encode(&image)).unwrap();
            }
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let (saved_w, saved_h, saved) = png::decode(&bytes).unwrap();
            assert_eq!((saved_w, saved_h), (w, h));
            let current = png::raw_rows(&image);
            let different = saved
                .iter()
                .zip(current.iter())
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
