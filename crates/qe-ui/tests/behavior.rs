//! Regression tests for the findings of the M7 reviews. Each test replays input and
//! reads state, requests, the widget tree or the display list. No window opens.
use qe_api::{
    CommandId, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, Modifiers,
    MouseButton, Point, Rect, TextEncoding, TextMeasurer, TextMetrics, Theme, ThemeKind, View,
    ViewContext, ViewRequest, Widget, WidgetId, WidgetRole,
};
use qe_ui::*;
use std::sync::Arc;

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
fn press(v: u16) -> InputEvent {
    key(v, Modifiers::default())
}
fn ctrl(v: u16) -> InputEvent {
    key(
        v,
        Modifiers {
            ctrl: true,
            ..Default::default()
        },
    )
}
fn shift(v: u16) -> InputEvent {
    key(
        v,
        Modifiers {
            shift: true,
            ..Default::default()
        },
    )
}
fn alt(v: u16) -> InputEvent {
    key(
        v,
        Modifiers {
            alt: true,
            ..Default::default()
        },
    )
}
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
fn mouse_down(x: f32, y: f32) -> InputEvent {
    ev(InputKind::MouseDown {
        at: p(x, y),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    })
}
fn mouse_up(x: f32, y: f32) -> InputEvent {
    ev(InputKind::MouseUp {
        at: p(x, y),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
    })
}
fn mouse_move(x: f32, y: f32) -> InputEvent {
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
fn draw(view: &dyn View) -> DisplayList {
    let mut list = DisplayList::default();
    view.draw(&mut list, &Theme::new(ThemeKind::Light), &Measure);
    list
}
fn repaints(ctx: &ViewContext) -> bool {
    ctx.requests
        .iter()
        .any(|r| matches!(r, ViewRequest::Invalidate(_)))
}
fn field() -> TextField {
    let mut f = TextField::new(WidgetId(1), "Name").with_clipboard(MemoryClipboard::shared());
    f.layout(r(0.0, 0.0, 240.0, 32.0), &Measure);
    f.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    f
}
fn typed(text: &str) -> TextField {
    let mut f = field();
    for ch in text.chars() {
        f.handle(&ev(InputKind::Char(ch)), &mut ViewContext::default());
    }
    f
}
fn send(view: &mut dyn View, event: &InputEvent) -> ViewContext {
    let mut ctx = ViewContext::default();
    view.handle(event, &mut ctx);
    ctx
}
fn alert() -> DialogFrame {
    DialogFrame::from_alert(
        WidgetId(100),
        &AlertModel::new(
            "Save changes?",
            "The file has edits.",
            vec!["Save".into(), "Cancel".into(), "Discard".into()],
        ),
        r(0.0, 0.0, 400.0, 150.0),
    )
}
fn focused_name(frame: &DialogFrame) -> Vec<String> {
    let tree = frame.widgets().unwrap();
    focused_nodes(&tree)
        .iter()
        .map(|n| n.name.clone())
        .collect()
}

// Item 1
#[test]
fn dialog_enter_goes_to_focused_button() {
    let mut frame = alert();
    send(&mut frame, &press(0x09));
    assert_eq!(focused_name(&frame), ["Cancel"]);
    send(&mut frame, &press(0x0d));
    send(&mut frame, &up(0x0d));
    assert_eq!(frame.result(), Some(DialogResult::Button(1)));
}
#[test]
fn dialog_enter_goes_to_default_when_focus_is_on_a_field() {
    let mut frame = DialogFrame::new(WidgetId(1), "Go to");
    frame.bounds = r(0.0, 0.0, 300.0, 200.0);
    let mut f = TextField::new(WidgetId(2), "Line").with_clipboard(MemoryClipboard::shared());
    f.bounds = r(8.0, 8.0, 200.0, 32.0);
    frame.add(Box::new(f));
    let mut ok = Button::new(WidgetId(3), "OK");
    ok.bounds = r(8.0, 48.0, 92.0, 32.0);
    ok.is_default = true;
    frame.add(Box::new(ok));
    frame.default_index = Some(1);
    frame.close_on_activate(1, 7);
    assert_eq!(focused_name(&frame), ["Line"]);
    send(&mut frame, &press(0x0d));
    send(&mut frame, &up(0x0d));
    assert_eq!(frame.result(), Some(DialogResult::Button(7)));
}

// Item 2
#[test]
fn dialog_opens_with_the_first_button_focused_and_keeps_one_focus() {
    let mut frame = alert();
    assert_eq!(focused_name(&frame), ["Save"]);
    send(&mut frame, &press(0x09));
    assert_eq!(focused_name(&frame), ["Cancel"]);
    send(&mut frame, &shift(0x09));
    assert_eq!(focused_name(&frame), ["Save"]);
}
#[test]
fn dialog_focus_change_sends_blur_before_focus() {
    let mut frame = alert();
    let ctx = send(&mut frame, &press(0x09));
    let rings = draw(&frame)
        .commands
        .iter()
        .filter(|c| matches!(c, DrawCommand::StrokeRect { width, .. } if width.0 == 2.0))
        .count();
    assert_eq!(rings, 1, "exactly one focus ring shows");
    assert!(ctx.requests.contains(&ViewRequest::SetFocus(WidgetId(103))));
}
#[test]
fn dialog_tab_skips_label() {
    let mut frame = alert();
    send(&mut frame, &press(0x09));
    send(&mut frame, &press(0x09));
    assert_eq!(focused_name(&frame), ["Discard"]);
    send(&mut frame, &press(0x09));
    assert_eq!(focused_name(&frame), ["Save"], "Tab wraps past the message");
}
#[test]
fn dialog_tab_skips_disabled_controls() {
    let mut frame = alert();
    frame.child_mut::<Button>(2).unwrap().enabled = false;
    send(&mut frame, &press(0x09));
    assert_eq!(focused_name(&frame), ["Discard"]);
}
#[test]
fn space_on_cancel_presses_cancel() {
    let mut frame = alert();
    send(&mut frame, &press(0x09));
    let down = send(&mut frame, &press(0x20));
    assert!(repaints(&down));
    send(&mut frame, &up(0x20));
    assert_eq!(frame.result(), Some(DialogResult::Button(1)));
}
#[test]
fn dialog_escape_first_closes_combo() {
    let mut frame = DialogFrame::new(WidgetId(1), "Open");
    frame.bounds = r(0.0, 0.0, 300.0, 300.0);
    let mut combo = ComboBox::new(
        WidgetId(2),
        "Encoding",
        vec![Some("A".into()), Some("B".into())],
    );
    combo.bounds = r(8.0, 8.0, 200.0, 32.0);
    frame.add(Box::new(combo));
    send(&mut frame, &press(0x73));
    assert!(frame.child::<ComboBox>(0).unwrap().open);
    send(&mut frame, &press(0x1b));
    assert!(!frame.child::<ComboBox>(0).unwrap().open);
    assert!(!frame.closed, "the first Esc only closes the list");
    send(&mut frame, &press(0x1b));
    assert!(frame.closed);
    assert_eq!(frame.result(), Some(DialogResult::Dismissed));
}
#[test]
fn dialog_mouse_goes_to_the_control_under_the_pointer() {
    let mut frame = alert();
    let cancel = frame.child::<Button>(2).unwrap().bounds;
    let (x, y) = (cancel.x.0 + 5.0, cancel.y.0 + 5.0);
    send(&mut frame, &mouse_down(x, y));
    assert_eq!(focused_name(&frame), ["Cancel"]);
    send(&mut frame, &mouse_up(x, y));
    assert_eq!(frame.result(), Some(DialogResult::Button(1)));
}
#[test]
fn dialog_mouse_up_reaches_the_pressed_control_after_leaving_it() {
    let mut frame = alert();
    let cancel = frame.child::<Button>(2).unwrap().bounds;
    send(&mut frame, &mouse_down(cancel.x.0 + 5.0, cancel.y.0 + 5.0));
    let ctx = send(&mut frame, &mouse_up(5.0, 5.0));
    assert!(ctx.requests.contains(&ViewRequest::CaptureMouse(false)));
    assert_eq!(frame.result(), None, "a release elsewhere does not click");
}

#[test]
fn controls_under_an_open_list_get_no_hover() {
    let mut frame = DialogFrame::new(WidgetId(1), "Open");
    frame.bounds = r(0.0, 0.0, 300.0, 300.0);
    let mut c = ComboBox::new(
        WidgetId(2),
        "Encoding",
        vec![Some("A".into()), Some("B".into())],
    );
    c.bounds = r(8.0, 8.0, 200.0, 32.0);
    frame.add(Box::new(c));
    let mut below = Button::new(WidgetId(3), "Below");
    below.bounds = r(8.0, 44.0, 200.0, 40.0);
    frame.add(Box::new(below));
    send(&mut frame, &press(0x73));
    send(&mut frame, &mouse_move(20.0, 60.0));
    assert!(!frame.child::<Button>(1).unwrap().hovered);
    assert_eq!(frame.child::<ComboBox>(0).unwrap().hover_row, Some(0));
}

// Item 3
#[test]
fn alert_buttons_give_a_result_and_close() {
    for (index, tabs) in [(0, 0), (1, 1), (2, 2)] {
        let mut frame = alert();
        for _ in 0..tabs {
            send(&mut frame, &press(0x09));
        }
        assert_eq!(frame.result(), None);
        send(&mut frame, &press(0x0d));
        send(&mut frame, &up(0x0d));
        assert!(frame.closed);
        assert_eq!(frame.result(), Some(DialogResult::Button(index)));
        assert_eq!(send(&mut frame, &press(0x0d)).requests, vec![]);
    }
}
#[test]
fn a_closed_frame_can_be_dismissed_by_the_window_notice() {
    let mut frame = alert();
    frame.dismiss();
    assert!(frame.closed);
    assert_eq!(frame.result(), Some(DialogResult::Dismissed));
}

fn nested_menu() -> MenuView {
    let deep = MenuModel {
        items: vec![MenuItem::command("Deep", CommandId::Save)],
    };
    let mut mid_row = MenuItem::submenu("Mid", deep);
    mid_row.mnemonic = Some('m');
    let mid = MenuModel {
        items: vec![mid_row, MenuItem::command("Leaf", CommandId::New)],
    };
    let model = MenuModel {
        items: vec![
            MenuItem::submenu("File", mid),
            MenuItem::command("Open", CommandId::Open),
        ],
    };
    let mut menu = MenuView::new(WidgetId(1), model);
    menu.layout(r(100.0, 100.0, 200.0, 80.0), &Measure);
    menu.open_keyboard();
    menu
}

// Item 6
#[test]
fn submenu_activation_closes_root() {
    let mut menu = nested_menu();
    send(&mut menu, &press(0x27));
    send(&mut menu, &press(0x28));
    let ctx = send(&mut menu, &press(0x0d));
    assert!(ctx.requests.contains(&ViewRequest::Run(CommandId::New)));
    assert!(menu.closed);
    assert!(menu.child.is_none());
    assert!(repaints(&ctx));
}
#[test]
fn menu_closes_on_blur_and_outside_click() {
    let mut menu = nested_menu();
    send(&mut menu, &press(0x27));
    send(&mut menu, &ev(InputKind::Blur));
    assert!(menu.closed);
    let mut menu = nested_menu();
    send(&mut menu, &mouse_down(5.0, 5.0));
    assert!(menu.closed);
}
#[test]
fn a_press_on_an_open_child_does_not_close_the_list() {
    let mut menu = nested_menu();
    send(&mut menu, &press(0x27));
    let b = menu.child.as_ref().unwrap().bounds;
    send(&mut menu, &mouse_down(b.x.0 + 10.0, b.y.0 + 10.0));
    assert!(!menu.closed);
}

// Item 7
#[test]
fn disabled_menu_row_does_not_activate_previous() {
    let mut off = MenuItem::command("Disabled", CommandId::Save);
    off.enabled = false;
    let mut menu = MenuView::new(
        WidgetId(1),
        MenuModel {
            items: vec![MenuItem::command("Open", CommandId::Open), off],
        },
    );
    menu.layout(r(0.0, 0.0, 200.0, 80.0), &Measure);
    send(&mut menu, &mouse_move(20.0, 15.0));
    assert_eq!(menu.selected, Some(0));
    send(&mut menu, &mouse_move(20.0, 44.0));
    send(&mut menu, &mouse_down(20.0, 44.0));
    let ctx = send(&mut menu, &mouse_up(20.0, 44.0));
    assert!(!ctx.requests.contains(&ViewRequest::Run(CommandId::Open)));
    assert!(!ctx.requests.contains(&ViewRequest::Run(CommandId::Save)));
    assert!(!menu.closed);
}
#[test]
fn release_below_the_rows_runs_nothing() {
    let mut menu = MenuView::new(
        WidgetId(1),
        MenuModel {
            items: vec![MenuItem::command("Open", CommandId::Open)],
        },
    );
    menu.layout(r(0.0, 0.0, 200.0, 200.0), &Measure);
    send(&mut menu, &mouse_move(20.0, 15.0));
    send(&mut menu, &mouse_down(20.0, 150.0));
    let ctx = send(&mut menu, &mouse_up(20.0, 150.0));
    assert!(
        ctx.requests
            .iter()
            .all(|r| !matches!(r, ViewRequest::Run(_)))
    );
}
#[test]
fn release_needs_a_press_that_began_in_the_menu() {
    let mut menu = MenuView::new(
        WidgetId(1),
        MenuModel {
            items: vec![MenuItem::command("Open", CommandId::Open)],
        },
    );
    menu.layout(r(0.0, 0.0, 200.0, 60.0), &Measure);
    send(&mut menu, &mouse_move(20.0, 15.0));
    let without = send(&mut menu, &mouse_up(20.0, 15.0));
    assert!(
        without
            .requests
            .iter()
            .all(|r| !matches!(r, ViewRequest::Run(_)))
    );
    send(&mut menu, &mouse_down(20.0, 15.0));
    let with = send(&mut menu, &mouse_up(20.0, 15.0));
    assert!(with.requests.contains(&ViewRequest::Run(CommandId::Open)));
}

// Item 8
#[test]
fn mnemonic_with_case_expansion_does_not_panic() {
    let mut item = MenuItem::command("\u{130}\u{c4}", CommandId::Open);
    item.mnemonic = Some('\u{e4}');
    let mut menu = MenuView::new(WidgetId(1), MenuModel { items: vec![item] });
    menu.layout(r(0.0, 0.0, 200.0, 60.0), &Measure);
    menu.open_keyboard();
    let list = draw(&menu);
    assert!(
        list.commands
            .iter()
            .any(|c| matches!(c, DrawCommand::Line { from, .. } if from.x.0 > 7.0)),
        "the underline sits under the second character"
    );
}

// Item 9
fn alt_event(down: bool, v: u16, ctrl_flag: bool) -> InputEvent {
    let modifiers = Modifiers {
        ctrl: ctrl_flag,
        alt: true,
        ..Default::default()
    };
    ev(if down {
        InputKind::KeyDown {
            virtual_key: v,
            modifiers,
            repeat: false,
        }
    } else {
        InputKind::KeyUp {
            virtual_key: v,
            modifiers,
        }
    })
}
#[test]
fn generic_windows_alt_opens() {
    for v in [0x12, 0xa4, 0xa5] {
        let mut t = AltKeyTracker::default();
        assert!(!t.event(&alt_event(true, v, false)));
        assert!(t.event(&alt_event(false, v, false)), "alt code {v:#x}");
    }
}
#[test]
fn altgr_does_not_open() {
    let mut t = AltKeyTracker::default();
    t.event(&ev(InputKind::KeyDown {
        virtual_key: 0x11,
        modifiers: Modifiers {
            ctrl: true,
            ..Default::default()
        },
        repeat: false,
    }));
    t.event(&alt_event(true, 0xa5, true));
    assert!(!t.event(&alt_event(false, 0xa5, true)));
}
#[test]
fn blur_or_any_key_between_disarms_alt() {
    let mut t = AltKeyTracker::default();
    t.event(&alt_event(true, 0x12, false));
    t.event(&ev(InputKind::Blur));
    assert!(!t.event(&alt_event(false, 0x12, false)));
    t.event(&alt_event(true, 0x12, false));
    t.event(&press(0x41));
    assert!(!t.event(&alt_event(false, 0x12, false)));
    t.event(&alt_event(true, 0x12, false));
    t.event(&up(0x41));
    assert!(!t.event(&alt_event(false, 0x12, false)));
}
#[test]
fn held_alt_repeat_does_not_rearm() {
    let mut t = AltKeyTracker::default();
    t.event(&alt_event(true, 0x12, false));
    t.event(&press(0x41));
    t.event(&repeat(0x12));
    assert!(!t.event(&alt_event(false, 0x12, false)));
}

// Item 10
fn bar() -> ScrollBar {
    let mut s = ScrollBar::new(WidgetId(1), 1000.0, 100.0);
    s.layout(r(0.0, 0.0, 12.0, 100.0), &Measure);
    s.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    s
}
#[test]
fn scrollbar_supports_keyboard() {
    let mut s = bar();
    assert!(s.focused);
    let ctx = send(&mut s, &press(0x28));
    assert!(repaints(&ctx));
    assert_eq!(s.offset, s.line);
    send(&mut s, &press(0x26));
    assert_eq!(s.offset, 0.0);
    send(&mut s, &press(0x22));
    assert_eq!(s.offset, 100.0);
    send(&mut s, &press(0x21));
    assert_eq!(s.offset, 0.0);
    send(&mut s, &press(0x23));
    assert_eq!(s.offset, 900.0);
    send(&mut s, &press(0x24));
    assert_eq!(s.offset, 0.0);
    send(&mut s, &wheel(5.0, 5.0, -120.0));
    assert_eq!(s.offset, 3.0 * s.line);
    send(&mut s, &ev(InputKind::Blur));
    assert!(!s.focused);
    assert_eq!(send(&mut s, &press(0x28)).requests, vec![]);
}
#[test]
fn scrollbar_says_what_it_is_in_words() {
    let mut s = bar();
    s.offset = 450.0;
    let node = s.widgets().unwrap();
    assert!(!node.name.is_empty());
    assert_eq!(node.value.as_deref(), Some("50 percent, 450 of 900"));
    assert!(node.states.focused);
}
#[test]
fn scrollbar_thumb_stays_inside_track() {
    let mut s = ScrollBar::new(WidgetId(1), 10000.0, 100.0);
    s.layout(r(0.0, 10.0, 12.0, 100.0), &Measure);
    s.offset = 9900.0;
    let thumb = draw(&s).commands.iter().find_map(|c| match c {
        DrawCommand::RoundedRect { rect, .. } => Some(*rect),
        _ => None,
    });
    let thumb = thumb.unwrap();
    assert!(thumb.y.0 + thumb.height.0 <= 110.0 + 0.01, "{thumb:?}");
    assert_eq!(
        thumb.y.0 + thumb.height.0,
        110.0,
        "the end sits at the track's end"
    );
}
#[test]
fn scrollbar_drag_covers_the_whole_range_with_a_minimum_thumb() {
    let mut s = ScrollBar::new(WidgetId(1), 10000.0, 100.0);
    s.layout(r(0.0, 0.0, 12.0, 100.0), &Measure);
    send(&mut s, &mouse_down(5.0, 5.0));
    send(&mut s, &mouse_move(5.0, 89.0));
    assert_eq!(s.offset, 9900.0);
    let ctx = send(&mut s, &mouse_up(5.0, 89.0));
    assert!(ctx.requests.contains(&ViewRequest::CaptureMouse(false)));
}

// Item 11
#[test]
fn nested_left_closes_only_deepest() {
    let mut menu = nested_menu();
    send(&mut menu, &press(0x27));
    send(&mut menu, &press(0x27));
    assert!(menu.child.as_ref().unwrap().child.is_some());
    send(&mut menu, &press(0x25));
    let mid = menu.child.as_ref().expect("depth 2 stays open");
    assert!(mid.child.is_none(), "depth 3 closed");
    send(&mut menu, &press(0x25));
    assert!(menu.child.is_none());
    assert!(!menu.closed);
}
#[test]
fn nested_escape_closes_only_deepest() {
    let mut menu = nested_menu();
    send(&mut menu, &press(0x27));
    send(&mut menu, &press(0x27));
    send(&mut menu, &press(0x1b));
    assert!(menu.child.as_ref().unwrap().child.is_none());
    send(&mut menu, &press(0x1b));
    assert!(menu.child.is_none());
    assert!(!menu.closed);
    send(&mut menu, &press(0x1b));
    assert!(menu.closed);
}
#[test]
fn opening_and_closing_a_submenu_repaints_its_area() {
    let mut menu = nested_menu();
    let ctx = send(&mut menu, &press(0x27));
    let child = menu.child.as_ref().unwrap().bounds;
    assert!(ctx.requests.contains(&ViewRequest::Invalidate(child)));
    let ctx = send(&mut menu, &press(0x25));
    assert!(ctx.requests.contains(&ViewRequest::Invalidate(child)));
}
#[test]
fn submenu_respects_right_screen_edge_on_a_negative_monitor() {
    let mut menu = nested_menu();
    let work = r(-1920.0, -200.0, 1920.0, 1080.0);
    menu.work_area = Some(work);
    menu.layout(r(-210.0, 800.0, 200.0, 80.0), &Measure);
    send(&mut menu, &press(0x27));
    let child = menu.child.as_ref().unwrap().bounds;
    assert!(child.x.0 + child.width.0 <= 0.0, "{child:?}");
    assert!(child.x.0 >= -1920.0, "{child:?}");
    assert!(child.y.0 + child.height.0 <= 880.0, "{child:?}");
}
fn long_menu(n: usize) -> MenuView {
    let items = (0..n)
        .map(|i| MenuItem::command(format!("Item {i}"), CommandId::Open))
        .collect();
    let mut menu = MenuView::new(WidgetId(1), MenuModel { items });
    menu.layout(r(0.0, 0.0, 200.0, 100.0), &Measure);
    menu.open_keyboard();
    menu
}
#[test]
fn long_menu_draw_is_bounded_by_viewport() {
    let menu = long_menu(1000);
    let count = draw(&menu).commands.len();
    assert!(count < 40, "{count} commands for 1,000 items");
}
#[test]
fn long_menu_scrolls_to_keep_the_selection_visible() {
    let mut menu = long_menu(1000);
    send(&mut menu, &press(0x23));
    assert_eq!(menu.selected, Some(999));
    assert!(menu.first_visible > 900);
    assert!(draw(&menu).to_stable_text().contains("Item 999"));
    send(&mut menu, &press(0x24));
    assert_eq!(menu.selected, Some(0));
    assert_eq!(menu.first_visible, 0);
    send(&mut menu, &press(0x22));
    assert!(menu.selected.unwrap() >= 2);
    let before = menu.first_visible;
    send(&mut menu, &wheel(10.0, 10.0, -120.0));
    assert!(menu.first_visible > before);
}
#[test]
fn tick_opens_a_hovered_submenu_at_any_depth() {
    let mut menu = nested_menu();
    menu.keyboard_opened = false;
    send(&mut menu, &press(0x27));
    let child = menu.child.as_ref().unwrap().bounds;
    let row = ev(InputKind::MouseMove {
        at: p(child.x.0 + 10.0, child.y.0 + 12.0),
    });
    send(
        &mut menu,
        &InputEvent {
            time_us: 1000,
            ..row
        },
    );
    assert!(menu.child.as_ref().unwrap().child.is_none());
    assert_eq!(menu.tick(1100), None, "the delay has not passed");
    assert!(menu.tick(251_000).is_some());
    assert!(menu.child.as_ref().unwrap().child.is_some());
}
#[test]
fn moving_to_another_row_closes_the_old_child() {
    let mut menu = nested_menu();
    send(&mut menu, &press(0x27));
    assert!(menu.child.is_some());
    let ctx = send(&mut menu, &mouse_move(110.0, 100.0 + 8.0 + 28.0 + 5.0));
    assert!(menu.child.is_none());
    assert!(repaints(&ctx));
}
#[test]
fn a_label_letter_only_selects_and_a_mnemonic_runs() {
    let mut save = MenuItem::command("Save", CommandId::Save);
    save.mnemonic = Some('v');
    let mut menu = MenuView::new(
        WidgetId(1),
        MenuModel {
            items: vec![MenuItem::command("Open", CommandId::Open), save],
        },
    );
    menu.layout(r(0.0, 0.0, 200.0, 80.0), &Measure);
    menu.open_keyboard();
    let by_label = send(&mut menu, &ev(InputKind::Char('s')));
    assert_eq!(menu.selected, Some(1));
    assert!(
        by_label
            .requests
            .iter()
            .all(|r| !matches!(r, ViewRequest::Run(_)))
    );
    assert!(!menu.closed);
    let by_mnemonic = send(&mut menu, &ev(InputKind::Char('v')));
    assert!(
        by_mnemonic
            .requests
            .contains(&ViewRequest::Run(CommandId::Save))
    );
    assert!(menu.closed);
}
#[test]
fn long_labels_do_not_overlap_shortcuts() {
    let mut item = MenuItem::command(
        "A very long label that needs a lot of room",
        CommandId::Save,
    );
    item.key_text = "Ctrl+Shift+S".into();
    let model = MenuModel { items: vec![item] };
    let (w, _) = model.preferred_size(&Measure);
    assert!(w >= 42.0 * 7.0 + 12.0 * 7.0, "{w}");
    // In a menu too narrow for it, the label is clipped before the shortcut.
    let mut menu = MenuView::new(WidgetId(1), model);
    menu.layout(r(0.0, 0.0, 200.0, 60.0), &Measure);
    let list = draw(&menu);
    let key_x = list
        .commands
        .iter()
        .find_map(|c| match c {
            DrawCommand::TextRun { at, text, .. } if text == "Ctrl+Shift+S" => Some(at.x.0),
            _ => None,
        })
        .unwrap();
    let clip_end = list
        .commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::PushClip(r) if r.width.0 < 200.0 => Some(r.x.0 + r.width.0),
            _ => None,
        })
        .next()
        .unwrap();
    assert!(
        clip_end <= key_x,
        "label clip ends at {clip_end}, key starts at {key_x}"
    );
}
#[test]
fn nested_submenu_uses_measured_width() {
    let inner = MenuModel {
        items: vec![MenuItem::command("x".repeat(100), CommandId::Save)],
    };
    let mid = MenuModel {
        items: vec![MenuItem::submenu("Inner", inner)],
    };
    let mut menu = MenuView::new(
        WidgetId(1),
        MenuModel {
            items: vec![MenuItem::submenu("Mid", mid)],
        },
    );
    menu.layout(r(0.0, 0.0, 200.0, 60.0), &Measure);
    menu.open_keyboard();
    send(&mut menu, &press(0x27));
    send(&mut menu, &press(0x27));
    let deep = menu.child.as_ref().unwrap().child.as_ref().unwrap();
    assert!(deep.bounds.width.0 >= 700.0, "{:?}", deep.bounds);
}

