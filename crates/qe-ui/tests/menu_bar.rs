use qe_api::CommandId;
use qe_api::{
    Dip, InputEvent, InputKind, Modifiers, Rect, TextMeasurer, TextMetrics, View, ViewContext,
    WidgetId,
};
use qe_ui::{MenuBar, menu_bar_groups};

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
fn the_hover_highlight_goes_when_the_pointer_leaves() {
    let mut bar = MenuBar::new(WidgetId(100));
    bar.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(40.0),
            width: Dip(500.0),
            height: Dip(32.0),
        },
        &Measure,
    );
    let label = bar.label_rect(1);
    bar.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::MouseMove {
                at: qe_api::Point {
                    x: Dip(label.x.0 + 2.0),
                    y: Dip(label.y.0 + 2.0),
                },
            },
        },
        &mut ViewContext::default(),
    );
    let mut ctx = ViewContext::default();
    bar.pointer_left(&mut ctx);
    assert_eq!(ctx.requests, vec![qe_api::ViewRequest::Invalidate(label)]);
    let mut again = ViewContext::default();
    bar.pointer_left(&mut again);
    assert!(again.requests.is_empty(), "no highlight is left to repaint");
}

fn laid_out_bar() -> MenuBar {
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
    bar
}
fn send_kind(bar: &mut MenuBar, kind: InputKind) -> qe_api::Handled {
    bar.handle(
        &InputEvent { time_us: 0, kind },
        &mut ViewContext::default(),
    )
}
fn plain_key(virtual_key: u16, modifiers: Modifiers, repeat: bool) -> InputKind {
    InputKind::KeyDown {
        virtual_key,
        modifiers,
        repeat,
    }
}
const ALT_ONLY: Modifiers = Modifiers {
    alt: true,
    ctrl: false,
    shift: false,
    win: false,
};
fn tap_alt(bar: &mut MenuBar) {
    send_kind(bar, plain_key(0x12, ALT_ONLY, false));
    send_kind(
        bar,
        InputKind::KeyUp {
            virtual_key: 0x12,
            modifiers: ALT_ONLY,
        },
    );
}

#[test]
fn every_size_comes_from_the_theme() {
    let theme = qe_api::Theme::new(qe_api::ThemeKind::Light);
    let bar = laid_out_bar();
    let first = bar.label_rect(0);
    assert_eq!(first.x, theme.menu_bar_inset);
    assert_eq!(
        first.width.0,
        "File".len() as f32 * 7.0 + theme.menu_bar_label_padding.0 * 2.0
    );
    assert_eq!(bar.label_rect(1).x.0, first.x.0 + first.width.0);
    let mut focused = laid_out_bar();
    tap_alt(&mut focused);
    let mut list = qe_api::DisplayList::default();
    focused.draw(&mut list, &theme, &Measure);
    let text_x = list.commands.iter().find_map(|c| match c {
        qe_api::DrawCommand::TextRun { text, at, .. } if &**text == "File" => Some(at.x.0),
        _ => None,
    });
    assert_eq!(text_x, Some(first.x.0 + theme.menu_bar_label_padding.0));
    let (width, line_y) = list
        .commands
        .iter()
        .find_map(|c| match c {
            qe_api::DrawCommand::Line { width, from, .. } => Some((width.0, from.y.0)),
            _ => None,
        })
        .expect("the focused label underlines its letter");
    assert_eq!(width, theme.menu_bar_underline_width.0);
    let text_y = (32.0 - 16.0) / 2.0 + 40.0 + 12.0;
    assert_eq!(line_y, text_y + theme.menu_bar_underline_offset.0);
}

#[test]
fn the_bar_is_the_color_of_the_active_tab() {
    for theme in [
        qe_api::Theme::new(qe_api::ThemeKind::Light),
        qe_api::Theme::new(qe_api::ThemeKind::Dark),
    ] {
        let mut list = qe_api::DisplayList::default();
        laid_out_bar().draw(&mut list, &theme, &Measure);
        match &list.commands[0] {
            qe_api::DrawCommand::FillRect { color, .. } => {
                assert_eq!(*color, theme.text_background)
            }
            other => panic!("the bar starts with {other:?}"),
        }
    }
}

#[test]
fn only_one_widget_says_focused_and_the_role_is_a_menu_bar() {
    let mut bar = laid_out_bar();
    assert_eq!(bar.widgets().role, qe_api::WidgetRole::MenuBar);
    tap_alt(&mut bar);
    let tree = bar.widgets();
    assert!(!tree.states.focused, "the bar itself never says focused");
    assert_eq!(
        tree.children
            .iter()
            .filter(|label| label.states.focused)
            .count(),
        1
    );
    // Once a menu opens, its row has the focus, and the label says only that it is open.
    send_kind(&mut bar, plain_key(0x28, Modifiers::default(), false));
    let tree = bar.widgets();
    assert_eq!(bar.focused(), Some(0));
    assert!(tree.children.iter().all(|label| !label.states.focused));
    assert!(tree.children[0].states.expanded);
}

