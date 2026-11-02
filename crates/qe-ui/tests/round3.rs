//! Regression tests for the third M7 review. Each test names its item. No window shows.
use qe_api::{
    CommandId, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, KeyBinding,
    Modifiers, MouseButton, Point, Rect, TextEncoding, TextMeasurer, TextMetrics, Theme, ThemeKind,
    View, ViewContext, ViewRequest, WidgetId, WidgetRole,
};
use qe_ui::*;

struct Measure;
impl TextMeasurer for Measure {
    fn measure(&self, s: &str, _: &str) -> TextMetrics {
        TextMetrics {
            width: Dip(s.chars().count() as f32 * 7.0),
            height: Dip(13.0),
            baseline: Dip(11.0),
        }
    }
}
fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(w),
        height: Dip(h),
    }
}
fn p(x: f32, y: f32) -> Point {
    Point {
        x: Dip(x),
        y: Dip(y),
    }
}
fn ev(kind: InputKind) -> InputEvent {
    InputEvent { time_us: 0, kind }
}
fn with(v: u16, modifiers: Modifiers) -> InputEvent {
    key(v, modifiers)
}
fn press(v: u16) -> InputEvent {
    key(v, Modifiers::default())
}
const CTRL: Modifiers = Modifiers {
    ctrl: true,
    alt: false,
    shift: false,
    win: false,
};
const ALT: Modifiers = Modifiers {
    ctrl: false,
    alt: true,
    shift: false,
    win: false,
};
const CTRL_SHIFT: Modifiers = Modifiers {
    ctrl: true,
    alt: false,
    shift: true,
    win: false,
};
fn up(v: u16) -> InputEvent {
    ev(InputKind::KeyUp {
        virtual_key: v,
        modifiers: Modifiers::default(),
    })
}
fn repeat(v: u16) -> InputEvent {
    ev(InputKind::KeyDown {
        virtual_key: v,
        modifiers: Modifiers::default(),
        repeat: true,
    })
}
fn down_at(x: f32, y: f32) -> InputEvent {
    down_button(x, y, MouseButton::Left)
}
fn down_button(x: f32, y: f32, button: MouseButton) -> InputEvent {
    ev(InputKind::MouseDown {
        at: p(x, y),
        button,
        modifiers: Modifiers::default(),
    })
}
fn up_at(x: f32, y: f32) -> InputEvent {
    ev(InputKind::MouseUp {
        at: p(x, y),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    })
}
fn move_to(x: f32, y: f32) -> InputEvent {
    ev(InputKind::MouseMove { at: p(x, y) })
}
fn wheel(x: f32, y: f32, delta: f32) -> InputEvent {
    ev(InputKind::Wheel {
        at: p(x, y),
        delta_x: 0.0,
        delta_y: delta,
        modifiers: Modifiers::default(),
    })
}
fn send(view: &mut dyn View, event: &InputEvent) -> ViewContext {
    let mut ctx = ViewContext::default();
    view.handle(event, &mut ctx);
    ctx
}
fn draw(view: &dyn View) -> DisplayList {
    let mut list = DisplayList::default();
    view.draw(&mut list, &Theme::new(ThemeKind::Light), &Measure);
    list
}
fn typed(text: &str) -> TextField {
    let mut f = TextField::new(WidgetId(1), "Name").with_clipboard(MemoryClipboard::shared());
    f.layout(r(0.0, 0.0, 240.0, 32.0), &Measure);
    f.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    for ch in text.chars() {
        f.handle(&ev(InputKind::Char(ch)), &mut ViewContext::default());
    }
    f
}
fn preview(f: &mut TextField, text: &str) {
    send(f, &ev(InputKind::ImeStart));
    send(
        f,
        &ev(InputKind::ImeUpdate {
            text: text.into(),
            cursor_utf16: 0,
        }),
    );
}
fn combo(n: usize) -> ComboBox {
    let items = (0..n).map(|i| Some(format!("Row {i}"))).collect();
    let mut c = ComboBox::new(WidgetId(2), "Encoding", items);
    c.layout(r(10.0, 10.0, 200.0, 32.0), &Measure);
    c.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    c
}

// Item 1
#[test]
fn a_cancelled_preview_keeps_the_selected_text() {
    let mut f = typed("hello");
    send(&mut f, &with(0x41, CTRL));
    preview(&mut f, "x");
    send(
        &mut f,
        &ev(InputKind::ImeEnd {
            text: String::new(),
        }),
    );
    assert_eq!(f.text, "hello");
    assert_eq!(f.composition, None);
    assert_eq!((f.anchor, f.caret), (0, 5), "the selection stays too");
}

// Item 2
#[test]
fn enter_during_a_preview_does_not_press_the_default_button() {
    let mut frame = DialogFrame::new(WidgetId(10), "Go to");
    frame.bounds = r(0.0, 0.0, 300.0, 200.0);
    let mut f = TextField::new(WidgetId(11), "Line").with_clipboard(MemoryClipboard::shared());
    f.bounds = r(8.0, 8.0, 200.0, 32.0);
    frame.add(Box::new(f));
    let mut ok = Button::new(WidgetId(12), "OK");
    ok.is_default = true;
    ok.bounds = r(8.0, 48.0, 92.0, 32.0);
    frame.add(Box::new(ok));
    frame.default_index = Some(1);
    frame.close_on_activate(1, 0);
    send(&mut frame, &ev(InputKind::ImeStart));
    for key_code in [0x0d, 0x09] {
        send(&mut frame, &press(key_code));
        send(&mut frame, &up(key_code));
    }
    assert!(!frame.closed, "Enter goes to the preview");
    assert_eq!(frame.focus_index, 0, "Tab goes to the preview");
    send(&mut frame, &ev(InputKind::ImeEnd { text: "x".into() }));
    send(&mut frame, &press(0x0d));
    send(&mut frame, &up(0x0d));
    assert!(
        frame.closed,
        "after the preview, Enter is the dialog's again"
    );
}