fn combo() -> ComboBox {
    let mut c = ComboBox::new(
        WidgetId(1),
        "Encoding",
        vec![
            Some("UTF-8".into()),
            None,
            Some("Mac Roman".into()),
            Some("Mac Japanese".into()),
            Some("Mac Korean".into()),
            Some("Windows-1252".into()),
        ],
    );
    c.layout(r(10.0, 10.0, 200.0, 32.0), &Measure);
    c.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    c
}
// Item 12
#[test]
fn combo_keys_open_close_and_cancel() {
    let mut c = combo();
    send(&mut c, &alt(0x28));
    assert!(c.open);
    send(&mut c, &alt(0x26));
    assert!(!c.open);
    send(&mut c, &press(0x73));
    assert!(c.open);
    send(&mut c, &press(0x73));
    assert!(!c.open);
    send(&mut c, &press(0x73));
    send(&mut c, &press(0x28));
    assert_eq!(c.selected, 2);
    send(&mut c, &press(0x23));
    assert_eq!(c.selected, 5);
    send(&mut c, &press(0x24));
    assert_eq!(c.selected, 0);
    send(&mut c, &press(0x23));
    send(&mut c, &press(0x1b));
    assert!(!c.open);
    assert_eq!(
        c.selected, 0,
        "Esc puts back the value from before the list opened"
    );
}
#[test]
fn combo_escape_is_not_taken_while_closed() {
    let mut c = combo();
    let mut ctx = ViewContext::default();
    assert_eq!(c.handle(&press(0x1b), &mut ctx), Handled::No);
}
#[test]
fn combo_click_picks_a_row() {
    let mut c = combo();
    send(&mut c, &mouse_down(20.0, 20.0));
    assert!(c.open);
    let popup = c.popup_rect();
    // Row 0 is 28 high, then a 9 high separator, then the third row.
    let y = popup.y.0 + 8.0 + 28.0 + 9.0 + 10.0;
    send(&mut c, &mouse_down(20.0, y));
    assert!(!c.open);
    assert_eq!(c.selected, 2);
}
#[test]
fn combo_click_on_a_separator_or_disabled_row_keeps_the_list() {
    let mut c = combo();
    c.disabled_items.insert(3);
    send(&mut c, &mouse_down(20.0, 20.0));
    let popup = c.popup_rect();
    send(&mut c, &mouse_down(20.0, popup.y.0 + 8.0 + 28.0 + 2.0));
    assert!(c.open);
    send(
        &mut c,
        &mouse_down(20.0, popup.y.0 + 8.0 + 28.0 + 9.0 + 28.0 + 10.0),
    );
    assert!(c.open);
    assert_eq!(c.selected, 0);
}
#[test]
fn combo_click_outside_closes() {
    let mut c = combo();
    send(&mut c, &mouse_down(20.0, 20.0));
    send(&mut c, &mouse_down(500.0, 500.0));
    assert!(!c.open);
}
#[test]
fn combo_open_row_is_highlighted() {
    let mut c = combo();
    c.open = true;
    c.selected = 2;
    let first = draw(&c);
    c.selected = 3;
    let second = draw(&c);
    assert_ne!(first, second);
    let t = UiTokens::from_theme(&Theme::new(ThemeKind::Light));
    assert!(first.commands.iter().any(
        |command| matches!(command, DrawCommand::RoundedRect { color, .. } if *color == t.selected)
    ));
}
#[test]
fn typing_the_same_letter_cycles_through_rows() {
    let mut c = combo();
    let m = ev(InputKind::Char('m'));
    send(&mut c, &m);
    assert_eq!(c.selected, 2);
    send(&mut c, &m);
    assert_eq!(c.selected, 3);
    send(&mut c, &m);
    assert_eq!(c.selected, 4);
    send(&mut c, &m);
    assert_eq!(c.selected, 2, "it wraps");
}
#[test]
fn different_letters_within_a_second_build_a_prefix() {
    let mut c = combo();
    let typed_at = |c: &mut ComboBox, ch: char, time_us: u64| {
        send(
            c,
            &InputEvent {
                time_us,
                kind: InputKind::Char(ch),
            },
        );
    };
    typed_at(&mut c, 'm', 10);
    typed_at(&mut c, 'a', 20);
    assert_eq!(c.selected, 2, "Mac Roman still starts with ma");
    typed_at(&mut c, 'w', 2_000_000);
    assert_eq!(c.selected, 5, "after a pause, a letter starts over");
    typed_at(&mut c, 'x', 2_000_100);
    assert_eq!(c.selected, 5, "no row starts with wx, so nothing moves");
}
#[test]
fn combo_opening_invalidates_the_list_area() {
    let mut c = combo();
    let ctx = send(&mut c, &press(0x0d));
    assert!(
        ctx.requests
            .contains(&ViewRequest::Invalidate(c.popup_rect()))
    );
}
#[test]
fn combo_blur_closes_and_repaints() {
    let mut c = combo();
    send(&mut c, &press(0x0d));
    let popup = c.popup_rect();
    let ctx = send(&mut c, &ev(InputKind::Blur));
    assert!(!c.open);
    assert!(ctx.requests.contains(&ViewRequest::Invalidate(popup)));
}
#[test]
fn combo_steps_skip_disabled_rows_with_names() {
    let mut c = combo();
    c.disabled_items.insert(2);
    send(&mut c, &press(0x28));
    assert_eq!(c.selected, 3, "a named disabled row is passed over");
    c.open = true;
    let text = draw(&c).to_stable_text();
    assert!(text.contains("Mac Roman"), "and it still shows");
}
#[test]
fn combo_list_paints_above_siblings_in_a_dialog() {
    let mut frame = DialogFrame::new(WidgetId(1), "Open");
    frame.bounds = r(0.0, 0.0, 300.0, 300.0);
    let mut c = ComboBox::new(
        WidgetId(2),
        "Encoding",
        vec![Some("A".into()), Some("B".into())],
    );
    c.bounds = r(8.0, 8.0, 200.0, 32.0);
    frame.add(Box::new(c));
    let mut below = Button::new(WidgetId(3), "Below");
    below.bounds = r(8.0, 44.0, 92.0, 32.0);
    frame.add(Box::new(below));
    send(&mut frame, &press(0x73));
    let text = draw(&frame).to_stable_text();
    let below_at = text.find("\"Below\"").unwrap();
    let option_at = text.find("\"B\"").unwrap();
    assert!(
        option_at > below_at,
        "the list comes after the button below it"
    );
}
#[test]
fn combo_list_stays_inside_the_dialog_and_scrolls() {
    let mut frame = DialogFrame::new(WidgetId(1), "Open");
    frame.bounds = r(0.0, 0.0, 300.0, 120.0);
    let items = (0..40).map(|i| Some(format!("Encoding {i}"))).collect();
    let mut c = ComboBox::new(WidgetId(2), "Encoding", items);
    c.bounds = r(8.0, 8.0, 200.0, 32.0);
    frame.add(Box::new(c));
    send(&mut frame, &press(0x73));
    let popup = frame.child::<ComboBox>(0).unwrap().popup_rect();
    assert!(
        popup.y.0 >= 0.0 && popup.y.0 + popup.height.0 <= 120.0,
        "{popup:?}"
    );
    send(&mut frame, &press(0x23));
    let c = frame.child::<ComboBox>(0).unwrap();
    assert_eq!(c.selected, 39);
    assert!(c.first_visible > 30);
    assert!(draw(&frame).to_stable_text().contains("Encoding 39"));
}
#[test]
fn a_click_on_a_row_above_a_sibling_goes_to_the_list() {
    let mut frame = DialogFrame::new(WidgetId(1), "Open");
    frame.bounds = r(0.0, 0.0, 300.0, 300.0);
    let mut c = ComboBox::new(
        WidgetId(2),
        "Encoding",
        vec![Some("A".into()), Some("B".into())],
    );
    c.bounds = r(8.0, 8.0, 200.0, 32.0);
    frame.add(Box::new(c));
    let mut below = Button::new(WidgetId(3), "Below");
    below.bounds = r(8.0, 44.0, 200.0, 60.0);
    frame.add(Box::new(below));
    send(&mut frame, &press(0x73));
    let popup = frame.child::<ComboBox>(0).unwrap().popup_rect();
    send(&mut frame, &mouse_down(20.0, popup.y.0 + 8.0 + 28.0 + 10.0));
    send(&mut frame, &mouse_up(20.0, popup.y.0 + 8.0 + 28.0 + 10.0));
    assert_eq!(frame.child::<ComboBox>(0).unwrap().selected, 1);
    assert_eq!(frame.child::<Button>(1).unwrap().clicks, 0);
}

