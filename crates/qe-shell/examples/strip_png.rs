//! Writes pictures of the top row through the real renderer. No window opens.
//! Usage: strip_png DIRECTORY
use qe_api::{Dip, InputEvent, InputKind, Modifiers, MouseButton, Point, Theme, ThemeKind, View};
use qe_shell::{CaptionHit, ContrastColors, Tab, TabStrip};
use std::path::Path;

#[path = "../tests/support/mod.rs"]
mod support;
use support::*;

fn strip(state: &str, contrast: bool, width: f32) -> TabStrip {
    let mut s = TabStrip::new(vec![
        Tab::new("Untitled"),
        Tab::new("Example.rs"),
        Tab::new("A longer tab name"),
    ]);
    s.tabs_mut()[1].dirty = true;
    if contrast {
        s.set_high_contrast(Some(ContrastColors {
            window: qe_api::Color::rgb(0, 0, 0),
            window_text: qe_api::Color::rgb(255, 255, 255),
            highlight: qe_api::Color::rgb(26, 235, 255),
            highlight_text: qe_api::Color::rgb(0, 0, 0),
            gray_text: qe_api::Color::rgb(63, 242, 63),
        }));
    }
    if state == "maximized" {
        s.set_maximized(true);
    }
    s.layout(rect(0.0, 0.0, width, s.row_height()), &RealMeasurer);
    let press = |s: &mut TabStrip, x: f32, y: f32, down: bool| {
        let at = Point {
            x: Dip(x),
            y: Dip(y),
        };
        let mut ctx = qe_api::ViewContext::default();
        s.handle(
            &InputEvent {
                time_us: 0,
                kind: if down {
                    InputKind::MouseDown {
                        at,
                        button: MouseButton::Left,
                        modifiers: Modifiers::default(),
                    }
                } else {
                    InputKind::MouseMove { at }
                },
            },
            &mut ctx,
        );
    };
    let plus = s.plus_rect();
    match state {
        "hover-plus" => press(&mut s, plus.x.0 + 10.0, plus.y.0 + 10.0, false),
        "pressed-plus" => press(&mut s, plus.x.0 + 10.0, plus.y.0 + 10.0, true),
        "hover-close" => press(&mut s, width - 10.0, 20.0, false),
        "pressed-close" => press(&mut s, width - 10.0, 20.0, true),
        "hover-max" => press(&mut s, width - 60.0, 20.0, false),
        "hover-tab" => press(&mut s, 300.0, 24.0, false),
        "hover-tab-close" => {
            let b = s.tab_rects()[2];
            press(&mut s, b.x.0 + b.width.0 - 10.0, 24.0, false);
        }
        "focus-tab" => s.set_focused(Some(CaptionHit::Tab(2))),
        "focus-active" => s.set_focused(Some(CaptionHit::Tab(0))),
        "focus-plus" => s.set_focused(Some(CaptionHit::NewTab)),
        "inactive" => s.set_window_active(false),
        _ => {}
    }
    s
}
fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let dir = args.get(1).ok_or("usage: strip_png DIRECTORY")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for (name, kind, contrast) in [
        ("light", ThemeKind::Light, false),
        ("dark", ThemeKind::Dark, false),
        ("contrast", ThemeKind::Dark, true),
    ] {
        for state in [
            "rest",
            "maximized",
            "hover-plus",
            "pressed-plus",
            "hover-close",
            "pressed-close",
            "hover-max",
            "hover-tab",
            "hover-tab-close",
            "focus-tab",
            "focus-active",
            "focus-plus",
            "inactive",
        ] {
            for dpi in [96, 144, 192] {
                let mut theme = Theme::new(kind);
                if contrast {
                    theme.text_background = qe_api::Color::rgb(0, 0, 0);
                    theme.text = qe_api::Color::rgb(255, 255, 255);
                    theme.dim_text = theme.text;
                    theme.border = theme.text;
                }
                let s = strip(state, contrast, 900.0);
                let image = render_strip(&s, &theme, dpi, 900.0, 80.0);
                let path = Path::new(dir).join(format!("{name}-{state}-{dpi}.png"));
                std::fs::write(&path, encode_png(&image)).map_err(|e| e.to_string())?;
            }
        }
    }
    println!("wrote pictures to {dir}");
    Ok(())
}