// Item 4
#[test]
fn every_user_change_is_reported_the_same_way() {
    let mut c = combo(4);
    assert!(!c.take_activated());
    send(&mut c, &press(0x28));
    assert!(c.take_activated(), "a key on a closed combo");
    assert!(!c.take_activated(), "once");
    send(&mut c, &wheel(20.0, 20.0, -120.0));
    assert!(c.take_activated(), "the wheel on a closed combo");
    send(&mut c, &ev(InputKind::Char('r')));
    assert!(c.take_activated(), "a letter on a closed combo");
    send(&mut c, &press(0x24));
    c.take_activated();
    send(&mut c, &press(0x24));
    assert!(!c.take_activated(), "no change, no report");

    let mut tabs = TabBar::new(WidgetId(3), "Pages", vec!["A".into(), "B".into()]);
    tabs.layout(r(0.0, 0.0, 200.0, 30.0), &Measure);
    send(&mut tabs, &ev(InputKind::Focus));
    send(&mut tabs, &press(0x27));
    assert!(tabs.take_activated(), "a tab by key");
    send(&mut tabs, &down_at(10.0, 10.0));
    assert!(tabs.take_activated(), "a tab by mouse");

    let mut f = typed("a");
    f.take_activated();
    send(&mut f, &ev(InputKind::Char('b')));
    assert!(f.take_activated(), "a typed character");

    let rows = (0..3)
        .map(|i| TableRow {
            label: format!("R{i}"),
            detail: String::new(),
            section: false,
            indent: 0,
        })
        .collect();
    let mut t = Table::new(WidgetId(4), "Keys", vec!["A".into()], rows);
    t.layout(r(0.0, 0.0, 200.0, 200.0), &Measure);
    send(&mut t, &ev(InputKind::Focus));
    send(&mut t, &press(0x28));
    assert!(t.take_activated(), "a table row by key");

    let mut s = ScrollBar::new(WidgetId(5), 500.0, 100.0);
    s.layout(r(0.0, 0.0, 12.0, 100.0), &Measure);
    send(&mut s, &ev(InputKind::Focus));
    send(&mut s, &press(0x28));
    assert!(s.take_activated(), "a scroll bar by key");
}
#[test]
fn a_tab_change_reaches_the_dialog() {
    let mut frame = DialogFrame::new(WidgetId(10), "Settings");
    frame.bounds = r(0.0, 0.0, 300.0, 200.0);
    let mut tabs = TabBar::new(WidgetId(11), "Pages", vec!["A".into(), "B".into()]);
    tabs.bounds = r(0.0, 0.0, 200.0, 30.0);
    frame.add(Box::new(tabs));
    send(&mut frame, &press(0x27));
    assert_eq!(frame.take_last_activated(), Some(0));
}

// Item 5
#[test]
fn alt_chords_pass_through_every_control() {
    let mut combo_box = combo(3);
    let mut button = Button::new(WidgetId(1), "x");
    let mut check = Checkbox::new(WidgetId(2), "x");
    let mut tabs = TabBar::new(WidgetId(3), "Pages", vec!["A".into(), "B".into()]);
    let mut table = Table::new(WidgetId(4), "Keys", vec![], vec![]);
    let mut bar = ScrollBar::new(WidgetId(5), 500.0, 100.0);
    let mut list = CheckList::new(WidgetId(6), "Encodings", vec![("A".into(), false)]);
    let mut menu = MenuView::new(WidgetId(7), "All menus", caret_menu());
    menu.layout(r(0.0, 0.0, 200.0, 300.0), &Measure);
    let mut field = typed("x");
    let views: Vec<&mut dyn View> = vec![
        &mut combo_box,
        &mut button,
        &mut check,
        &mut tabs,
        &mut table,
        &mut bar,
        &mut list,
        &mut menu,
        &mut field,
    ];
    for view in views {
        send(view, &ev(InputKind::Focus));
        for code in [0x73, 0x20, 0x0d, 0x25, 0x27, 0x24, 0x23] {
            let mut ctx = ViewContext::default();
            assert_eq!(
                view.handle(&with(code, ALT), &mut ctx),
                Handled::No,
                "{code:#x}"
            );
            assert!(ctx.requests.is_empty(), "{code:#x}: {:?}", ctx.requests);
        }
    }
    assert!(!combo_box.open, "Alt+F4 does not open a combo");
    assert!(!check.checked, "Alt+Space does not flip a checkbox");
    assert_eq!(button.clicks, 0);
    assert!(!button.pressed, "Alt+Space does not press a button");
}
#[test]
fn alt_down_and_up_stay_with_the_combo() {
    let mut c = combo(3);
    send(&mut c, &with(0x28, ALT));
    assert!(c.open);
    send(&mut c, &with(0x26, ALT));
    assert!(!c.open);
}
#[test]
fn alt_chords_pass_through_a_dialog() {
    let mut frame = DialogFrame::from_alert(
        WidgetId(100),
        &AlertModel::new("t", "m", vec!["OK".into(), "No".into()]),
        r(0.0, 0.0, 400.0, 150.0),
    );
    for code in [0x0d, 0x1b, 0x09] {
        let mut ctx = ViewContext::default();
        assert_eq!(frame.handle(&with(code, ALT), &mut ctx), Handled::No);
    }
    assert!(!frame.closed);
    assert_eq!(frame.focus_index, 1);
}

