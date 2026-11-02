use qe_api::CommandId;
use qe_api::{
    Dip, InputEvent, InputKind, Modifiers, Rect, TextMeasurer, TextMetrics, View, ViewContext,
    WidgetId,
};
use qe_ui::{MenuBar, caret_menu, menu_bar_groups};
use std::collections::BTreeSet;

fn commands(model: &qe_ui::MenuModel, out: &mut BTreeSet<String>) {
    for item in &model.items {
        if let Some(command) = item.command {
            out.insert(format!("{command:?}"));
        }
        if let Some(submenu) = &item.submenu {
            commands(submenu, out);
        }
    }
}

struct Measure;
impl TextMeasurer for Measure {
    fn measure(&self, text: &str, _: &str) -> TextMetrics {
        TextMetrics {
            width: Dip(text.len() as f32 * 7.0),
            height: Dip(16.0),
            baseline: Dip(12.0),
        }
    }
}

fn key(bar: &mut MenuBar, vk: u16, alt: bool) {
    bar.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::KeyDown {
                virtual_key: vk,
                modifiers: Modifiers {
                    alt,
                    ..Default::default()
                },
                repeat: false,
            },
        },
        &mut ViewContext::default(),
    );
}

#[test]
fn keyboard_focus_wraps_and_opens_mnemonic() {
    let mut bar = MenuBar::new(WidgetId(100));
    bar.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(40.0),
            width: Dip(400.0),
            height: Dip(32.0),
        },
        &Measure,
    );
    key(&mut bar, 0x79, false); // F10
    assert_eq!(bar.focused(), Some(0));
    key(&mut bar, 0x25, false); // Left wraps
    assert_eq!(bar.focused(), Some(4));
    key(&mut bar, 0x28, false); // Down opens
    assert_eq!(bar.take_open_request().map(|v| v.0), Some(4));
    key(&mut bar, 0x1b, false); // First Esc closes popup
    assert_eq!(bar.focused(), Some(4));
    key(&mut bar, 0x1b, false); // Second Esc returns to text
    assert_eq!(bar.focused(), None);
    key(&mut bar, b'O' as u16, true);
    assert_eq!(bar.take_open_request().map(|v| v.0), Some(2));
    key(&mut bar, b'F' as u16, true);
    assert_eq!(bar.take_open_request().map(|v| v.0), Some(0));
}

#[test]
fn blur_closes_bar_without_a_command() {
    let mut bar = MenuBar::new(WidgetId(100));
    bar.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(40.0),
            width: Dip(400.0),
            height: Dip(32.0),
        },
        &Measure,
    );
    key(&mut bar, b'V' as u16, true);
    assert_eq!(bar.take_open_request().map(|v| v.0), Some(3));
    bar.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::Blur,
        },
        &mut ViewContext::default(),
    );
    assert_eq!(bar.focused(), None);
    assert_eq!(bar.open(), None);
}

#[test]
fn mouse_opens_switches_and_closes_only_on_the_labels() {
    let mut bar = MenuBar::new(WidgetId(100));
    bar.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(40.0),
            width: Dip(400.0),
            height: Dip(32.0),
        },
        &Measure,
    );
    let send = |bar: &mut MenuBar, kind| {
        bar.handle(
            &InputEvent { time_us: 0, kind },
            &mut ViewContext::default(),
        )
    };
    let file = qe_api::Point {
        x: Dip(18.0),
        y: Dip(50.0),
    };
    let edit = qe_api::Point {
        x: Dip(64.0),
        y: Dip(50.0),
    };
    send(
        &mut bar,
        InputKind::MouseDown {
            at: file,
            button: qe_api::MouseButton::Left,
            modifiers: Modifiers::default(),
        },
    );
    assert_eq!(bar.take_open_request().map(|v| v.0), Some(0));
    send(&mut bar, InputKind::MouseMove { at: edit });
    assert_eq!(bar.take_open_request().map(|v| v.0), Some(1));
    send(
        &mut bar,
        InputKind::MouseDown {
            at: edit,
            button: qe_api::MouseButton::Left,
            modifiers: Modifiers::default(),
        },
    );
    assert_eq!(bar.open(), None);
    assert_eq!(bar.focused(), None);
}

#[test]
fn altgr_never_opens_a_menu() {
    let mut bar = MenuBar::new(WidgetId(100));
    bar.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(40.0),
            width: Dip(400.0),
            height: Dip(32.0),
        },
        &Measure,
    );
    bar.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::KeyDown {
                virtual_key: b'F' as u16,
                modifiers: Modifiers {
                    ctrl: true,
                    alt: true,
                    ..Default::default()
                },
                repeat: false,
            },
        },
        &mut ViewContext::default(),
    );
    assert_eq!(bar.take_open_request(), None);
    bar.handle(
        &InputEvent {
            time_us: 1,
            kind: InputKind::KeyDown {
                virtual_key: 0xa5,
                modifiers: Modifiers {
                    ctrl: true,
                    alt: true,
                    ..Default::default()
                },
                repeat: false,
            },
        },
        &mut ViewContext::default(),
    );
    bar.handle(
        &InputEvent {
            time_us: 2,
            kind: InputKind::KeyUp {
                virtual_key: 0xa5,
                modifiers: Modifiers {
                    alt: true,
                    ..Default::default()
                },
            },
        },
        &mut ViewContext::default(),
    );
    assert_eq!(bar.focused(), None);
}

