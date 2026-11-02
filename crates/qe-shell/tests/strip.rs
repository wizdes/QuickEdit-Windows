use qe_api::testing::FixedTextMeasurer;
use qe_api::{
    Dip, InputEvent, InputKind, Modifiers, MouseButton, Point, Rect, Theme, ThemeKind, View,
    ViewContext,
};
use qe_shell::{CaptionHit, ShellEvent, Tab, TabStrip, caption_hit, maximized_insets};

fn measure() -> FixedTextMeasurer {
    FixedTextMeasurer {
        char_width: Dip(7.0),
        height: Dip(14.0),
        baseline: Dip(11.0),
    }
}
fn point(x: f32, y: f32) -> Point {
    Point {
        x: Dip(x),
        y: Dip(y),
    }
}
fn strip(count: usize, width: f32) -> TabStrip {
    let mut strip = TabStrip::new((0..count).map(|i| Tab::new(format!("Tab {i}"))).collect());
    strip.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(width),
            height: Dip(40.0),
        },
        &measure(),
    );
    strip
}

#[test]
fn widths_and_overflow() {
    for width in [500.0, 800.0, 1600.0] {
        for count in [1, 3, 10, 40] {
            let s = strip(count, width);
            assert_eq!(s.tab_rects().len(), count);
            assert!(s.tab_width() <= 240.0);
            assert!(s.tab_width() >= 80.0);
            assert_eq!(s.overflow(), count as f32 * 80.0 + 64.0 > width - 138.0);
            if !s.overflow() {
                assert!(s.split_rect().x.0 >= s.tab_rects().last().unwrap().x.0 + s.tab_width());
            }
        }
    }
}

#[test]
fn caption_buttons_and_edges_at_three_scales() {
    for scale in [1.0, 1.5, 2.0] {
        let w = 800.0 * scale;
        let s = strip(1, 800.0);
        assert_eq!(
            caption_hit(
                point(w - 23.0 * scale, 20.0 * scale),
                w,
                600.0 * scale,
                scale,
                false,
                &s
            ),
            CaptionHit::Close
        );
        assert_eq!(
            caption_hit(
                point(w - 69.0 * scale, 20.0 * scale),
                w,
                600.0 * scale,
                scale,
                false,
                &s
            ),
            CaptionHit::Maximize
        );
        assert_eq!(
            caption_hit(
                point(w - 115.0 * scale, 20.0 * scale),
                w,
                600.0 * scale,
                scale,
                false,
                &s
            ),
            CaptionHit::Minimize
        );
        assert_eq!(
            caption_hit(point(1.0, 1.0), w, 600.0 * scale, scale, false, &s),
            CaptionHit::ResizeTopLeft
        );
        assert_eq!(
            caption_hit(
                point(w - 1.0, 300.0 * scale),
                w,
                600.0 * scale,
                scale,
                false,
                &s
            ),
            CaptionHit::ResizeRight
        );
        let h = 600.0 * scale;
        for (at, expected) in [
            (point(1.0, 300.0 * scale), CaptionHit::ResizeLeft),
            (point(300.0 * scale, 1.0), CaptionHit::ResizeTop),
            (point(300.0 * scale, h - 1.0), CaptionHit::ResizeBottom),
            (point(w - 1.0, 1.0), CaptionHit::ResizeTopRight),
            (point(1.0, h - 1.0), CaptionHit::ResizeBottomLeft),
            (point(w - 1.0, h - 1.0), CaptionHit::ResizeBottomRight),
        ] {
            assert_eq!(caption_hit(at, w, h, scale, false, &s), expected);
        }
    }
}

#[test]
fn reorder_close_and_last_tab() {
    let mut s = strip(3, 800.0);
    s.reorder(0, 2);
    assert_eq!(s.tabs()[2].title, "Tab 0");
    s.close(2);
    s.close(1);
    s.close(0);
    assert_eq!(s.tabs().len(), 1);
}