// Item 6
fn panel() -> EncodingPanel {
    EncodingPanel::new(
        WidgetId(1),
        TextEncoding::DEFAULT_LIST,
        r(0.0, 0.0, ENCODING_PANEL_WIDTH, ENCODING_PANEL_HEIGHT),
    )
}
#[test]
fn a_press_on_the_encoding_thumb_changes_no_setting() {
    let mut p = panel();
    let before = p.enabled_encodings();
    let list = p.frame.child::<CheckList>(0).unwrap().bounds;
    let x = list.x.0 + list.width.0 - 3.0;
    let y = list.y.0 + 8.0;
    let down = send(&mut p, &down_at(x, y));
    assert!(down.requests.contains(&ViewRequest::CaptureMouse(true)));
    assert_eq!(p.enabled_encodings(), before, "no row changed");
    // Dragging scrolls.
    send(&mut p, &move_to(x, y + 60.0));
    let scrolled = p.frame.child::<CheckList>(0).unwrap().first_visible;
    assert!(scrolled > 5, "first row {scrolled}");
    let released = send(&mut p, &up_at(x, y + 60.0));
    assert!(
        released
            .requests
            .contains(&ViewRequest::CaptureMouse(false))
    );
    assert_eq!(p.enabled_encodings(), before);
    // A press on the track below pages and changes nothing.
    let first = p.frame.child::<CheckList>(0).unwrap().first_visible;
    send(&mut p, &down_at(x, list.y.0 + list.height.0 - 3.0));
    send(&mut p, &up_at(x, list.y.0 + list.height.0 - 3.0));
    assert!(p.frame.child::<CheckList>(0).unwrap().first_visible >= first);
    assert_eq!(p.enabled_encodings(), before);
}
#[test]
fn a_press_on_a_menu_thumb_runs_no_row() {
    let items = (0..100)
        .map(|i| MenuItem::command(format!("Item {i}"), CommandId::Quit))
        .collect();
    let mut menu = MenuView::new(WidgetId(1), "All menus", MenuModel { items });
    menu.layout(r(0.0, 0.0, 200.0, 100.0), &Measure);
    let x = 197.0;
    let down = send(&mut menu, &down_at(x, 12.0));
    assert!(down.requests.contains(&ViewRequest::CaptureMouse(true)));
    send(&mut menu, &move_to(x, 60.0));
    assert!(menu.first_visible > 10, "{}", menu.first_visible);
    let released = send(&mut menu, &up_at(x, 60.0));
    assert!(
        !released
            .requests
            .contains(&ViewRequest::Run(CommandId::Quit))
    );
    assert!(
        released
            .requests
            .contains(&ViewRequest::CaptureMouse(false))
    );
    assert!(!menu.closed);
    // A press and release on a real row still runs it.
    send(&mut menu, &down_at(20.0, 15.0));
    let ran = send(&mut menu, &up_at(20.0, 15.0));
    assert!(ran.requests.contains(&ViewRequest::Run(CommandId::Quit)));
}
#[test]
fn a_press_on_a_combo_thumb_picks_no_row() {
    let mut c = combo(40);
    send(&mut c, &press(0x73));
    let popup = c.popup_rect();
    let x = popup.x.0 + popup.width.0 - 3.0;
    let y = popup.y.0 + 12.0;
    send(&mut c, &down_at(x, y));
    assert!(c.open);
    assert_eq!(c.selected, 0);
    send(&mut c, &move_to(x, y + 120.0));
    assert!(c.first_visible > 5, "{}", c.first_visible);
    send(&mut c, &up_at(x, y + 120.0));
    assert!(c.open);
    assert_eq!(c.selected, 0);
}

// Item 9
#[test]
fn a_browsed_value_is_dropped_on_blur_and_kept_on_enter() {
    let mut c = combo(4);
    send(&mut c, &press(0x73));
    send(&mut c, &press(0x28));
    assert_eq!(c.selected, 1);
    send(&mut c, &ev(InputKind::Blur));
    assert_eq!(c.selected, 0);
    assert!(!c.take_activated(), "nothing changed");

    let mut c = combo(4);
    send(&mut c, &press(0x73));
    send(&mut c, &press(0x28));
    send(&mut c, &press(0x0d));
    assert_eq!(c.selected, 1);
    assert!(c.take_activated());
}

// Item 10
#[test]
fn keys_during_a_preview_leave_the_text_alone() {
    let mut f = typed("hello");
    preview(&mut f, "x");
    for event in [
        press(0x08),
        press(0x2e),
        press(0x25),
        press(0x24),
        with(0x41, CTRL),
        with(0x5a, CTRL),
        with(0x56, CTRL),
    ] {
        let ctx = send(&mut f, &event);
        assert_eq!(f.text, "hello", "{ctx:?}");
    }
    assert_eq!(f.caret, 5);
    send(&mut f, &ev(InputKind::Char('q')));
    assert_eq!(f.text, "hello");
    assert_eq!(f.composition.as_ref().map(|c| c.0.as_str()), Some("x"));
}
#[test]
fn a_click_commits_the_preview_where_it_was() {
    let mut f = typed("abc");
    let _ = draw(&f);
    send(&mut f, &press(0x24));
    preview(&mut f, "Z");
    send(&mut f, &down_at(8.0 + 7.0 * 2.0, 10.0));
    assert_eq!(f.text, "Zabc", "the preview lands at its own place");
    assert_eq!(f.composition, None);
}

// Item 12
#[test]
fn a_list_opened_by_mouse_gets_a_ring_when_keys_take_over() {
    let items = vec![
        MenuItem::command("A", CommandId::Open),
        MenuItem::command("B", CommandId::Save),
    ];
    let mut menu = MenuView::new(WidgetId(1), "All menus", MenuModel { items });
    menu.layout(r(0.0, 0.0, 200.0, 80.0), &Measure);
    let rings = |menu: &MenuView| {
        draw(menu)
            .commands
            .iter()
            .filter(|c| matches!(c, DrawCommand::StrokeRect { width, .. } if width.0 == 2.0))
            .count()
    };
    send(&mut menu, &move_to(20.0, 15.0));
    assert_eq!(rings(&menu), 0, "a mouse highlight has no ring");
    send(&mut menu, &press(0x28));
    assert_eq!(rings(&menu), 1, "the keys have taken over");
    send(&mut menu, &move_to(20.0, 15.0));
    assert_eq!(rings(&menu), 0, "the mouse has taken back");
}