fn row(label: &str, section: bool) -> TableRow {
    TableRow {
        label: label.into(),
        detail: if section {
            String::new()
        } else {
            "Ctrl+K".into()
        },
        section,
        indent: 0,
    }
}
fn table(n: usize, height: f32) -> Table {
    let rows = (0..n).map(|i| row(&format!("Row {i}"), false)).collect();
    let mut t = Table::new(WidgetId(1), vec!["Command".into(), "Shortcut".into()], rows);
    t.layout(r(0.0, 0.0, 300.0, height), &Measure);
    t.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    t
}
// Item 13
#[test]
fn focused_table_has_visual_focus() {
    let mut t = table(4, 200.0);
    let focused = draw(&t);
    send(&mut t, &ev(InputKind::Blur));
    assert_ne!(focused, draw(&t));
}
#[test]
fn table_page_keys_enter_and_columns() {
    let mut t = table(100, 200.0);
    let page = t.visible_range().len();
    send(&mut t, &press(0x22));
    assert_eq!(t.selected, page);
    send(&mut t, &press(0x21));
    assert_eq!(t.selected, 0);
    send(&mut t, &press(0x27));
    assert_eq!(t.focused_column, 1);
    send(&mut t, &press(0x27));
    assert_eq!(t.focused_column, 1);
    send(&mut t, &press(0x25));
    assert_eq!(t.focused_column, 0);
    send(&mut t, &press(0x0d));
    assert_eq!(t.activations, 1);
    assert!(t.take_activated());
}
#[test]
fn table_wheel_scrolls_inside_its_bounds() {
    let mut t = table(100, 200.0);
    send(&mut t, &wheel(10.0, 10.0, -120.0));
    assert_eq!(t.first_visible, 3);
    send(&mut t, &wheel(10.0, 10.0, 120.0));
    assert_eq!(t.first_visible, 0);
    send(&mut t, &wheel(10.0, 10.0, -120000.0));
    assert_eq!(t.first_visible, 100 - t.visible_range().len());
    let mut ctx = ViewContext::default();
    assert_eq!(
        t.handle(&wheel(900.0, 900.0, -120.0), &mut ctx),
        Handled::No
    );
}
#[test]
fn section_rows_are_not_selectable() {
    let rows = vec![
        row("File", true),
        row("New", false),
        row("Edit", true),
        row("Undo", false),
    ];
    let mut t = Table::new(WidgetId(1), vec!["Command".into()], rows);
    t.layout(r(0.0, 0.0, 300.0, 200.0), &Measure);
    t.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    assert_eq!(t.selected, 1, "it starts past the first section");
    send(&mut t, &press(0x28));
    assert_eq!(t.selected, 3, "Down jumps over a section");
    send(&mut t, &press(0x26));
    assert_eq!(t.selected, 1, "Up jumps back");
    send(&mut t, &press(0x26));
    assert_eq!(t.selected, 1, "nothing above");
    // A click on a section row changes nothing.
    send(&mut t, &mouse_down(5.0, 28.0 * 3.0 + 5.0));
    assert_eq!(t.selected, 1);
    send(&mut t, &press(0x24));
    assert_eq!(t.selected, 1);
    send(&mut t, &press(0x23));
    assert_eq!(t.selected, 3);
}
#[test]
fn partly_hidden_last_row_does_not_count_as_visible() {
    let t = table(50, 100.0);
    assert_eq!(t.visible_range().len(), 2);
    let tree = t.widgets().unwrap();
    assert_eq!(tree.children.len(), 2);
    for child in &tree.children {
        assert!(
            child.bounds.y.0 + child.bounds.height.0 <= 100.0,
            "{:?}",
            child.bounds
        );
    }
}
#[test]
fn scrolled_table_header_click_does_not_select_hidden_row() {
    let mut t = table(50, 200.0);
    t.first_visible = 5;
    t.selected = 20;
    send(&mut t, &mouse_down(5.0, 5.0));
    assert_eq!(t.selected, 20);
    send(&mut t, &mouse_down(5.0, 28.0 + 5.0));
    assert_eq!(t.selected, 5);
}
#[test]
fn narrow_table_keeps_selected_row_fully_visible() {
    let mut t = table(50, 100.0);
    send(&mut t, &press(0x23));
    assert_eq!(t.selected, 49);
    let last = t
        .widgets()
        .unwrap()
        .children
        .into_iter()
        .find(|c| c.states.selected)
        .expect("the selected row is in the tree");
    assert!(
        last.bounds.y.0 + last.bounds.height.0 <= 100.0,
        "{:?}",
        last.bounds
    );
}
#[test]
fn header_columns_line_up_with_cell_columns() {
    let t = table(3, 200.0);
    let list = draw(&t);
    let x_of = |name: &str| {
        list.commands
            .iter()
            .find_map(|c| match c {
                DrawCommand::TextRun { at, text, .. } if text == name => Some(at.x.0),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(x_of("Shortcut"), x_of("Ctrl+K"));
    assert_eq!(x_of("Command"), x_of("Row 0"));
}

// Item 14
#[test]
fn focus_changes_repaint_tabs_tables_and_combos() {
    let mut tabs = TabBar::new(WidgetId(1), vec!["A".into()]);
    tabs.layout(r(0.0, 0.0, 100.0, 30.0), &Measure);
    let mut t = table(3, 100.0);
    let mut c = combo();
    for view in [&mut tabs as &mut dyn View, &mut t, &mut c] {
        assert!(repaints(&send(view, &ev(InputKind::Focus))));
        assert!(repaints(&send(view, &ev(InputKind::Blur))));
    }
}
#[test]
fn tab_bar_home_end_hover_and_pressed() {
    let mut tabs = TabBar::new(WidgetId(1), vec!["A".into(), "B".into(), "C".into()]);
    tabs.layout(r(0.0, 0.0, 300.0, 30.0), &Measure);
    send(&mut tabs, &ev(InputKind::Focus));
    send(&mut tabs, &press(0x23));
    assert_eq!(tabs.selected, 2);
    send(&mut tabs, &press(0x24));
    assert_eq!(tabs.selected, 0);
    let plain = draw(&tabs);
    send(&mut tabs, &mouse_move(150.0, 10.0));
    assert_eq!(tabs.hovered, Some(1));
    let hover = draw(&tabs);
    assert_ne!(plain, hover);
    let down = send(&mut tabs, &mouse_down(150.0, 10.0));
    assert!(down.requests.contains(&ViewRequest::CaptureMouse(true)));
    assert_eq!(tabs.pressed, Some(1));
    let released = send(&mut tabs, &mouse_up(150.0, 10.0));
    assert!(
        released
            .requests
            .contains(&ViewRequest::CaptureMouse(false))
    );
    assert_eq!(tabs.pressed, None);
}

// Item 15
#[test]
fn select_all_requests_repaint() {
    let mut f = typed("abc");
    assert!(repaints(&send(&mut f, &ctrl(0x41))));
    assert_eq!((f.anchor, f.caret), (0, 3));
}
#[test]
fn same_length_paste_requests_repaint() {
    let board = MemoryClipboard::shared();
    board.write_text("xyz").unwrap();
    let mut f = typed("abc").with_clipboard(board);
    send(&mut f, &ctrl(0x41));
    let ctx = send(&mut f, &ctrl(0x56));
    assert_eq!(f.text, "xyz");
    assert!(repaints(&ctx));
    let ctx = send(&mut f, &ctrl(0x5a));
    assert_eq!(f.text, "abc");
    assert!(repaints(&ctx), "a same-length undo repaints too");
}
#[test]
fn left_collapses_selection_to_start_and_right_to_end() {
    let mut f = typed("abc");
    send(&mut f, &ctrl(0x41));
    send(&mut f, &press(0x25));
    assert_eq!((f.caret, f.anchor), (0, 0));
    send(&mut f, &ctrl(0x41));
    send(&mut f, &press(0x27));
    assert_eq!((f.caret, f.anchor), (3, 3));
    send(&mut f, &press(0x24));
    send(&mut f, &shift(0x27));
    assert_eq!((f.caret, f.anchor), (1, 0));
    send(&mut f, &press(0x27));
    assert_eq!(
        (f.caret, f.anchor),
        (1, 1),
        "Right at a selection's end stays at the end"
    );
}
#[test]
fn backspace_refused_by_the_validator_keeps_the_caret_and_no_selection() {
    let mut f = typed("12");
    f.validator = Some(|s| s.len() == 2);
    send(&mut f, &press(0x08));
    assert_eq!(f.text, "12");
    assert_eq!(f.caret, f.anchor, "no selection is left behind");
    assert_eq!(f.rejected_input.as_deref(), Some(""));
}
#[test]
fn cut_without_selection_adds_no_undo_step() {
    let mut f = typed("abc");
    let steps = f.undo_len();
    send(&mut f, &ctrl(0x58));
    assert_eq!(f.undo_len(), steps);
    assert_eq!(f.text, "abc");
}
#[test]
fn undo_history_has_a_limit_and_keeps_changes_not_copies() {
    let mut f = field();
    for _ in 0..(UNDO_MAX_STEPS + 100) {
        send(&mut f, &ev(InputKind::Char('a')));
    }
    assert_eq!(f.undo_len(), UNDO_MAX_STEPS);
    assert_eq!(
        f.undo_held_bytes(),
        UNDO_MAX_STEPS,
        "one byte per step, no copies of the text"
    );
    let board = MemoryClipboard::shared();
    board.write_text(&"b".repeat(UNDO_MAX_BYTES)).unwrap();
    let mut f = f.with_clipboard(board);
    send(&mut f, &ctrl(0x56));
    send(&mut f, &press(0x24));
    send(&mut f, &ctrl(0x56));
    assert!(
        f.undo_held_bytes() <= UNDO_MAX_BYTES,
        "{}",
        f.undo_held_bytes()
    );
    assert_eq!(
        f.undo_len(),
        1,
        "the newest step stays even when it fills the limit"
    );
}
#[test]
fn caller_shortening_text_does_not_panic_in_draw_or_keys() {
    let mut f = typed("héllo wörld");
    f.text = "é".into();
    f.anchor = 99;
    let _ = draw(&f);
    send(&mut f, &press(0x25));
    let mut g = typed("héllo");
    g.caret = 2; // Inside the two-byte é.
    g.anchor = 2;
    let _ = draw(&g);
    send(&mut g, &press(0x08));
    assert_eq!(
        g.text, "éllo",
        "the caret moved back to before é, so h went"
    );
}
#[test]
fn undo_after_the_caller_changed_text_ends_history_and_does_not_panic() {
    let mut f = typed("abc");
    f.text = "z".into();
    f.caret = 1;
    f.anchor = 1;
    send(&mut f, &ctrl(0x5a));
    assert_eq!(f.text, "z");
    assert_eq!(f.undo_len(), 0);
}

// Item 16
#[test]
fn modifier_emoji_backspace_is_atomic() {
    let mut f = typed("👩🏽‍💻");
    send(&mut f, &press(0x08));
    assert_eq!(f.text, "");
}
#[test]
fn flag_backspace_is_atomic_and_arrows_stop_at_clusters() {
    let mut f = typed("a🇺🇸é");
    send(&mut f, &press(0x25));
    assert_eq!(&f.text[f.caret..], "é");
    send(&mut f, &press(0x25));
    assert_eq!(f.caret, 1);
    send(&mut f, &press(0x2e));
    assert_eq!(f.text, "aé");
    send(&mut f, &press(0x27));
    send(&mut f, &press(0x08));
    assert_eq!(f.text, "a");
}

// Item 17
fn run_ranges(list: &DisplayList) -> Vec<(f32, String)> {
    list.commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::TextRun { at, text, .. } => Some((at.x.0, text.clone())),
            _ => None,
        })
        .collect()
}
fn caret_line(list: &DisplayList) -> Option<f32> {
    list.commands.iter().find_map(|c| match c {
        DrawCommand::Line { from, to, .. } if from.x == to.x => Some(from.x.0),
        _ => None,
    })
}
#[test]
fn long_field_clips_to_bounds_and_keeps_the_caret_visible() {
    let mut f = TextField::new(WidgetId(1), "Name").with_clipboard(MemoryClipboard::shared());
    f.layout(r(0.0, 0.0, 100.0, 32.0), &Measure);
    f.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    for ch in "abcdefghijklmnopqrstuvwxyz".chars() {
        f.handle(&ev(InputKind::Char(ch)), &mut ViewContext::default());
    }
    let list = draw(&f);
    let caret = caret_line(&list).expect("a focused field shows its caret");
    assert!((8.0..=92.0).contains(&caret), "caret at {caret}");
    assert!(f.scroll_offset() > 0.0);
    assert!(
        list.commands.iter().any(
            |c| matches!(c, DrawCommand::PushClip(r) if r.x.0 >= 0.0 && r.x.0 + r.width.0 <= 100.0)
        ),
        "the text is clipped to the field"
    );
    send(&mut f, &press(0x24));
    let list = draw(&f);
    assert_eq!(caret_line(&list), Some(8.0));
    assert_eq!(f.scroll_offset(), 0.0);
}
#[test]
fn click_sets_the_caret_by_hit_test_and_drag_selects() {
    let mut f = typed("abcdefgh");
    let _ = draw(&f);
    let down = send(&mut f, &mouse_down(8.0 + 7.0 * 2.0 + 1.0, 10.0));
    assert_eq!(f.caret, 2);
    assert_eq!(f.anchor, 2);
    assert!(down.requests.contains(&ViewRequest::CaptureMouse(true)));
    let moved = send(&mut f, &mouse_move(8.0 + 7.0 * 5.0 + 1.0, 10.0));
    assert_eq!((f.anchor, f.caret), (2, 5));
    assert!(repaints(&moved));
    let released = send(&mut f, &mouse_up(8.0 + 7.0 * 5.0, 10.0));
    assert!(
        released
            .requests
            .contains(&ViewRequest::CaptureMouse(false))
    );
    let after = send(&mut f, &mouse_move(8.0, 10.0));
    assert_eq!(f.caret, 5, "a move after the release selects nothing");
    assert!(!repaints(&after));
}
#[test]
fn a_click_needs_only_layout_and_not_a_draw() {
    let mut f = TextField::new(WidgetId(1), "Name").with_clipboard(MemoryClipboard::shared());
    f.text = "abcdef".into();
    f.layout(r(0.0, 0.0, 200.0, 32.0), &Measure);
    send(&mut f, &mouse_down(8.0 + 7.0 * 3.0, 10.0));
    assert_eq!(f.caret, 3);
}
#[test]
fn shift_click_extends() {
    let mut f = typed("abcdef");
    let _ = draw(&f);
    send(&mut f, &press(0x24));
    send(
        &mut f,
        &ev(InputKind::MouseDown {
            at: p(8.0 + 7.0 * 4.0, 10.0),
            button: MouseButton::Left,
            modifiers: Modifiers {
                shift: true,
                ..Default::default()
            },
        }),
    );
    assert_eq!((f.anchor, f.caret), (0, 4));
}

// Item 18
#[test]
fn ime_preview_moves_the_rest_of_the_text_and_honors_its_cursor() {
    let mut f = typed("ab");
    send(&mut f, &press(0x25));
    send(&mut f, &ev(InputKind::ImeStart));
    send(
        &mut f,
        &ev(InputKind::ImeUpdate {
            text: "xy".into(),
            cursor_utf16: 0,
        }),
    );
    let at_zero = draw(&f);
    assert_eq!(run_ranges(&at_zero), [(8.0, "axyb".to_string())]);
    assert_eq!(caret_line(&at_zero), Some(8.0 + 7.0));
    send(
        &mut f,
        &ev(InputKind::ImeUpdate {
            text: "xy".into(),
            cursor_utf16: 2,
        }),
    );
    let at_two = draw(&f);
    assert_eq!(caret_line(&at_two), Some(8.0 + 21.0));
    assert_ne!(at_zero, at_two);
    assert!(
        at_two.commands.iter().any(|c| matches!(
            c,
            DrawCommand::Line { from, to, .. } if from.y == to.y && from.x.0 == 15.0 && to.x.0 == 29.0
        )),
        "the preview is underlined"
    );
    assert_eq!(f.text, "ab", "preview text is not committed");
}
#[test]
fn ime_cursor_counts_utf16_units() {
    let mut f = typed("");
    send(&mut f, &ev(InputKind::ImeStart));
    send(
        &mut f,
        &ev(InputKind::ImeUpdate {
            text: "😀b".into(),
            cursor_utf16: 2,
        }),
    );
    assert_eq!(caret_line(&draw(&f)), Some(8.0 + 7.0));
}
#[test]
fn blur_commits_a_preview_and_does_not_drop_it() {
    let mut f = typed("a");
    send(&mut f, &ev(InputKind::ImeStart));
    send(
        &mut f,
        &ev(InputKind::ImeUpdate {
            text: "か".into(),
            cursor_utf16: 1,
        }),
    );
    send(&mut f, &ev(InputKind::Blur));
    assert_eq!(f.text, "aか");
    assert_eq!(f.composition, None);
}
#[test]
fn ime_end_rejected_by_the_validator_is_visible_to_the_caller() {
    let mut f = typed("1");
    f.validator = Some(|s| s.chars().all(|c| c.is_ascii_digit()));
    send(&mut f, &ev(InputKind::ImeStart));
    send(&mut f, &ev(InputKind::ImeEnd { text: "か".into() }));
    assert_eq!(f.text, "1");
    assert_eq!(f.rejected_input.as_deref(), Some("か"));
    send(&mut f, &ev(InputKind::Char('2')));
    assert_eq!(f.rejected_input, None);
}

// Item 19
#[test]
fn copy_can_be_pasted_in_another_field() {
    let board: Arc<MemoryClipboard> = MemoryClipboard::shared();
    let mut one = typed("abc").with_clipboard(board.clone());
    let mut two = field().with_clipboard(board.clone());
    send(&mut one, &ctrl(0x41));
    send(&mut one, &ctrl(0x43));
    send(&mut two, &ctrl(0x56));
    assert_eq!(two.text, "abc");
}
#[test]
fn copy_with_no_selection_does_not_empty_the_clipboard() {
    let board = MemoryClipboard::shared();
    board.write_text("kept").unwrap();
    let mut f = typed("abc").with_clipboard(board.clone());
    send(&mut f, &ctrl(0x43));
    send(&mut f, &ctrl(0x58));
    assert_eq!(board.read_text().unwrap().as_deref(), Some("kept"));
}
#[test]
fn paste_turns_line_breaks_into_spaces() {
    let board = MemoryClipboard::shared();
    board.write_text("a\r\nb\nc").unwrap();
    let mut f = field().with_clipboard(board);
    send(&mut f, &ctrl(0x56));
    assert_eq!(f.text, "a b c");
}
#[derive(Debug)]
struct BrokenClipboard;
impl ClipboardAccess for BrokenClipboard {
    fn read_text(&self) -> Result<Option<String>, String> {
        Err("no clipboard here".into())
    }
    fn write_text(&self, _: &str) -> Result<(), String> {
        Err("no clipboard here".into())
    }
}
#[test]
fn a_clipboard_failure_is_kept_for_the_caller() {
    let mut f = typed("abc").with_clipboard(Arc::new(BrokenClipboard));
    send(&mut f, &ctrl(0x41));
    send(&mut f, &ctrl(0x58));
    assert_eq!(f.text, "abc", "a failed cut keeps the text");
    assert_eq!(f.clipboard_error.as_deref(), Some("no clipboard here"));
}

// Item 20
#[test]
fn ctrl_arrows_move_by_word() {
    let mut f = typed("one two  three");
    send(&mut f, &ctrl(0x25));
    assert_eq!(&f.text[f.caret..], "three");
    send(&mut f, &ctrl(0x25));
    assert_eq!(&f.text[f.caret..], "two  three");
    send(&mut f, &ctrl(0x27));
    assert_eq!(&f.text[f.caret..], "three");
    send(&mut f, &ctrl(0x27));
    assert_eq!(f.caret, f.text.len());
}
#[test]
fn ctrl_backspace_and_ctrl_delete_remove_a_word() {
    let mut f = typed("one two");
    send(&mut f, &ctrl(0x08));
    assert_eq!(f.text, "one ");
    send(&mut f, &press(0x24));
    send(&mut f, &ctrl(0x2e));
    assert_eq!(f.text, "");
}
#[test]
fn ctrl_y_redoes_and_shift_insert_pastes() {
    let board = MemoryClipboard::shared();
    board.write_text("Z").unwrap();
    let mut f = typed("a").with_clipboard(board);
    send(&mut f, &ctrl(0x5a));
    assert_eq!(f.text, "");
    send(&mut f, &ctrl(0x59));
    assert_eq!(f.text, "a");
    send(&mut f, &shift(0x2d));
    assert_eq!(f.text, "aZ");
    send(&mut f, &ev(InputKind::Char('q')));
    send(&mut f, &ctrl(0x5a));
    send(&mut f, &ctrl(0x59));
    assert_eq!(f.text, "aZq");
    send(&mut f, &ev(InputKind::Char('r')));
    send(&mut f, &ctrl(0x59));
    assert_eq!(f.text, "aZqr", "a new edit ends redo");
}
#[test]
fn altgr_fires_no_shortcut() {
    let altgr = Modifiers {
        ctrl: true,
        alt: true,
        ..Default::default()
    };
    let mut f = typed("abc");
    let caret = f.caret;
    let mut ctx = ViewContext::default();
    assert_eq!(f.handle(&key(0x41, altgr), &mut ctx), Handled::No);
    assert_eq!((f.anchor, f.caret), (caret, caret));
    f.handle(&ev(InputKind::Char('€')), &mut ctx);
    assert_eq!(f.text, "abc€");
}

// Item 21
fn walk<'a>(node: &'a Widget, out: &mut Vec<&'a Widget>) {
    out.push(node);
    for c in &node.children {
        walk(c, out);
    }
}
fn all_nodes(tree: &Widget) -> Vec<&Widget> {
    let mut out = vec![];
    walk(tree, &mut out);
    out
}
#[test]
fn keyboard_menu_reports_one_focused_item() {
    let mut menu = nested_menu();
    let tree = menu.widgets().unwrap();
    let focused = focused_nodes(&tree);
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].role, WidgetRole::MenuItem);
    assert_eq!(focused[0].name, "File");
    send(&mut menu, &press(0x27));
    let tree = menu.widgets().unwrap();
    let focused = focused_nodes(&tree);
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].name, "Mid");
}
#[test]
fn a_row_with_a_submenu_says_so_and_holds_the_child() {
    let mut menu = nested_menu();
    let closed = menu.widgets().unwrap();
    assert_eq!(closed.children[0].value.as_deref(), Some("Submenu"));
    assert!(!closed.children[0].states.expanded);
    send(&mut menu, &press(0x27));
    let tree = menu.widgets().unwrap();
    let file = &tree.children[0];
    assert!(file.states.expanded);
    assert_eq!(file.children.len(), 1);
    assert_eq!(file.children[0].role, WidgetRole::Menu);
    assert_eq!(
        file.children[0].name, "File",
        "the child is named after its row"
    );
}
#[test]
fn no_empty_shortcut_values() {
    let tree = MenuView::new(WidgetId(1), caret_menu()).widgets().unwrap();
    assert!(
        all_nodes(&tree)
            .iter()
            .all(|n| n.value.as_deref() != Some(""))
    );
}
#[test]
fn open_combo_has_list_children_with_one_focus() {
    let mut c = combo();
    assert!(c.widgets().unwrap().children.is_empty());
    send(&mut c, &press(0x73));
    send(&mut c, &press(0x28));
    let tree = c.widgets().unwrap();
    assert!(tree.states.expanded);
    let list = &tree.children[0];
    assert_eq!(list.children.len(), 5, "named rows only");
    assert_eq!(list.children[1].name, "Mac Roman");
    assert!(list.children[1].states.selected);
    let focused = focused_nodes(&tree);
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].name, "Mac Roman");
    c.disabled_items.insert(3);
    let tree = c.widgets().unwrap();
    assert!(!tree.children[0].children[2].states.enabled);
}
#[test]
fn table_exposes_headers_cells_row_count_and_sections() {
    let rows = vec![row("File", true), row("New", false)];
    let mut t =
        Table::new(WidgetId(1), vec!["Command".into(), "Shortcut".into()], rows).named("Shortcuts");
    t.layout(r(0.0, 0.0, 300.0, 200.0), &Measure);
    t.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    let tree = t.widgets().unwrap();
    assert_eq!(tree.name, "Shortcuts");
    assert_eq!(tree.value.as_deref(), Some("2 rows"));
    assert!(tree.children[0].states.read_only, "a section row says so");
    assert_eq!(tree.children[0].value.as_deref(), Some("Section"));
    let cells = &tree.children[1].children;
    assert_eq!(cells[0].role, WidgetRole::Cell);
    assert_eq!(cells[0].name, "Command", "a cell's name is its header");
    assert_eq!(cells[0].value.as_deref(), Some("New"));
    assert_eq!(cells[1].name, "Shortcut");
    assert_eq!(cells[1].value.as_deref(), Some("Ctrl+K"));
    let focused = focused_nodes(&tree);
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].name, "New");
}
#[test]
fn no_fixed_names() {
    let tabs = TabBar::new(WidgetId(1), vec!["A".into()]);
    assert_ne!(tabs.widgets().unwrap().name, "Settings tabs");
    assert_eq!(tabs.clone().named("Pages").widgets().unwrap().name, "Pages");
    let t = Table::new(WidgetId(2), vec![], vec![]);
    assert_ne!(t.widgets().unwrap().name, "Table");
    let m = MenuView::new(WidgetId(3), MenuModel::default());
    assert_ne!(m.widgets().unwrap().name, "Menu");
    assert_eq!(m.named("All menus").widgets().unwrap().name, "All menus");
}
#[test]
fn tabs_report_one_focus() {
    let mut tabs = TabBar::new(WidgetId(1), vec!["A".into(), "B".into()]);
    tabs.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    let tree = tabs.widgets().unwrap();
    let focused = focused_nodes(&tree);
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].role, WidgetRole::Tab);
}
#[test]
fn child_ids_do_not_collide_across_a_whole_dialog() {
    let mut frame = DialogFrame::new(WidgetId(1), "All");
    frame.bounds = r(0.0, 0.0, 800.0, 800.0);
    let rows: Vec<TableRow> = (0..20).map(|i| row(&format!("Row {i}"), false)).collect();
    let mut t = Table::new(WidgetId(2), vec!["A".into(), "B".into()], rows);
    t.bounds = r(0.0, 0.0, 400.0, 600.0);
    frame.add(Box::new(t));
    let mut tabs = TabBar::new(WidgetId(3), (0..20).map(|i| format!("T{i}")).collect());
    tabs.bounds = r(0.0, 600.0, 400.0, 30.0);
    frame.add(Box::new(tabs));
    let mut menu = MenuView::new(WidgetId(4), caret_menu());
    menu.bounds = r(400.0, 0.0, 300.0, 400.0);
    menu.open_keyboard();
    send(&mut menu, &press(0x27));
    frame.add(Box::new(menu));
    let mut c = combo();
    c.id = WidgetId(5);
    c.open = true;
    c.bounds = r(400.0, 420.0, 300.0, 32.0);
    frame.add(Box::new(c));
    frame.add(Box::new(EncodingListForIds::new()));
    let tree = frame.widgets().unwrap();
    assert!(all_nodes(&tree).len() > 100);
    assert_eq!(duplicate_ids(&tree), vec![]);
}
/// A check list beside the rest, for the id test.
struct EncodingListForIds(CheckList);
impl EncodingListForIds {
    fn new() -> Self {
        Self(CheckList::new(
            WidgetId(6),
            "Encodings",
            vec![("A".into(), true), ("B".into(), false)],
        ))
    }
}
impl View for EncodingListForIds {
    fn handle(&mut self, e: &InputEvent, c: &mut ViewContext) -> Handled {
        self.0.handle(e, c)
    }
    fn draw(&self, l: &mut DisplayList, t: &Theme, m: &dyn TextMeasurer) {
        self.0.draw(l, t, m)
    }
    fn layout(&mut self, b: Rect, m: &dyn TextMeasurer) {
        self.0.layout(b, m)
    }
    fn widgets(&self) -> Option<Widget> {
        self.0.widgets()
    }
}
impl Control for EncodingListForIds {}
#[test]
fn a_foreign_control_gets_the_default_focus_rules() {
    assert!(!EncodingListForIds::new().focusable(), "its role is Group");
    assert!(!EncodingListForIds::new().uses_key(0x0d));
}