#[test]
fn shift_f10_and_alt_f10_never_take_the_focus() {
    let shift = Modifiers {
        shift: true,
        ..Default::default()
    };
    for modifiers in [shift, ALT_ONLY] {
        let mut bar = laid_out_bar();
        let got = send_kind(&mut bar, plain_key(0x79, modifiers, false));
        assert_eq!(got, qe_api::Handled::No);
        assert_eq!(bar.focused(), None);
    }
}

#[test]
fn a_held_f10_or_enter_changes_nothing() {
    let mut bar = laid_out_bar();
    send_kind(&mut bar, plain_key(0x79, Modifiers::default(), false));
    send_kind(&mut bar, plain_key(0x79, Modifiers::default(), true));
    assert_eq!(bar.focused(), Some(0));
    send_kind(&mut bar, plain_key(0x0d, Modifiers::default(), true));
    assert_eq!(bar.take_open_request(), None);
    send_kind(&mut bar, plain_key(0x0d, Modifiers::default(), false));
    assert_eq!(bar.take_open_request().map(|v| v.0), Some(0));
}

#[test]
fn a_ctrl_chord_lets_go_and_a_lone_ctrl_does_not() {
    let ctrl = Modifiers {
        ctrl: true,
        ..Default::default()
    };
    let altgr = Modifiers {
        ctrl: true,
        alt: true,
        ..Default::default()
    };
    let mut bar = laid_out_bar();
    tap_alt(&mut bar);
    assert_eq!(
        send_kind(&mut bar, plain_key(0x11, ctrl, false)),
        qe_api::Handled::Yes
    );
    assert_eq!(bar.focused(), Some(0));
    assert_eq!(
        send_kind(&mut bar, plain_key(b'X'.into(), ctrl, false)),
        qe_api::Handled::No
    );
    assert_eq!(bar.focused(), None);
    tap_alt(&mut bar);
    assert_eq!(
        send_kind(&mut bar, plain_key(b'E'.into(), altgr, false)),
        qe_api::Handled::No
    );
    assert_eq!(bar.focused(), None);
    assert_eq!(bar.take_open_request(), None);
}

#[test]
fn any_press_or_wheel_cancels_a_held_alt() {
    let outside = qe_api::Point {
        x: Dip(300.0),
        y: Dip(300.0),
    };
    for kind in [
        InputKind::MouseDown {
            at: outside,
            button: qe_api::MouseButton::Left,
            modifiers: ALT_ONLY,
        },
        InputKind::MouseDown {
            at: outside,
            button: qe_api::MouseButton::Right,
            modifiers: ALT_ONLY,
        },
        InputKind::Wheel {
            at: outside,
            delta_x: 0.0,
            delta_y: 1.0,
            modifiers: ALT_ONLY,
        },
    ] {
        let mut bar = laid_out_bar();
        send_kind(&mut bar, plain_key(0x12, ALT_ONLY, false));
        send_kind(&mut bar, kind);
        send_kind(
            &mut bar,
            InputKind::KeyUp {
                virtual_key: 0x12,
                modifiers: ALT_ONLY,
            },
        );
        assert_eq!(bar.focused(), None);
    }
}

#[test]
fn a_right_or_middle_press_outside_lets_go_of_the_focus() {
    for button in [qe_api::MouseButton::Right, qe_api::MouseButton::Middle] {
        let mut bar = laid_out_bar();
        tap_alt(&mut bar);
        let got = send_kind(
            &mut bar,
            InputKind::MouseDown {
                at: qe_api::Point {
                    x: Dip(300.0),
                    y: Dip(300.0),
                },
                button,
                modifiers: Modifiers::default(),
            },
        );
        assert_eq!(got, qe_api::Handled::No, "the press goes on to the text");
        assert_eq!(bar.focused(), None);
    }
}

fn submenu_view() -> qe_ui::MenuView {
    let child = qe_ui::MenuModel {
        items: vec![qe_ui::MenuItem::command("Child", CommandId::Close)],
    };
    let mut view = qe_ui::MenuView::new(
        WidgetId(300),
        "Test",
        qe_ui::MenuModel {
            items: vec![
                qe_ui::MenuItem::submenu("Parent", child),
                qe_ui::MenuItem::command("Plain", CommandId::Print),
            ],
        },
    );
    view.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(200.0),
            height: Dip(120.0),
        },
        &Measure,
    );
    view
}
fn view_key(view: &mut qe_ui::MenuView, vk: u16, repeat: bool) -> Vec<qe_api::ViewRequest> {
    let mut ctx = ViewContext::default();
    view.handle(
        &InputEvent {
            time_us: 1,
            kind: plain_key(vk, Modifiers::default(), repeat),
        },
        &mut ctx,
    );
    ctx.requests
}