// Item 14 and the Zoom In break.
#[test]
fn every_key_has_a_name() {
    let of = |code: u16| key_text(KeyBinding::new(code, true, false, false));
    for (code, name) in [
        (0x25, "Ctrl+Left"),
        (0x26, "Ctrl+Up"),
        (0x27, "Ctrl+Right"),
        (0x28, "Ctrl+Down"),
        (0x2e, "Ctrl+Delete"),
        (0x2d, "Ctrl+Insert"),
        (0x0d, "Ctrl+Enter"),
        (0x08, "Ctrl+Backspace"),
        (0x1b, "Ctrl+Esc"),
        (0x21, "Ctrl+Page Up"),
        (0x22, "Ctrl+Page Down"),
        (0x23, "Ctrl+End"),
        (0x24, "Ctrl+Home"),
        (0xbf, "Ctrl+/"),
        (0xc0, "Ctrl+`"),
        (0xdb, "Ctrl+["),
        (0xdc, "Ctrl+\\"),
        (0xdd, "Ctrl+]"),
        (0xde, "Ctrl+'"),
        (0x60, "Ctrl+Num 0"),
        (0x69, "Ctrl+Num 9"),
        (0x6a, "Ctrl+Num *"),
        (0x6b, "Ctrl+Num +"),
        (0x6d, "Ctrl+Num -"),
        (0x6e, "Ctrl+Num ."),
        (0x6f, "Ctrl+Num /"),
        (0xbb, "Ctrl+="),
        (0xbd, "Ctrl+-"),
        (0xbe, "Ctrl+."),
        (0xbc, "Ctrl+,"),
        (0xba, "Ctrl+;"),
        (0x70, "Ctrl+F1"),
        (0x41, "Ctrl+A"),
        (0x30, "Ctrl+0"),
        (0x20, "Ctrl+Space"),
        (0x09, "Ctrl+Tab"),
    ] {
        assert_eq!(of(code), name, "{code:#x}");
    }
}
#[test]
fn every_default_key_in_the_menu_bar_is_named() {
    for command in CommandId::ALL {
        if let Some(binding) = command.default_key() {
            let text = key_text(binding);
            assert!(!text.contains("Key 0x"), "{command:?}: {text}");
        }
    }
    fn zoom_key(model: &MenuModel) -> Option<String> {
        model.items.iter().find_map(|item| {
            if item.command == Some(CommandId::ZoomIn) {
                return Some(item.key_text.clone());
            }
            item.submenu.as_ref().and_then(zoom_key)
        })
    }
    assert_eq!(
        zoom_key(&MenuModel {
            items: menu_bar_groups()
        })
        .as_deref(),
        Some("Ctrl+=")
    );
}

// Item 16
#[test]
fn table_cell_has_focus_and_left_right_move_it() {
    let rows = vec![TableRow {
        label: "New".into(),
        detail: "Ctrl+N".into(),
        section: false,
        indent: 0,
    }];
    let mut t = Table::new(
        WidgetId(1),
        "Keys",
        vec!["Command".into(), "Shortcut".into()],
        rows,
    );
    t.layout(r(0.0, 0.0, 300.0, 200.0), &Measure);
    send(&mut t, &ev(InputKind::Focus));
    let tree = t.widgets().unwrap();
    let focused = focused_nodes(&tree);
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].name, "Command");
    send(&mut t, &press(0x27));
    let tree = t.widgets().unwrap();
    let focused = focused_nodes(&tree);
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].role, WidgetRole::Cell);
    assert_eq!(focused[0].name, "Shortcut");
    assert_eq!(focused[0].value.as_deref(), Some("Ctrl+N"));
}
#[test]
fn placeholder_is_in_the_tree_while_the_field_is_empty() {
    let mut f = typed("");
    f.placeholder = "Line number".into();
    let tree = f.widgets().unwrap();
    assert_eq!(tree.children[0].role, WidgetRole::Label);
    assert_eq!(tree.children[0].name, "Line number");
    f.text = "1".into();
    assert!(f.widgets().unwrap().children.is_empty());
}
#[test]
fn table_tabs_and_menu_take_their_names_when_made() {
    assert_eq!(
        Table::new(WidgetId(1), "Keys", vec![], vec![])
            .widgets()
            .unwrap()
            .name,
        "Keys"
    );
    assert_eq!(
        TabBar::new(WidgetId(1), "Pages", vec![])
            .widgets()
            .unwrap()
            .name,
        "Pages"
    );
    assert_eq!(
        MenuView::new(WidgetId(1), "All menus", MenuModel::default())
            .widgets()
            .unwrap()
            .name,
        "All menus"
    );
}

// Item 19
#[test]
fn moving_focus_during_enter_releases_the_default_button() {
    let mut frame = DialogFrame::new(WidgetId(10), "Go to");
    frame.bounds = r(0.0, 0.0, 300.0, 200.0);
    let mut f = TextField::new(WidgetId(11), "Line").with_clipboard(MemoryClipboard::shared());
    f.bounds = r(8.0, 8.0, 200.0, 32.0);
    frame.add(Box::new(f));
    let mut ok = Button::new(WidgetId(12), "OK");
    ok.is_default = true;
    ok.bounds = r(8.0, 48.0, 92.0, 32.0);
    frame.add(Box::new(ok));
    let mut other = Button::new(WidgetId(13), "Other");
    other.bounds = r(108.0, 48.0, 92.0, 32.0);
    frame.add(Box::new(other));
    frame.default_index = Some(1);
    frame.close_on_activate(1, 0);
    send(&mut frame, &press(0x0d));
    assert!(frame.child::<Button>(1).unwrap().pressed);
    // Tab lands on the default button itself, which then has focus and no press.
    send(&mut frame, &press(0x09));
    send(&mut frame, &up(0x0d));
    assert!(!frame.child::<Button>(1).unwrap().pressed);
    assert!(!frame.closed);
    assert_eq!(frame.child::<Button>(1).unwrap().clicks, 0);
}