// Item 22
#[test]
fn disabled_combo_and_field_look_different() {
    let mut c = combo();
    c.focused = false;
    let enabled = draw(&c);
    c.enabled = false;
    assert_ne!(enabled, draw(&c));
    let mut f = typed("abc");
    f.focused = false;
    let enabled = draw(&f);
    f.enabled = false;
    assert_ne!(enabled, draw(&f));
}
#[test]
fn checked_checkbox_shows_hover_and_pressed() {
    let mut cb = Checkbox::new(WidgetId(1), "x");
    cb.bounds = r(0.0, 0.0, 200.0, 32.0);
    cb.checked = true;
    let rest = draw(&cb);
    cb.hovered = true;
    let hover = draw(&cb);
    cb.pressed = true;
    let pressed = draw(&cb);
    assert_ne!(rest, hover);
    assert_ne!(hover, pressed);
}

// Item 23
#[test]
fn blur_during_a_press_releases_the_mouse() {
    let mut b = Button::new(WidgetId(1), "x");
    b.layout(r(0.0, 0.0, 100.0, 32.0), &Measure);
    send(&mut b, &mouse_down(5.0, 5.0));
    let blur = send(&mut b, &ev(InputKind::Blur));
    assert!(blur.requests.contains(&ViewRequest::CaptureMouse(false)));
    let up_after = send(&mut b, &mouse_up(5.0, 5.0));
    assert_eq!(b.clicks, 0);
    assert!(up_after.requests.is_empty());

    let mut cb = Checkbox::new(WidgetId(2), "x");
    cb.layout(r(0.0, 0.0, 100.0, 32.0), &Measure);
    send(&mut cb, &mouse_down(5.0, 5.0));
    let blur = send(&mut cb, &ev(InputKind::Blur));
    assert!(blur.requests.contains(&ViewRequest::CaptureMouse(false)));
    assert!(!cb.checked);
}
#[test]
fn checkbox_captures_and_repaints_on_press() {
    let mut cb = Checkbox::new(WidgetId(1), "x");
    cb.layout(r(0.0, 0.0, 100.0, 32.0), &Measure);
    let down = send(&mut cb, &mouse_down(5.0, 5.0));
    assert!(down.requests.contains(&ViewRequest::CaptureMouse(true)));
    assert!(repaints(&down));
    let released = send(&mut cb, &mouse_up(5.0, 5.0));
    assert!(
        released
            .requests
            .contains(&ViewRequest::CaptureMouse(false))
    );
    assert!(cb.checked);
}
#[test]
fn holding_space_flips_a_checkbox_once() {
    let mut cb = Checkbox::new(WidgetId(1), "x");
    cb.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    send(&mut cb, &press(0x20));
    send(&mut cb, &repeat(0x20));
    send(&mut cb, &repeat(0x20));
    assert!(cb.checked);
}
#[test]
fn a_button_disabled_mid_press_does_not_click() {
    let mut b = Button::new(WidgetId(1), "x");
    b.layout(r(0.0, 0.0, 100.0, 32.0), &Measure);
    send(&mut b, &mouse_down(5.0, 5.0));
    b.enabled = false;
    let released = send(&mut b, &mouse_up(5.0, 5.0));
    assert_eq!(b.clicks, 0);
    assert!(
        released
            .requests
            .contains(&ViewRequest::CaptureMouse(false))
    );
    assert!(!b.pressed);

    let mut b = Button::new(WidgetId(1), "x");
    b.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    send(&mut b, &press(0x20));
    b.enabled = false;
    send(&mut b, &up(0x20));
    assert_eq!(b.clicks, 0);
    assert!(!b.pressed, "the press ends even when disabled");
}
#[test]
fn moves_away_from_a_control_do_nothing() {
    let mut views: Vec<Box<dyn View>> = vec![
        Box::new(Button::new(WidgetId(1), "x")),
        Box::new(Checkbox::new(WidgetId(2), "x")),
        Box::new(ScrollBar::new(WidgetId(3), 500.0, 100.0)),
        Box::new(ComboBox::new(WidgetId(4), "x", vec![Some("A".into())])),
        Box::new(TabBar::new(WidgetId(5), vec!["A".into()])),
        Box::new(TextField::new(WidgetId(6), "x")),
    ];
    for view in &mut views {
        view.layout(r(0.0, 0.0, 100.0, 32.0), &Measure);
        let mut ctx = ViewContext::default();
        assert_eq!(
            view.handle(&mouse_move(500.0, 500.0), &mut ctx),
            Handled::No
        );
        assert!(ctx.requests.is_empty());
    }
}
#[test]
fn a_hover_change_repaints_once() {
    let mut b = Button::new(WidgetId(1), "x");
    b.layout(r(0.0, 0.0, 100.0, 32.0), &Measure);
    assert!(repaints(&send(&mut b, &mouse_move(5.0, 5.0))));
    assert!(!repaints(&send(&mut b, &mouse_move(6.0, 5.0))));
    assert!(repaints(&send(&mut b, &mouse_move(500.0, 5.0))));
}
#[test]
fn holding_enter_presses_a_button_once() {
    let mut b = Button::new(WidgetId(1), "x");
    b.handle(&ev(InputKind::Focus), &mut ViewContext::default());
    send(&mut b, &press(0x0d));
    send(&mut b, &repeat(0x0d));
    send(&mut b, &up(0x0d));
    send(&mut b, &up(0x0d));
    assert_eq!(b.clicks, 1);
}