#[test]
fn a_key_that_moves_off_a_hovered_row_ends_its_submenu_wait() {
    let t = qe_ui::UiTokens::sizes();
    let mut view = submenu_view();
    view.handle(
        &InputEvent {
            time_us: 0,
            kind: InputKind::MouseMove {
                at: qe_api::Point {
                    x: Dip(20.0),
                    y: Dip(t.inset.0 + t.menu_row.0 / 2.0),
                },
            },
        },
        &mut ViewContext::default(),
    );
    assert!(view.waiting_for_submenu());
    view_key(&mut view, 0x28, false);
    assert!(!view.waiting_for_submenu(), "the timer has no more work");
    view_key(&mut view, 0x26, false);
    assert_eq!(view.tick(u64::MAX), None, "nothing opens by itself");
    assert_eq!(view.levels().len(), 1);
}

#[test]
fn a_repeated_enter_or_space_in_a_menu_does_nothing() {
    for vk in [0x0du16, 0x20] {
        let mut view = submenu_view();
        view.open_keyboard();
        view_key(&mut view, 0x28, false);
        let requests = view_key(&mut view, vk, true);
        assert!(!view.closed);
        assert!(
            !requests
                .iter()
                .any(|r| matches!(r, qe_api::ViewRequest::Run(_)))
        );
        assert!(
            view_key(&mut view, vk, false)
                .iter()
                .any(|r| matches!(r, qe_api::ViewRequest::Run(_)))
        );
    }
}

#[test]
fn a_row_with_a_submenu_says_it_has_a_popup() {
    let tree = View::widgets(&submenu_view()).unwrap();
    assert!(tree.children[0].states.has_popup);
    assert!(!tree.children[1].states.has_popup);
}

// ---- Settings lives in File. About and Quit are gone.

fn labels_and_commands(model: &qe_ui::MenuModel, out: &mut Vec<(String, Option<CommandId>)>) {
    for item in &model.items {
        out.push((item.label.clone(), item.command));
        if let Some(submenu) = &item.submenu {
            labels_and_commands(submenu, out);
        }
    }
}

#[test]
fn file_ends_with_print_a_separator_and_settings() {
    let file = menu_bar_groups().remove(0).submenu.unwrap();
    let last: Vec<_> = file.items[file.items.len() - 4..]
        .iter()
        .map(|item| (item.label.as_str(), item.command))
        .collect();
    assert_eq!(
        last,
        [
            ("Export as PDF…", Some(CommandId::ExportPdf)),
            ("Print…", Some(CommandId::Print)),
            ("", None),
            ("Settings…", Some(CommandId::Settings)),
        ]
    );
}

#[test]
fn no_about_or_quit_in_the_menus_or_the_command_table() {
    let mut rows = Vec::new();
    labels_and_commands(
        &qe_ui::MenuModel {
            items: menu_bar_groups(),
        },
        &mut rows,
    );
    for (label, _) in &rows {
        assert!(!label.contains("About"), "{label}");
        assert!(!label.contains("Quit"), "{label}");
        assert!(!label.contains("Exit"), "{label}");
    }
    for command in CommandId::ALL {
        let name = format!("{command:?}");
        assert!(name != "About" && name != "Quit", "{name}");
        assert!(
            !command.menu_path().starts_with("QuickEdit/"),
            "{}",
            command.menu_path()
        );
    }
    // Alt+F4 belongs to Windows now: no command asks for it.
    let alt_f4 = qe_api::KeyBinding::new(0x73, false, true, false);
    assert!(
        !CommandId::ALL
            .iter()
            .any(|command| command.default_key() == Some(alt_f4))
    );
}

#[test]
fn ctrl_comma_still_names_settings_and_its_row_shows_it() {
    let comma = qe_api::KeyBinding::new(0xBC, true, false, false);
    assert_eq!(CommandId::Settings.default_key(), Some(comma));
    assert_eq!(CommandId::Settings.menu_path(), "File/Settings");
    let bound: Vec<_> = CommandId::ALL
        .iter()
        .filter(|command| command.default_key() == Some(comma))
        .collect();
    assert_eq!(bound, [&CommandId::Settings]);
    let file = menu_bar_groups().remove(0).submenu.unwrap();
    let row = file.items.last().unwrap();
    assert_eq!(row.command, Some(CommandId::Settings));
    assert_eq!(row.key_text, "Ctrl+,");
}

/// Each command has one row, in the group its menu path names. No other menu is left to
/// reach a command from, so a command missing here would be out of reach.
#[test]
fn every_command_is_in_the_group_its_path_names_and_only_once() {
    for command in CommandId::ALL {
        let top = command.menu_path().split('/').next().unwrap();
        let mut found = 0;
        for group in menu_bar_groups() {
            let mut rows = Vec::new();
            labels_and_commands(group.submenu.as_ref().unwrap(), &mut rows);
            let here = rows.iter().filter(|(_, c)| *c == Some(*command)).count();
            if here > 0 {
                assert_eq!(group.label, top, "{command:?}");
            }
            found += here;
        }
        assert_eq!(found, 1, "{command:?}");
    }
}