// Item 20
#[test]
fn holding_enter_does_not_flicker_a_combo() {
    let mut c = combo(3);
    send(&mut c, &press(0x0d));
    assert!(c.open);
    for code in [0x0d, 0x20, 0x73] {
        send(&mut c, &repeat(code));
        assert!(c.open, "{code:#x}");
    }
}

// Item 21
#[test]
fn a_right_press_outside_closes_the_list_and_cancels() {
    let mut c = combo(3);
    send(&mut c, &press(0x73));
    send(&mut c, &press(0x28));
    send(&mut c, &down_button(500.0, 500.0, MouseButton::Right));
    assert!(!c.open);
    assert_eq!(c.selected, 0);
}

// Item 22
#[test]
fn default_index_alone_makes_enter_press_the_button() {
    let mut frame = DialogFrame::new(WidgetId(10), "Go to");
    frame.bounds = r(0.0, 0.0, 300.0, 200.0);
    let mut f = TextField::new(WidgetId(11), "Line").with_clipboard(MemoryClipboard::shared());
    f.bounds = r(8.0, 8.0, 200.0, 32.0);
    frame.add(Box::new(f));
    let mut ok = Button::new(WidgetId(12), "OK");
    ok.bounds = r(8.0, 48.0, 92.0, 32.0);
    frame.add(Box::new(ok));
    frame.default_index = Some(1);
    frame.close_on_activate(1, 3);
    send(&mut frame, &press(0x0d));
    send(&mut frame, &up(0x0d));
    assert_eq!(frame.result(), Some(DialogResult::Button(3)));
}
#[test]
fn a_default_index_that_names_no_button_says_so() {
    let mut frame = DialogFrame::new(WidgetId(10), "Go to");
    let mut f = TextField::new(WidgetId(11), "Line").with_clipboard(MemoryClipboard::shared());
    f.bounds = r(8.0, 8.0, 200.0, 32.0);
    frame.add(Box::new(f));
    frame.default_index = Some(7);
    let mut ctx = ViewContext::default();
    assert_eq!(frame.handle(&press(0x0d), &mut ctx), Handled::No);
    assert!(frame.problem().unwrap().contains("default_index 7"));
}

// Item 23
#[test]
fn one_line_field_turns_every_line_break_and_tab_into_a_space() {
    let board = MemoryClipboard::shared();
    board
        .write_text("a\u{2028}b\u{2029}c\u{85}d\te\r\nf")
        .unwrap();
    let mut f = typed("").with_clipboard(board);
    send(&mut f, &with(0x56, CTRL));
    assert_eq!(f.text, "a b c d e f");
    let mut f = typed("");
    send(&mut f, &ev(InputKind::ImeStart));
    send(
        &mut f,
        &ev(InputKind::ImeEnd {
            text: "x\ny".into(),
        }),
    );
    assert_eq!(f.text, "x y");
}

// Item 24
#[test]
fn typing_is_one_undo_step_until_the_caret_moves() {
    let mut f = typed("hello");
    assert_eq!(f.undo_len(), 1);
    send(&mut f, &with(0x5a, CTRL));
    assert_eq!(f.text, "");
    send(&mut f, &with(0x59, CTRL));
    assert_eq!(f.text, "hello");
    let mut f = typed("ab");
    send(&mut f, &press(0x25));
    send(&mut f, &press(0x27));
    send(&mut f, &ev(InputKind::Char('c')));
    assert_eq!(f.undo_len(), 2, "moving the caret ends a step");
    send(&mut f, &with(0x5a, CTRL));
    assert_eq!(f.text, "ab");
    assert_eq!(f.caret, 2);
}
#[test]
fn backspaces_group_and_differ_from_typing() {
    let mut f = typed("abcd");
    send(&mut f, &press(0x08));
    send(&mut f, &press(0x08));
    assert_eq!(f.text, "ab");
    assert_eq!(f.undo_len(), 2, "typing, then backspaces");
    send(&mut f, &with(0x5a, CTRL));
    assert_eq!(f.text, "abcd");
    send(&mut f, &with(0x5a, CTRL));
    assert_eq!(f.text, "");
}
#[test]
fn ctrl_shift_z_redoes() {
    let mut f = typed("a");
    send(&mut f, &with(0x5a, CTRL));
    assert_eq!(f.text, "");
    send(&mut f, &with(0x5a, CTRL_SHIFT));
    assert_eq!(f.text, "a");
}

// Item 25 and the Page Down break.
#[test]
fn combo_wheel_stops_where_the_list_is_still_full() {
    let mut c = combo(20);
    send(&mut c, &press(0x73));
    let popup = c.popup_rect();
    send(
        &mut c,
        &wheel(popup.x.0 + 5.0, popup.y.0 + 20.0, -120_000.0),
    );
    assert_eq!(c.first_visible, 20 - UiTokens::sizes().popup_rows);
}
#[test]
fn combo_page_keys_move_by_a_page_open_or_closed() {
    let rows = UiTokens::sizes().popup_rows;
    let mut c = combo(30);
    send(&mut c, &press(0x22));
    assert_eq!(c.selected, rows);
    send(&mut c, &press(0x21));
    assert_eq!(c.selected, 0);
    send(&mut c, &press(0x73));
    send(&mut c, &press(0x22));
    assert_eq!(c.selected, rows);
    assert!(c.first_visible > 0, "the new row is in view");
}