// Item 24
#[test]
fn narrow_alert_keeps_three_buttons_inside_and_wraps_its_message() {
    let model = AlertModel::new(
        "Save?",
        "A long message that cannot fit on one line of a very narrow dialog frame at all.",
        vec!["Save".into(), "Cancel".into(), "Discard".into()],
    );
    let frame = DialogFrame::from_alert(WidgetId(1), &model, r(50.0, 0.0, 120.0, 150.0));
    let mut right = 50.0;
    for i in 1..=3 {
        let b = frame.child::<Button>(i).unwrap().bounds;
        assert!(b.x.0 >= 50.0, "{b:?}");
        assert!(b.x.0 >= right, "{b:?} overlaps the button before it");
        right = b.x.0 + b.width.0;
    }
    assert!(right <= 170.0);
    let label = frame.child::<Label>(0).unwrap();
    assert!(label.wrap);
    let list = draw(&frame);
    let lines = list
        .commands
        .iter()
        .filter(|c| matches!(c, DrawCommand::TextRun { text, .. } if text.contains(' ') || text.len() > 8))
        .count();
    assert!(lines >= 2, "{}", list.to_stable_text());
    assert!(
        list.commands
            .iter()
            .any(|c| matches!(c, DrawCommand::PushClip(r) if *r == label.bounds)),
        "the message is clipped to its area"
    );
}
#[test]
fn resizing_an_alert_moves_its_buttons() {
    let model = AlertModel::new("t", "m", vec!["OK".into()]);
    let mut frame = DialogFrame::from_alert(WidgetId(1), &model, r(0.0, 0.0, 400.0, 150.0));
    frame.layout(r(0.0, 0.0, 600.0, 300.0), &Measure);
    let b = frame.child::<Button>(1).unwrap().bounds;
    assert_eq!(b.x.0 + b.width.0, 592.0);
    assert_eq!(b.y.0 + b.height.0, 292.0);
}
#[test]
fn wrap_lines_breaks_at_spaces_and_inside_long_words() {
    let lines = wrap_lines("one two three", 7.0 * 8.0, &Measure, "x");
    assert_eq!(lines, ["one two", "three"]);
    let lines = wrap_lines("abcdefghij", 7.0 * 4.0, &Measure, "x");
    assert_eq!(lines, ["abcd", "efgh", "ij"]);
    let lines = wrap_lines("a\nb", 100.0, &Measure, "x");
    assert_eq!(lines, ["a", "b"]);
}