#[test]
fn click_emits_request() {
    let mut s = strip(1, 800.0);
    let at = point(s.split_rect().x.0 + 10.0, 16.0);
    let mut ctx = ViewContext::default();
    for kind in [
        InputKind::MouseDown {
            at,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
        InputKind::MouseUp {
            at,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
    ] {
        s.handle(&InputEvent { time_us: 0, kind }, &mut ctx);
    }
    assert_eq!(s.take_events(), vec![ShellEvent::NewTab]);
    assert!(!ctx.requests.is_empty());
}

#[test]
fn max_insets() {
    assert_eq!(maximized_insets(8, 4), 12);
    assert_eq!(maximized_insets(12, 6), 18);
    assert_eq!(maximized_insets(16, 8), 24);
}

#[test]
fn maximized_row_uses_restore_glyph_and_32_dips() {
    let mut s = strip(1, 800.0);
    s.set_maximized(true);
    s.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(800.0),
            height: Dip(32.0),
        },
        &measure(),
    );
    let mut list = qe_api::DisplayList::default();
    s.draw(&mut list, &Theme::new(ThemeKind::Light), &measure());
    let text = list.to_stable_text();
    assert!(text.contains(r"\u{e923}"));
    assert_eq!(s.hit_test(point(500.0, 35.0)), CaptionHit::None);
}

#[test]
fn display_list_states() {
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        let mut s = strip(3, 800.0);
        s.tabs_mut()[0].dirty = true;
        let mut list = qe_api::DisplayList::default();
        s.draw(&mut list, &Theme::new(kind), &measure());
        let name = match kind {
            ThemeKind::Light => "light.txt",
            ThemeKind::Dark => "dark.txt",
        };
        assert_eq!(
            list.to_stable_text(),
            std::fs::read_to_string(format!("{}/tests/lists/{name}", env!("CARGO_MANIFEST_DIR")))
                .unwrap()
        );
    }
}