// Item 26
#[test]
fn a_click_in_a_third_column_focuses_the_third_column() {
    let rows = vec![TableRow {
        label: "New".into(),
        detail: "Ctrl+N".into(),
        section: false,
        indent: 0,
    }];
    let mut t = Table::new(
        WidgetId(1),
        "Keys",
        vec!["A".into(), "B".into(), "C".into()],
        rows,
    );
    t.layout(r(0.0, 0.0, 400.0, 200.0), &Measure);
    send(&mut t, &down_at(390.0, 40.0));
    assert_eq!(t.focused_column, 2);
    send(&mut t, &down_at(250.0, 40.0));
    assert_eq!(t.focused_column, 1);
    send(&mut t, &down_at(10.0, 40.0));
    assert_eq!(t.focused_column, 0);
}

// Item 27
#[test]
fn double_click_selects_a_word() {
    let mut f = typed("one two three");
    let _ = draw(&f);
    let x = 8.0 + 7.0 * 5.0;
    let click = |f: &mut TextField, time_us: u64| {
        send(
            f,
            &InputEvent {
                time_us,
                kind: InputKind::MouseDown {
                    at: p(x, 10.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::default(),
                },
            },
        );
        send(f, &up_at(x, 10.0));
    };
    click(&mut f, 1_000_000);
    assert_eq!(f.caret, f.anchor);
    click(&mut f, 1_200_000);
    assert_eq!((f.anchor, f.caret), (4, 7), "two");
    click(&mut f, 5_000_000);
    assert_eq!(f.caret, f.anchor, "long after, it is a single click again");
}
#[test]
fn context_menu_comes_from_right_click_and_keys() {
    let mut f = typed("x");
    send(
        &mut f,
        &ev(InputKind::MouseUp {
            at: p(20.0, 10.0),
            button: MouseButton::Right,
            modifiers: Modifiers::default(),
        }),
    );
    assert_eq!(f.take_context_menu(), Some(p(20.0, 10.0)));
    assert_eq!(f.take_context_menu(), None);
    send(&mut f, &press(0x5d));
    assert!(f.take_context_menu().is_some());
    send(
        &mut f,
        &with(
            0x79,
            Modifiers {
                shift: true,
                ..Default::default()
            },
        ),
    );
    assert!(f.take_context_menu().is_some());
}

// Item 28
#[test]
fn ctrl_tab_switches_tabs_from_any_control_in_a_dialog() {
    let mut frame = DialogFrame::new(WidgetId(10), "Settings");
    frame.bounds = r(0.0, 0.0, 300.0, 200.0);
    let mut button = Button::new(WidgetId(11), "Restore");
    button.bounds = r(0.0, 100.0, 92.0, 32.0);
    frame.add(Box::new(button));
    let mut tabs = TabBar::new(
        WidgetId(12),
        "Pages",
        vec!["A".into(), "B".into(), "C".into()],
    );
    tabs.bounds = r(0.0, 0.0, 200.0, 30.0);
    frame.add(Box::new(tabs));
    send(&mut frame, &with(0x09, CTRL));
    assert_eq!(frame.child::<TabBar>(1).unwrap().selected, 1);
    assert_eq!(frame.focus_index, 0, "focus stays where it was");
    assert_eq!(frame.take_last_activated(), Some(1));
    send(&mut frame, &with(0x09, CTRL_SHIFT));
    send(&mut frame, &with(0x09, CTRL_SHIFT));
    assert_eq!(frame.child::<TabBar>(1).unwrap().selected, 2, "it wraps");
}

// Item 29
#[test]
fn a_combo_press_captures_and_a_release_or_blur_lets_go() {
    let mut c = combo(3);
    let down = send(&mut c, &down_at(20.0, 20.0));
    assert!(down.requests.contains(&ViewRequest::CaptureMouse(true)));
    let released = send(&mut c, &up_at(600.0, 600.0));
    assert!(
        released
            .requests
            .contains(&ViewRequest::CaptureMouse(false))
    );
    assert!(!c.pressed);
    let mut c = combo(3);
    send(&mut c, &down_at(20.0, 20.0));
    let blur = send(&mut c, &ev(InputKind::Blur));
    assert!(blur.requests.contains(&ViewRequest::CaptureMouse(false)));
    assert!(!c.pressed);
}

// Item 30
#[test]
fn a_rejected_edit_shows_as_a_warning_border_and_in_the_tree() {
    let mut f = typed("1");
    f.validator = Some(|s| s.chars().all(|c| c.is_ascii_digit()));
    let plain = draw(&f);
    send(&mut f, &ev(InputKind::Char('x')));
    assert_eq!(
        f.error_message().as_deref(),
        Some("\"x\" is not allowed here.")
    );
    let t = UiTokens::from_theme(&Theme::new(ThemeKind::Light));
    let warned = draw(&f);
    assert_ne!(plain, warned);
    assert!(
        warned
            .commands
            .iter()
            .any(|c| matches!(c, DrawCommand::StrokeRect { color, .. } if *color == t.warning))
    );
    let tree = f.widgets().unwrap();
    assert_eq!(tree.children[0].name, "\"x\" is not allowed here.");
    send(&mut f, &ev(InputKind::Char('2')));
    assert_eq!(f.error_message(), None);
}

// Also: encoding order, Open Recent, the second Alt tap.
#[test]
fn enabled_encodings_keep_the_default_order_until_changed() {
    let mut p = panel();
    assert_eq!(p.enabled_encodings(), TextEncoding::DEFAULT_LIST, "row I42");
    // The first change gives the panel's order, as the Mac's checkboxChanged does.
    send(&mut p, &press(0x20));
    let order: Vec<_> = TextEncoding::ALL
        .iter()
        .filter(|e| **e != TextEncoding::Utf8 && TextEncoding::DEFAULT_LIST.contains(e))
        .copied()
        .collect();
    assert_eq!(p.enabled_encodings(), order);
    // Revert gives the default order back.
    for _ in 0..3 {
        send(&mut p, &press(0x09));
    }
    send(&mut p, &press(0x20));
    send(&mut p, &up(0x20));
    assert_eq!(p.enabled_encodings(), TextEncoding::DEFAULT_LIST);
}
#[test]
fn encoding_panel_has_the_mac_line_above_the_list() {
    let tree = panel().widgets().unwrap();
    assert!(tree.children.iter().any(|c| c.role == WidgetRole::Label
        && c.name == "Select the encodings you want to appear in text encoding menus."));
}
#[test]
fn open_recent_rows_name_their_files() {
    let mut model = menu_bar_groups()[0].submenu.clone().unwrap();
    assert!(set_recent_files(
        &mut model,
        &[("notes.txt".into(), r"C:\notes.txt".into())]
    ));
    let (w, h) = model.preferred_size(&Measure);
    let mut menu = MenuView::new(WidgetId(1), "All menus", model);
    menu.layout(r(0.0, 0.0, w, h), &Measure);
    menu.open_keyboard();
    send(&mut menu, &ev(InputKind::Char('r'))); // Open Recent
    let ctx = send(&mut menu, &press(0x0d)); // The file
    assert!(
        ctx.requests
            .contains(&ViewRequest::Run(CommandId::OpenRecent))
    );
    assert!(menu.closed);
    assert_eq!(menu.take_chosen_data().as_deref(), Some(r"C:\notes.txt"));
    assert_eq!(menu.take_chosen_data(), None);
}
#[test]
fn a_second_alt_tap_closes_the_list() {
    let mut menu = MenuView::new(WidgetId(1), "All menus", caret_menu());
    menu.layout(r(0.0, 0.0, 200.0, 300.0), &Measure);
    menu.open_keyboard();
    send(&mut menu, &press(0x27));
    let alt = Modifiers {
        alt: true,
        ..Default::default()
    };
    send(&mut menu, &with(0x12, alt));
    assert!(!menu.closed);
    send(
        &mut menu,
        &ev(InputKind::KeyUp {
            virtual_key: 0x12,
            modifiers: alt,
        }),
    );
    assert!(menu.closed);
}

// Item 3: what a host needs for one window per level.
#[test]
fn a_menu_gives_its_levels_and_paints_each_at_its_own_origin() {
    let mut menu = MenuView::new(WidgetId(1), "All menus", legacy_caret_menu());
    menu.layout(r(100.0, 200.0, 200.0, 300.0), &Measure);
    menu.open_keyboard();
    assert_eq!(menu.levels().len(), 1);
    send(&mut menu, &press(0x27));
    let levels = menu.levels();
    assert_eq!(levels.len(), 2);
    assert_eq!(levels[0], r(100.0, 200.0, 200.0, 300.0));
    assert_eq!(menu.full_bounds().x, Dip(100.0));
    let theme = Theme::new(ThemeKind::Light);
    let mut list = DisplayList::default();
    menu.draw_level(1, &mut list, &theme, &Measure);
    let first = list.commands.first().unwrap();
    assert!(
        matches!(first, DrawCommand::RoundedRect { rect, .. } if rect.x.0 == 0.0 && rect.y.0 == 0.0),
        "{first:?}"
    );
    assert!(
        list.to_stable_text().contains("New Tab"),
        "the child's rows"
    );
    assert!(
        !list.to_stable_text().contains("Window"),
        "not the parent's"
    );
}

#[test]
fn checked_encoding_row_refuses_a_row_that_cannot_hold_the_text() {
    let rows = [
        EncodingRow::new("UTF-8"),
        EncodingRow {
            name: "Windows-1252".into(),
            enabled: false,
        },
    ];
    assert_eq!(checked_encoding_row(Ok(0), &rows), (Some(0), None));
    let (row, note) = checked_encoding_row(Ok(1), &rows);
    assert_eq!(row, None, "P23: a lossy encoding cannot get through");
    assert!(note.unwrap().contains("Windows-1252"));
    let (row, note) = checked_encoding_row(Err("no selection".into()), &rows);
    assert_eq!(row, None);
    assert!(note.unwrap().contains("no selection"));
}

// Final review, item 1: no key may panic on an empty list. Release builds abort on panic.
#[test]
fn every_key_is_safe_on_every_empty_list() {
    let keys = [
        0x08, 0x09, 0x0d, 0x1b, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x2e, 0x73,
    ];
    let bounds = r(0.0, 0.0, 200.0, 100.0);
    let section = |label: &str| TableRow {
        label: label.into(),
        detail: String::new(),
        section: true,
        indent: 0,
    };
    let only_separators = MenuModel {
        items: vec![MenuItem::separator(), MenuItem::separator()],
    };
    let mut disabled = MenuItem::command("Off", CommandId::Open);
    disabled.enabled = false;
    let only_disabled = MenuModel {
        items: vec![disabled],
    };
    let mut off_combo = ComboBox::new(WidgetId(1), "Encoding", vec![Some("Off".into()), None]);
    off_combo.disabled_items.insert(0);
    let mut views: Vec<Box<dyn View>> = vec![
        Box::new(MenuView::new(WidgetId(1), "Menu", MenuModel::default())),
        Box::new(MenuView::new(WidgetId(1), "Menu", only_separators)),
        Box::new(MenuView::new(WidgetId(1), "Menu", only_disabled)),
        Box::new(ComboBox::new(WidgetId(1), "Encoding", vec![])),
        Box::new(ComboBox::new(WidgetId(1), "Encoding", vec![None])),
        Box::new(off_combo),
        Box::new(Table::new(WidgetId(1), "Keys", vec![], vec![])),
        Box::new(Table::new(
            WidgetId(1),
            "Keys",
            vec!["A".into()],
            vec![section("One"), section("Two")],
        )),
        Box::new(TabBar::new(WidgetId(1), "Pages", vec![])),
        Box::new(CheckList::new(WidgetId(1), "Encodings", vec![])),
        Box::new(ScrollBar::new(WidgetId(1), 0.0, 0.0)),
    ];
    for view in &mut views {
        view.layout(bounds, &Measure);
        // Twice, because a first pass can open a list that the second then uses.
        for _ in 0..2 {
            send(view.as_mut(), &ev(InputKind::Focus));
            for code in keys {
                send(view.as_mut(), &press(code));
                send(view.as_mut(), &up(code));
                let _ = draw(view.as_ref());
                let _ = view.widgets();
            }
            send(view.as_mut(), &ev(InputKind::Char('a')));
            send(view.as_mut(), &wheel(10.0, 10.0, -120.0));
            send(view.as_mut(), &down_at(10.0, 10.0));
            send(view.as_mut(), &up_at(10.0, 10.0));
            send(view.as_mut(), &move_to(10.0, 10.0));
        }
    }
    let mut empty = MenuView::new(WidgetId(1), "Menu", MenuModel::default());
    empty.layout(bounds, &Measure);
    send(&mut empty, &press(0x21));
    assert_eq!(empty.selected, None);
}
#[test]
fn a_selected_row_past_the_end_of_a_changed_model_does_not_panic() {
    let mut menu = MenuView::new(WidgetId(1), "Menu", caret_menu());
    menu.layout(r(0.0, 0.0, 200.0, 100.0), &Measure);
    menu.open_keyboard();
    menu.model = MenuModel::default();
    send(&mut menu, &press(0x0d));
    send(&mut menu, &press(0x27));
}

// Final review, item 2.
#[test]
fn esc_cancels_a_preview_once_and_the_next_esc_reaches_the_dialog() {
    let mut frame = DialogFrame::new(WidgetId(10), "Go to");
    frame.bounds = r(0.0, 0.0, 300.0, 200.0);
    let mut f = TextField::new(WidgetId(11), "Line").with_clipboard(MemoryClipboard::shared());
    f.bounds = r(8.0, 8.0, 200.0, 32.0);
    f.text = "abc".into();
    frame.add(Box::new(f));
    send(&mut frame, &ev(InputKind::ImeStart));
    send(
        &mut frame,
        &ev(InputKind::ImeUpdate {
            text: "x".into(),
            cursor_utf16: 0,
        }),
    );
    let first = send(&mut frame, &press(0x1b));
    assert!(!frame.closed, "the first Esc belongs to the preview");
    let field = frame.child::<TextField>(0).unwrap();
    assert_eq!(field.composition, None, "and it ends the preview");
    assert_eq!(field.text, "abc", "with no change to the text");
    assert!(
        first
            .requests
            .iter()
            .any(|q| matches!(q, ViewRequest::Invalidate(_)))
    );
    send(&mut frame, &press(0x1b));
    assert!(frame.closed, "the next Esc reaches the dialog");
    assert_eq!(frame.result(), Some(DialogResult::Dismissed));
}

// Final review, item 4.
fn sections_then_rows(sections: usize, rows: usize) -> Table {
    let mut all = Vec::new();
    for i in 0..sections {
        all.push(TableRow {
            label: format!("Section {i}"),
            detail: String::new(),
            section: true,
            indent: 0,
        });
    }
    for i in 0..rows {
        all.push(TableRow {
            label: format!("Row {i}"),
            detail: String::new(),
            section: false,
            indent: 0,
        });
    }
    // 100 high: a header and two rows.
    let mut t = Table::new(WidgetId(1), "Keys", vec!["A".into()], all);
    t.layout(r(0.0, 0.0, 200.0, 100.0), &Measure);
    t.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    t
}
#[test]
fn home_keeps_the_selected_row_in_view_past_leading_sections() {
    let mut t = sections_then_rows(5, 5);
    send(&mut t, &press(0x23));
    send(&mut t, &press(0x24));
    assert_eq!(t.selected, 5);
    assert!(t.visible_range().contains(&5), "{:?}", t.visible_range());
}
#[test]
fn up_at_the_first_row_keeps_it_in_view_past_leading_sections() {
    let mut t = sections_then_rows(5, 5);
    t.first_visible = 5;
    t.selected = 5;
    send(&mut t, &press(0x26));
    assert_eq!(t.selected, 5);
    assert!(t.visible_range().contains(&5), "{:?}", t.visible_range());
}
#[test]
fn home_shows_a_leading_section_when_it_fits() {
    let mut t = sections_then_rows(1, 9);
    send(&mut t, &press(0x23));
    send(&mut t, &press(0x24));
    assert_eq!(t.selected, 1);
    assert_eq!(t.first_visible, 0);
}

// Final review, item 5.
#[test]
fn a_closed_menu_has_no_focused_node() {
    let mut menu = MenuView::new(WidgetId(1), "Menu", caret_menu());
    menu.layout(r(0.0, 0.0, 200.0, 300.0), &Measure);
    menu.open_keyboard();
    assert_eq!(focused_nodes(&menu.widgets().unwrap()).len(), 1);
    send(&mut menu, &press(0x1b));
    assert!(menu.closed);
    assert_eq!(focused_nodes(&menu.widgets().unwrap()).len(), 0);
    // With nothing selected, an open menu's own node has focus, and a closed one's not.
    let mut plain = MenuView::new(WidgetId(1), "Menu", caret_menu());
    assert_eq!(focused_nodes(&plain.widgets().unwrap()).len(), 1);
    send(&mut plain, &ev(InputKind::Blur));
    assert_eq!(focused_nodes(&plain.widgets().unwrap()).len(), 0);
}