#[test]
fn every_mouse_button_stays_out_of_the_text_area() {
    let mut bar = MenuBar::new(WidgetId(100));
    bar.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(40.0),
            width: Dip(400.0),
            height: Dip(32.0),
        },
        &Measure,
    );
    for button in [qe_api::MouseButton::Right, qe_api::MouseButton::Middle] {
        let at = qe_api::Point {
            x: Dip(300.0),
            y: Dip(50.0),
        };
        let got = bar.handle(
            &InputEvent {
                time_us: 0,
                kind: InputKind::MouseDown {
                    at,
                    button,
                    modifiers: Modifiers::default(),
                },
            },
            &mut ViewContext::default(),
        );
        assert_eq!(got, qe_api::Handled::Yes);
    }
}

#[test]
fn focused_bar_consumes_letters_before_the_editor_sees_them() {
    let mut bar = MenuBar::new(WidgetId(100));
    bar.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(40.0),
            width: Dip(400.0),
            height: Dip(32.0),
        },
        &Measure,
    );
    key(&mut bar, 0x79, false);
    let got = bar.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::KeyDown {
                virtual_key: b'E' as u16,
                modifiers: Modifiers::default(),
                repeat: false,
            },
        },
        &mut ViewContext::default(),
    );
    assert_eq!(got, qe_api::Handled::Yes);
    assert_eq!(bar.take_open_request().map(|v| v.0), Some(1));
    assert_eq!(
        bar.handle(
            &InputEvent {
                time_us: 0,
                kind: InputKind::Char('e')
            },
            &mut ViewContext::default()
        ),
        qe_api::Handled::Yes
    );
}

#[test]
fn hover_invalidates_only_the_two_changed_labels() {
    let mut bar = MenuBar::new(WidgetId(100));
    bar.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(40.0),
            width: Dip(400.0),
            height: Dip(32.0),
        },
        &Measure,
    );
    let file = qe_api::Point {
        x: Dip(18.0),
        y: Dip(50.0),
    };
    let edit = qe_api::Point {
        x: Dip(64.0),
        y: Dip(50.0),
    };
    bar.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::MouseMove { at: file },
        },
        &mut ViewContext::default(),
    );
    let mut ctx = ViewContext::default();
    bar.handle(
        &InputEvent {
            time_us: 1,
            kind: InputKind::MouseMove { at: edit },
        },
        &mut ctx,
    );
    assert_eq!(
        ctx.requests,
        [
            qe_api::ViewRequest::Invalidate(bar.label_rect(0)),
            qe_api::ViewRequest::Invalidate(bar.label_rect(1))
        ]
    );
}

#[test]
fn every_alt_mnemonic_and_right_wrap_selects_the_matching_group() {
    let mut bar = MenuBar::new(WidgetId(100));
    bar.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(40.0),
            width: Dip(400.0),
            height: Dip(32.0),
        },
        &Measure,
    );
    for (index, letter) in [(0, b'F'), (1, b'E'), (2, b'O'), (3, b'V'), (4, b'W')] {
        key(&mut bar, letter as u16, true);
        assert_eq!(bar.take_open_request().map(|v| v.0), Some(index));
        assert_eq!(bar.focused(), Some(index));
    }
    key(&mut bar, 0x27, false);
    assert_eq!(bar.take_open_request().map(|v| v.0), Some(0));
    key(&mut bar, 0x1b, false);
    assert_eq!(bar.open(), None);
    assert_eq!(bar.focused(), Some(0));
    key(&mut bar, 0x0d, false);
    assert_eq!(bar.take_open_request().map(|v| v.0), Some(0));
}

#[test]
fn alt_tap_focuses_then_returns_to_text() {
    let mut bar = MenuBar::new(WidgetId(100));
    bar.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(40.0),
            width: Dip(400.0),
            height: Dip(32.0),
        },
        &Measure,
    );
    for expected in [Some(0), None] {
        bar.handle(
            &InputEvent {
                time_us: 0,
                kind: InputKind::KeyDown {
                    virtual_key: 0x12,
                    modifiers: Modifiers {
                        alt: true,
                        ..Default::default()
                    },
                    repeat: false,
                },
            },
            &mut ViewContext::default(),
        );
        bar.handle(
            &InputEvent {
                time_us: 1,
                kind: InputKind::KeyUp {
                    virtual_key: 0x12,
                    modifiers: Modifiers {
                        alt: true,
                        ..Default::default()
                    },
                },
            },
            &mut ViewContext::default(),
        );
        assert_eq!(bar.focused(), expected);
    }
}

#[test]
fn groups_and_caret_keep_every_command_once() {
    let groups = menu_bar_groups();
    assert_eq!(
        groups.iter().map(|g| g.label.as_str()).collect::<Vec<_>>(),
        ["File", "Edit", "Format", "View", "Window"]
    );
    let caret = caret_menu();
    assert_eq!(
        caret
            .items
            .iter()
            .filter_map(|i| i.command)
            .collect::<Vec<_>>(),
        [CommandId::About, CommandId::Settings, CommandId::Quit]
    );
    let mut actual = BTreeSet::new();
    commands(&qe_ui::MenuModel { items: groups }, &mut actual);
    commands(&caret, &mut actual);
    let mut baseline = BTreeSet::new();
    commands(&qe_ui::legacy_caret_menu(), &mut baseline);
    assert_eq!(actual, baseline);
}