// Item 25: the caret list and the encoding panel.
fn commands_of(model: &MenuModel, out: &mut Vec<CommandId>) {
    for item in &model.items {
        if let Some(command) = item.command {
            out.push(command);
        }
        if let Some(sub) = &item.submenu {
            commands_of(sub, out);
        }
    }
}
#[test]
fn caret_list_has_every_command_once() {
    let mut found = vec![];
    commands_of(&caret_menu(), &mut found);
    for command in CommandId::ALL {
        assert_eq!(
            found.iter().filter(|c| *c == command).count(),
            1,
            "{command:?}"
        );
    }
    assert_eq!(found.len(), CommandId::ALL.len());
}
#[test]
fn caret_list_has_the_groups_in_order_and_no_mac_only_rows() {
    let model = caret_menu();
    let names: Vec<&str> = model.items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        names,
        [
            "File",
            "Edit",
            "Format",
            "View",
            "Window",
            "",
            "About QuickEdit",
            "Settings…",
            "Quit QuickEdit"
        ]
    );
    fn labels(model: &MenuModel, out: &mut Vec<String>) {
        for item in &model.items {
            out.push(item.label.clone());
            if let Some(sub) = &item.submenu {
                labels(sub, out);
            }
        }
    }
    let mut all = vec![];
    labels(&model, &mut all);
    for gone in [
        "Share",
        "Hide",
        "Show All",
        "Services",
        "Grammar",
        "Data Detectors",
        "Help",
        "Speech",
    ] {
        assert!(
            !all.iter().any(|l| l.contains(gone)),
            "{gone} must not appear"
        );
    }
    let edit = model.items[1].submenu.as_ref().unwrap();
    let groups: Vec<&str> = edit
        .items
        .iter()
        .filter(|i| i.submenu.is_some())
        .map(|i| i.label.as_str())
        .collect();
    assert_eq!(
        groups,
        ["Find", "Spelling", "Substitutions", "Transformations"]
    );
}
#[test]
fn caret_list_shows_windows_keys() {
    let model = caret_menu();
    let file = model.items[0].submenu.as_ref().unwrap();
    let key_of = |label: &str| {
        file.items
            .iter()
            .find(|i| i.label == label)
            .map(|i| i.key_text.clone())
            .unwrap()
    };
    assert_eq!(key_of("Save"), "Ctrl+S");
    assert_eq!(key_of("Duplicate"), "Ctrl+Shift+S");
    assert_eq!(key_of("Save As…"), "F12");
    assert_eq!(key_of("Rename…"), "");
    let view = model.items[3].submenu.as_ref().unwrap();
    assert!(view.items.iter().any(|i| i.key_text == "F11"));
}
#[test]
fn caret_list_runs_a_command_by_keyboard_alone() {
    let mut menu = MenuView::new(WidgetId(1), caret_menu());
    let (w, h) = caret_menu().preferred_size(&Measure);
    menu.layout(r(0.0, 0.0, w, h), &Measure);
    menu.open_keyboard();
    send(&mut menu, &ev(InputKind::Char('e')));
    assert_eq!(menu.selected, Some(1));
    assert!(menu.child.is_some(), "a group's mnemonic opens it");
    send(&mut menu, &press(0x23));
    send(&mut menu, &press(0x27));
    send(&mut menu, &press(0x28));
    let ctx = send(&mut menu, &press(0x0d));
    assert!(
        ctx.requests
            .contains(&ViewRequest::Run(CommandId::LowerCase))
    );
    assert!(menu.closed);
}
#[test]
fn set_command_state_reaches_nested_rows() {
    let mut model = caret_menu();
    assert!(set_command_state(
        &mut model,
        CommandId::SmartQuotes,
        true,
        true
    ));
    assert!(set_command_state(&mut model, CommandId::Undo, false, false));
    let edit = model.items[1].submenu.as_ref().unwrap();
    assert!(!edit.items[0].enabled);
    let subs = edit
        .items
        .iter()
        .find(|i| i.label == "Substitutions")
        .unwrap();
    let quotes = subs
        .submenu
        .as_ref()
        .unwrap()
        .items
        .iter()
        .find(|i| i.label == "Smart Quotes")
        .unwrap();
    assert!(quotes.checked);
}
fn panel() -> EncodingPanel {
    EncodingPanel::new(
        WidgetId(1),
        TextEncoding::DEFAULT_LIST,
        r(0.0, 0.0, ENCODING_PANEL_WIDTH, ENCODING_PANEL_HEIGHT),
    )
}
#[test]
fn encoding_panel_works_by_keyboard_alone() {
    let mut p = panel();
    let tree = p.widgets().unwrap();
    assert_eq!(tree.name, "Customize Encodings List");
    let focused = focused_nodes(&tree);
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].name, TextEncoding::ALL[0].name());
    assert_eq!(
        p.enabled_encodings().len(),
        TextEncoding::DEFAULT_LIST.len()
    );
    // UTF-8 is row 0 and starts on. Space turns it off.
    send(&mut p, &press(0x20));
    assert!(!p.enabled_encodings().contains(&TextEncoding::Utf8));
    send(&mut p, &press(0x20));
    assert!(p.enabled_encodings().contains(&TextEncoding::Utf8));
    // End reaches the last row and keeps it on screen.
    send(&mut p, &press(0x23));
    let list = p.frame.child::<CheckList>(0).unwrap();
    assert_eq!(list.current, TextEncoding::ALL.len() - 1);
    assert!(list.visible_range().contains(&list.current));
    // Tab reaches Select all, and Space presses it.
    send(&mut p, &press(0x09));
    send(&mut p, &press(0x20));
    send(&mut p, &up(0x20));
    assert_eq!(p.enabled_encodings().len(), TextEncoding::ALL.len());
    send(&mut p, &press(0x09));
    send(&mut p, &press(0x0d));
    send(&mut p, &up(0x0d));
    assert_eq!(p.enabled_encodings(), vec![]);
    send(&mut p, &press(0x09));
    send(&mut p, &press(0x20));
    send(&mut p, &up(0x20));
    let mut restored = p.enabled_encodings();
    let mut defaults = TextEncoding::DEFAULT_LIST.to_vec();
    restored.sort_by_key(|e| e.name());
    defaults.sort_by_key(|e| e.name());
    assert_eq!(restored, defaults);
    send(&mut p, &press(0x09));
    assert_eq!(p.frame.focus_index, 0, "Tab wraps to the list");
    assert!(!p.closed());
    send(&mut p, &press(0x1b));
    assert!(p.closed());
}
#[test]
fn encoding_panel_list_works_by_mouse_and_wheel() {
    let mut p = panel();
    let b = p.frame.child::<CheckList>(0).unwrap().bounds;
    send(&mut p, &wheel(20.0, 20.0, -120.0));
    assert_eq!(p.frame.child::<CheckList>(0).unwrap().first_visible, 3);
    send(&mut p, &wheel(20.0, 20.0, 120.0));
    send(&mut p, &mouse_down(b.x.0 + 12.0, b.y.0 + 30.0));
    send(&mut p, &mouse_up(b.x.0 + 12.0, b.y.0 + 30.0));
    assert!(!p.enabled_encodings().contains(&TextEncoding::Utf16));
}
#[test]
fn encoding_panel_buttons_work_by_mouse() {
    let mut p = panel();
    let clear = p.frame.child::<Button>(2).unwrap().bounds;
    send(&mut p, &mouse_down(clear.x.0 + 5.0, clear.y.0 + 5.0));
    send(&mut p, &mouse_up(clear.x.0 + 5.0, clear.y.0 + 5.0));
    assert_eq!(p.enabled_encodings(), vec![]);
    assert!(!p.closed(), "these buttons do not close the panel");
}
#[test]
fn encoding_panel_lays_out_again_when_resized() {
    let mut p = panel();
    p.layout(r(0.0, 0.0, 500.0, 400.0), &Measure);
    let list = p.frame.child::<CheckList>(0).unwrap();
    assert_eq!(list.bounds.width.0, 500.0 - 16.0);
    assert!(list.bounds.height.0 > 300.0);
}