#[test]
fn replay_drag_and_close() {
    for file in ["reorder.txt", "close.txt"] {
        let script = std::fs::read_to_string(format!(
            "{}/tests/replays/{file}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let mut s = strip(3, 800.0);
        let mut expected = Vec::new();
        for line in script.lines() {
            let parts: Vec<_> = line.split_whitespace().collect();
            match parts.as_slice() {
                ["down" | "move" | "up", x, y] => {
                    let at = point(x.parse().unwrap(), y.parse().unwrap());
                    let kind = match parts[0] {
                        "down" => InputKind::MouseDown {
                            at,
                            button: MouseButton::Left,
                            modifiers: Modifiers::default(),
                        },
                        "move" => InputKind::MouseMove { at },
                        _ => InputKind::MouseUp {
                            at,
                            button: MouseButton::Left,
                            modifiers: Modifiers::default(),
                        },
                    };
                    s.handle(
                        &InputEvent { time_us: 0, kind },
                        &mut ViewContext::default(),
                    );
                }
                ["layout"] => s.layout(
                    Rect {
                        x: Dip(0.0),
                        y: Dip(0.0),
                        width: Dip(800.0),
                        height: Dip(40.0),
                    },
                    &measure(),
                ),
                ["expect", rest @ ..] => expected.push(rest.join(" ")),
                _ => panic!("bad replay line: {line}"),
            }
        }
        let actual: Vec<_> = s
            .take_events()
            .iter()
            .map(|event| format!("{event:?}"))
            .collect();
        assert_eq!(actual, expected, "{file}");
    }
}

#[test]
fn widget_tree_has_caption_and_scroll_buttons() {
    let regular = strip(3, 800.0).widgets().unwrap();
    assert_eq!(
        regular
            .children
            .iter()
            .filter(|w| w.role == qe_api::WidgetRole::Button)
            .count(),
        5
    );
    assert!(
        regular
            .children
            .iter()
            .filter(|w| w.role == qe_api::WidgetRole::Tab)
            .all(|tab| {
                tab.children.iter().any(|child| {
                    child.role == qe_api::WidgetRole::Button && child.name == "Close tab"
                })
            })
    );
    let overflow = strip(40, 500.0).widgets().unwrap();
    assert_eq!(
        overflow
            .children
            .iter()
            .filter(|w| w.role == qe_api::WidgetRole::Button)
            .count(),
        7
    );
}

#[test]
fn keyboard_focus_and_new_tab() {
    let mut s = strip(2, 800.0);
    let mut ctx = ViewContext::default();
    for key in [0x09, 0x09, 0x0d] {
        s.handle(
            &InputEvent {
                time_us: 0,
                kind: InputKind::KeyDown {
                    virtual_key: key,
                    modifiers: Modifiers::default(),
                    repeat: false,
                },
            },
            &mut ctx,
        );
    }
    assert_eq!(s.take_events(), vec![ShellEvent::SelectTab(1)]);
    assert!(
        ctx.requests
            .iter()
            .any(|request| matches!(request, qe_api::ViewRequest::SetFocus(_)))
    );
}

#[test]
fn overflow_buttons_scroll_one_tab() {
    let mut s = strip(40, 500.0);
    let before = s.tab_rects()[0].x.0;
    let at = point(s.split_rect().x.0 - 10.0, 16.0);
    for kind in [
        InputKind::MouseDown {
            at,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
        InputKind::MouseUp {
            at,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
    ] {
        s.handle(
            &InputEvent { time_us: 0, kind },
            &mut ViewContext::default(),
        );
    }
    s.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(500.0),
            height: Dip(40.0),
        },
        &measure(),
    );
    assert_eq!(s.tab_rects()[0].x.0, before - s.tab_width());
}

#[test]
fn display_list_visual_states() {
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        for state in [
            "hover",
            "pressed",
            "inactive",
            "focus",
            "contrast",
            "maximized",
        ] {
            let mut s = strip(2, 800.0);
            let at = point(s.split_rect().x.0 + 10.0, 16.0);
            let input = match state {
                "hover" => Some(InputKind::MouseMove { at }),
                "pressed" => Some(InputKind::MouseDown {
                    at,
                    button: MouseButton::Left,
                    modifiers: Modifiers::default(),
                }),
                _ => None,
            };
            if let Some(kind) = input {
                s.handle(
                    &InputEvent { time_us: 0, kind },
                    &mut ViewContext::default(),
                );
            }
            match state {
                "inactive" => s.set_window_active(false),
                "focus" => s.set_focused(Some(CaptionHit::NewTab)),
                "contrast" => s.set_high_contrast(true),
                "maximized" => {
                    s.set_maximized(true);
                    s.layout(
                        Rect {
                            x: Dip(0.0),
                            y: Dip(0.0),
                            width: Dip(800.0),
                            height: Dip(32.0),
                        },
                        &measure(),
                    );
                }
                _ => {}
            }
            let mut list = qe_api::DisplayList::default();
            s.draw(&mut list, &Theme::new(kind), &measure());
            let theme = match kind {
                ThemeKind::Light => "light",
                ThemeKind::Dark => "dark",
            };
            let path = format!(
                "{}/tests/lists/{theme}-{state}.txt",
                env!("CARGO_MANIFEST_DIR")
            );
            assert_eq!(
                list.to_stable_text(),
                std::fs::read_to_string(&path).unwrap(),
                "{path}"
            );
        }
    }
}

#[test]
fn press_changes_split_button_without_prior_move() {
    let mut s = strip(1, 800.0);
    let mut before = qe_api::DisplayList::default();
    s.draw(&mut before, &Theme::new(ThemeKind::Light), &measure());
    let at = point(s.split_rect().x.0 + 10.0, 16.0);
    s.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            },
        },
        &mut ViewContext::default(),
    );
    let mut after = qe_api::DisplayList::default();
    s.draw(&mut after, &Theme::new(ThemeKind::Light), &measure());
    assert_ne!(before, after);
}

#[test]
fn dirty_dot_uses_text_font() {
    let mut s = strip(1, 800.0);
    s.tabs_mut()[0].dirty = true;
    let mut list = qe_api::DisplayList::default();
    s.draw(&mut list, &Theme::new(ThemeKind::Light), &measure());
    assert!(list.commands.iter().any(|command| matches!(command, qe_api::DrawCommand::TextRun { text, font, .. } if text == "•" && font == "Segoe UI")));
}

#[test]
fn caption_icons_request_ten_pixel_font() {
    let s = strip(1, 800.0);
    let mut list = qe_api::DisplayList::default();
    s.draw(&mut list, &Theme::new(ThemeKind::Light), &measure());
    assert_eq!(list.commands.iter().filter(|command| matches!(command, qe_api::DrawCommand::TextRun { font, .. } if font == "Segoe Fluent Icons;10px")).count(), 3);
}
