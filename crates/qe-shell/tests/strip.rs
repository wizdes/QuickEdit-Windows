use qe_api::testing::FixedTextMeasurer;
use qe_api::{
    Color, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind, Modifiers, MouseButton,
    Point, Rect, ReplayStep, Theme, ThemeKind, View, ViewContext, ViewRequest, WidgetId,
    WidgetRole, parse_replay, run_replay,
};
use qe_shell::{
    CONTENT_WIDGET, CaptionHit, ContrastColors, FrameMode, ShellEvent, ShellTokens, Tab, TabId,
    TabStrip, caption_hit, maximized_insets,
};
use std::cell::RefCell;
use std::collections::HashSet;

const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0d;
const VK_MENU: u16 = 0x12;
const VK_ESCAPE: u16 = 0x1b;
const VK_RIGHT: u16 = 0x27;

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
fn bounds(width: f32) -> Rect {
    Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(width),
        height: Dip(40.0),
    }
}
fn strip(count: usize, width: f32) -> TabStrip {
    let mut strip = TabStrip::new((0..count).map(|i| Tab::new(format!("Tab {i}"))).collect());
    strip.layout(bounds(width), &measure());
    strip
}
fn send(s: &mut TabStrip, kind: InputKind) -> (Handled, ViewContext) {
    let mut ctx = ViewContext::default();
    let handled = s.handle(&InputEvent { time_us: 0, kind }, &mut ctx);
    (handled, ctx)
}
fn down(s: &mut TabStrip, x: f32, y: f32) -> (Handled, ViewContext) {
    send(
        s,
        InputKind::MouseDown {
            at: point(x, y),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
    )
}
fn up(s: &mut TabStrip, x: f32, y: f32) -> (Handled, ViewContext) {
    send(
        s,
        InputKind::MouseUp {
            at: point(x, y),
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        },
    )
}
fn mv(s: &mut TabStrip, x: f32, y: f32) -> (Handled, ViewContext) {
    send(s, InputKind::MouseMove { at: point(x, y) })
}
fn key(s: &mut TabStrip, virtual_key: u16, modifiers: Modifiers) -> Handled {
    send(
        s,
        InputKind::KeyDown {
            virtual_key,
            modifiers,
            repeat: false,
        },
    )
    .0
}
fn key_up(s: &mut TabStrip, virtual_key: u16, modifiers: Modifiers) -> Handled {
    send(
        s,
        InputKind::KeyUp {
            virtual_key,
            modifiers,
        },
    )
    .0
}
fn draw(s: &TabStrip, kind: ThemeKind) -> DisplayList {
    let mut list = DisplayList::default();
    s.draw(&mut list, &Theme::new(kind), &measure());
    list
}
fn check_list(list: &DisplayList, name: &str) {
    let path = format!("{}/tests/lists/{name}", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("QE_SHELL_UPDATE_LISTS").is_some() {
        std::fs::write(&path, list.to_stable_text()).unwrap();
    }
    assert_eq!(
        list.to_stable_text(),
        std::fs::read_to_string(&path).unwrap(),
        "{path}"
    );
}

#[test]
fn widths_and_overflow() {
    for width in [500.0, 800.0, 1600.0] {
        for count in [1, 3, 10, 40] {
            let s = strip(count, width);
            assert_eq!(s.tab_rects().len(), count);
            assert!(s.tab_width() <= 240.0);
            assert!(s.tab_width() >= 80.0);
            assert_eq!(s.overflow(), count as f32 * 80.0 + 31.0 > width - 138.0);
            if !s.overflow() {
                assert!(s.plus_rect().x.0 >= s.tab_rects().last().unwrap().x.0 + s.tab_width());
            }
        }
    }
}

/// The window buttons at each scale. This test used to expect 8-dip resize zones inside
/// the client area, which review item 4 proved wrong: they took clicks from Close, the
/// first tab and the text area. Terminal keeps those zones in the frame outside.
#[test]
fn caption_buttons_and_edges_at_three_scales() {
    for scale in [1.0, 1.5, 2.0] {
        let w = 800.0 * scale;
        let s = strip(1, 800.0);
        let at = |x: f32, y: f32| caption_hit(point(x * scale, y * scale), scale, &s);
        assert_eq!(at(800.0 - 23.0, 20.0), CaptionHit::Close);
        assert_eq!(at(800.0 - 69.0, 20.0), CaptionHit::Maximize);
        assert_eq!(at(800.0 - 115.0, 20.0), CaptionHit::Minimize);
        // The last device pixel and the top row of the buttons still count.
        assert_eq!(
            caption_hit(point(w - 1.0, 0.0), scale, &s),
            CaptionHit::Close
        );
        assert_eq!(at(800.0 - 7.0, 20.0), CaptionHit::Close);
        assert_eq!(at(800.0 - 69.0, 0.0), CaptionHit::Maximize);
        // The top band resizes, and only above the tabs.
        assert_eq!(at(300.0, 1.0), CaptionHit::ResizeTop);
        assert_eq!(at(1.0, 1.0), CaptionHit::ResizeTop);
        assert_eq!(at(300.0, 7.5), CaptionHit::ResizeTop);
        // Nothing else in the client area resizes.
        assert_eq!(at(7.0, 20.0), CaptionHit::Tab(0));
        assert_eq!(at(300.0, 300.0), CaptionHit::None);
        assert_eq!(at(1.0, 300.0), CaptionHit::None);
        assert_eq!(at(799.0, 300.0), CaptionHit::None);
        assert_eq!(at(300.0, 599.0), CaptionHit::None);
        let plus = s.plus_rect();
        assert_eq!(at(plus.x.0 + 5.0, 12.0), CaptionHit::NewTab);
        assert_eq!(at(plus.x.0 + 5.0, 11.5), CaptionHit::Drag);
        assert_eq!(at(plus.x.0 + 5.0, 6.0), CaptionHit::ResizeTop);
    }
}

#[test]
fn maximized_has_no_top_band() {
    let mut s = strip(2, 800.0);
    s.set_maximized(true);
    assert_eq!(s.hit_test(point(5.0, 1.0)), CaptionHit::Tab(0));
    assert_eq!(s.hit_test(point(700.0, 1.0)), CaptionHit::Minimize);
    assert_eq!(s.hit_test(point(500.0, 1.0)), CaptionHit::Drag);
    assert_eq!(s.plus_rect().y.0, 4.0);
    assert_eq!(s.plus_rect().height.0, 24.0);
}

#[test]
fn reorder_close_and_last_tab() {
    let mut s = strip(3, 800.0);
    assert_eq!(s.active(), 0);
    s.reorder(s.tab_id(0).unwrap(), 2);
    assert_eq!(s.tabs()[2].title, "Tab 0");
    assert_eq!(s.active(), 2, "the active document moved with its tab");
    s.close(s.tab_id(2).unwrap());
    assert_eq!(s.active(), 1);
    s.close(s.tab_id(1).unwrap());
    assert_eq!(s.active(), 0);
    s.close(s.tab_id(0).unwrap());
    assert_eq!(s.tabs().len(), 1);
    assert_eq!(s.active(), 0);
}

#[test]
fn closing_left_of_active_keeps_the_document() {
    let mut s = strip(4, 800.0);
    s.select(s.tab_id(1).unwrap());
    s.take_events();
    s.close(s.tab_id(0).unwrap());
    assert_eq!(s.tabs()[s.active()].title, "Tab 1");
    assert_eq!(s.active(), 0);
    assert_eq!(s.take_events(), vec![]);
    s.close(s.tab_id(2).unwrap());
    assert_eq!(s.tabs()[s.active()].title, "Tab 1");
    assert_eq!(s.take_events(), vec![]);
    s.close(s.tab_id(0).unwrap());
    assert_eq!(s.tabs()[s.active()].title, "Tab 2");
    assert_eq!(
        s.take_events(),
        vec![ShellEvent::SelectTab(s.tab_id(0).unwrap())]
    );
}

#[test]
fn click_emits_request() {
    let mut s = strip(1, 800.0);
    let (x, y) = (s.plus_rect().x.0 + 10.0, 16.0);
    let (_, press) = down(&mut s, x, y);
    assert!(press.requests.contains(&ViewRequest::CaptureMouse(true)));
    assert!(s.take_events().is_empty(), "nothing acts on the press");
    let (_, release) = up(&mut s, x, y);
    assert!(release.requests.contains(&ViewRequest::CaptureMouse(false)));
    assert_eq!(s.take_events(), vec![ShellEvent::NewTab]);
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
            height: Dip(32.0),
            ..bounds(800.0)
        },
        &measure(),
    );
    let text = draw(&s, ThemeKind::Light).to_stable_text();
    assert!(text.contains("\"E923\""));
    assert_eq!(s.hit_test(point(500.0, 35.0)), CaptionHit::None);
}

#[test]
fn display_list_states() {
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        let mut s = strip(3, 800.0);
        s.tabs_mut()[0].dirty = true;
        let name = match kind {
            ThemeKind::Light => "light.txt",
            ThemeKind::Dark => "dark.txt",
        };
        check_list(&draw(&s, kind), name);
    }
}

/// This test had its own text format. It now uses the helpers in `qe-api`, and the
/// owner's side is real: a close request closes only because the test answers it.
#[test]
fn replay_drag_and_close() {
    for file in ["reorder.txt", "close.txt"] {
        let script = std::fs::read_to_string(format!(
            "{}/tests/replays/{file}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let steps: Vec<ReplayStep> = parse_replay(&script).unwrap();
        // The replays' points are for tabs 199.67 wide.
        let s = RefCell::new(strip(3, 768.0));
        let log = RefCell::new(Vec::new());
        run_replay(
            &steps,
            |event| {
                let mut s = s.borrow_mut();
                s.handle(event, &mut ViewContext::default());
                let mut pending = s.take_events();
                while !pending.is_empty() {
                    for e in pending {
                        log.borrow_mut().push(format!("{e:?}"));
                        if let ShellEvent::CloseTabRequested(id) = e {
                            s.close(id);
                        }
                    }
                    pending = s.take_events();
                }
            },
            |name| match name {
                "events" => Some(log.borrow().join("|")),
                "active" => Some(s.borrow().tabs()[s.borrow().active()].title.clone()),
                "count" => Some(s.borrow().tabs().len().to_string()),
                _ => None,
            },
        )
        .unwrap_or_else(|e| panic!("{file}: {e}"));
    }
}

#[test]
fn widget_tree_has_caption_and_scroll_buttons() {
    let regular = strip(3, 800.0).widgets().unwrap();
    assert_eq!(
        regular
            .children
            .iter()
            .filter(|w| w.role == WidgetRole::Button)
            .count(),
        4
    );
    assert!(
        regular
            .children
            .iter()
            .filter(|w| w.role == WidgetRole::Tab)
            .all(|tab| {
                tab.children
                    .iter()
                    .any(|child| child.role == WidgetRole::Button && child.name == "Close tab")
            })
    );
    let overflow = strip(40, 500.0).widgets().unwrap();
    assert_eq!(
        overflow
            .children
            .iter()
            .filter(|w| w.role == WidgetRole::Button)
            .count(),
        6
    );
}

/// Renamed from `keyboard_focus_and_new_tab`, which never touched a new tab. A strip
/// with no focus now takes no keys, so that Tab no longer enters it from nowhere.
#[test]
fn right_arrow_and_enter_select_the_next_tab() {
    let mut s = strip(2, 800.0);
    s.focus_strip();
    assert_eq!(key(&mut s, VK_RIGHT, Modifiers::default()), Handled::Yes);
    assert_eq!(key(&mut s, VK_RETURN, Modifiers::default()), Handled::Yes);
    assert_eq!(
        s.take_events(),
        vec![ShellEvent::SelectTab(s.tab_id(1).unwrap())]
    );
    assert_eq!(s.focused(), Some(CaptionHit::Tab(1)));
}

#[test]
fn strip_takes_keys_only_with_focus() {
    let mut s = strip(2, 800.0);
    for vk in [VK_TAB, VK_RETURN, VK_RIGHT, 0x41] {
        assert_eq!(key(&mut s, vk, Modifiers::default()), Handled::No);
    }
    assert_eq!(s.focused(), None);
    assert!(s.take_events().is_empty());
    // With focus, a chord still passes through: Alt+Left and Ctrl+Tab are not for it.
    s.focus_strip();
    assert_eq!(
        key(&mut s, VK_RIGHT, alt(Modifiers::default())),
        Handled::No
    );
    let ctrl = Modifiers {
        ctrl: true,
        ..Default::default()
    };
    assert_eq!(key(&mut s, VK_TAB, ctrl), Handled::No);
    assert_eq!(s.focused(), Some(CaptionHit::Tab(0)));
}

#[test]
fn tab_walks_every_stop_and_leaves_to_the_content() {
    let mut s = strip(2, 800.0);
    s.set_focused(Some(CaptionHit::Tab(0)));
    let mut seen = vec![s.focused().unwrap()];
    let mut ctx = ViewContext::default();
    loop {
        s.handle(
            &InputEvent {
                time_us: 0,
                kind: InputKind::KeyDown {
                    virtual_key: VK_TAB,
                    modifiers: Modifiers::default(),
                    repeat: false,
                },
            },
            &mut ctx,
        );
        match s.focused() {
            Some(hit) => seen.push(hit),
            None => break,
        }
    }
    assert_eq!(
        seen,
        vec![
            CaptionHit::Tab(0),
            CaptionHit::TabClose(0),
            CaptionHit::Tab(1),
            CaptionHit::TabClose(1),
            CaptionHit::NewTab,
        ]
    );
    assert_eq!(s.take_events(), vec![ShellEvent::FocusContent]);
    assert!(
        ctx.requests
            .contains(&ViewRequest::SetFocus(CONTENT_WIDGET))
    );
    // Shift+Tab from the first stop leaves too, and so does Escape.
    s.set_focused(Some(CaptionHit::Tab(0)));
    let shift = Modifiers {
        shift: true,
        ..Default::default()
    };
    assert_eq!(key(&mut s, VK_TAB, shift), Handled::Yes);
    assert_eq!(s.focused(), None);
    s.focus_strip();
    assert_eq!(key(&mut s, VK_ESCAPE, Modifiers::default()), Handled::Yes);
    assert_eq!(s.focused(), None);
}

#[test]
fn overflow_tab_stops_include_scroll_buttons_and_enter_on_close_asks() {
    let mut s = strip(40, 500.0);
    s.set_focused(Some(CaptionHit::ScrollLeft));
    key(&mut s, VK_TAB, Modifiers::default());
    assert_eq!(s.focused(), Some(CaptionHit::Tab(0)));
    key(&mut s, VK_TAB, Modifiers::default());
    assert_eq!(s.focused(), Some(CaptionHit::TabClose(0)));
    key(&mut s, VK_RETURN, Modifiers::default());
    assert_eq!(
        s.take_events(),
        vec![ShellEvent::CloseTabRequested(s.tab_id(0).unwrap())]
    );
    s.set_focused(Some(CaptionHit::Tab(39)));
    key(&mut s, VK_TAB, Modifiers::default());
    key(&mut s, VK_TAB, Modifiers::default());
    assert_eq!(s.focused(), Some(CaptionHit::ScrollRight));
}

#[test]
fn every_focused_part_reports_focus_in_the_widget_tree() {
    let mut s = strip(2, 800.0);
    s.set_focused(Some(CaptionHit::NewTab));
    let tree = s.widgets().unwrap();
    let focused: Vec<_> = tree.children.iter().filter(|w| w.states.focused).collect();
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].id, TabStrip::widget_id(CaptionHit::NewTab));
    s.set_focused(Some(CaptionHit::TabClose(1)));
    let tree = s.widgets().unwrap();
    let tab = tree.children.iter().find(|w| w.name == "Tab 1").unwrap();
    assert!(tab.children[0].states.focused);
}

#[test]
fn focus_stays_right_after_close_and_reorder() {
    let mut s = strip(4, 800.0);
    s.set_focused(Some(CaptionHit::Tab(3)));
    s.close(s.tab_id(0).unwrap());
    assert_eq!(s.focused(), Some(CaptionHit::Tab(2)));
    s.close(s.tab_id(2).unwrap());
    assert_eq!(s.focused(), Some(CaptionHit::Tab(1)));
    s.set_focused(Some(CaptionHit::TabClose(0)));
    s.reorder(s.tab_id(0).unwrap(), 1);
    assert_eq!(s.focused(), Some(CaptionHit::TabClose(1)));
    s.close(s.tab_id(1).unwrap());
    assert_eq!(s.focused(), Some(CaptionHit::Tab(0)));
    let mut wide = strip(40, 500.0);
    wide.set_focused(Some(CaptionHit::ScrollRight));
    wide.layout(bounds(4000.0), &measure());
    assert_eq!(wide.focused(), Some(CaptionHit::NewTab));
}

#[test]
fn widget_ids_never_collide() {
    let s = strip(901, 800.0);
    let tree = s.widgets().unwrap();
    let mut ids: HashSet<WidgetId> = HashSet::new();
    let mut count = 0;
    let mut add = |id: WidgetId| {
        count += 1;
        assert!(ids.insert(id), "{id:?} is used twice");
    };
    add(tree.id);
    for child in &tree.children {
        add(child.id);
        for inner in &child.children {
            add(inner.id);
        }
    }
    assert!(count > 1800);
}

#[test]
fn overflow_buttons_scroll_one_tab() {
    let mut s = strip(40, 500.0);
    let before = s.tab_rects()[0].x.0;
    let x = s.plus_rect().x.0 - 10.0;
    down(&mut s, x, 16.0);
    up(&mut s, x, 16.0);
    s.layout(bounds(500.0), &measure());
    assert_eq!(s.tab_rects()[0].x.0, before - s.tab_width());
}

#[test]
fn scroll_stops_at_the_last_tab() {
    let mut s = strip(40, 500.0);
    let x = s.plus_rect().x.0 - 10.0;
    for _ in 0..100 {
        down(&mut s, x, 16.0);
        up(&mut s, x, 16.0);
    }
    let last = s.tab_rects()[39];
    let end = s.plus_rect().x.0 - 20.0;
    assert!(last.x.0 + last.width.0 <= end, "the last tab fully shows");
    assert!(last.x.0 + 2.0 * last.width.0 > end, "and no further");
}

#[test]
fn widening_or_closing_brings_scrolled_tabs_back() {
    let mut s = strip(10, 500.0);
    for _ in 0..8 {
        s.handle(
            &InputEvent {
                time_us: 0,
                kind: InputKind::Wheel {
                    at: point(100.0, 20.0),
                    delta_x: 0.0,
                    delta_y: -1.0,
                    modifiers: Modifiers::default(),
                },
            },
            &mut ViewContext::default(),
        );
    }
    assert!(s.tab_rects()[0].x.0 < 0.0);
    s.layout(bounds(1600.0), &measure());
    assert_eq!(s.tab_rects()[0].x.0, 0.0, "widening");

    let mut s = strip(10, 500.0);
    s.select(s.tab_id(9).unwrap());
    while s.tabs().len() > 3 {
        s.close(s.tab_id(0).unwrap());
    }
    assert_eq!(s.tab_rects()[0].x.0, 0.0, "closing");
    assert!(!s.overflow());
}

#[test]
fn new_selected_and_focused_tabs_scroll_into_view() {
    let shows = |s: &TabStrip, i: usize| {
        let b = s.tab_rects()[i];
        b.x.0 >= 20.0 && b.x.0 + b.width.0 <= s.plus_rect().x.0 - 20.0
    };
    let mut s = strip(20, 500.0);
    s.add_tab(Tab::new("New"));
    assert!(shows(&s, 20), "new");
    s.select(s.tab_id(0).unwrap());
    assert!(shows(&s, 0), "selected left");
    s.select(s.tab_id(15).unwrap());
    assert!(shows(&s, 15), "selected right");
    s.set_focused(Some(CaptionHit::Tab(2)));
    assert!(shows(&s, 2), "focused");
    key(&mut s, VK_RIGHT, Modifiers::default());
    assert!(shows(&s, 3), "arrow");
    s.set_focused(Some(CaptionHit::Tab(19)));
    assert!(shows(&s, 19), "focused right");
}

#[test]
fn hidden_tabs_clip_at_the_left_scroll_button() {
    let mut s = strip(40, 500.0);
    s.select(s.tab_id(20).unwrap());
    let list = draw(&s, ThemeKind::Light);
    let clip = list
        .commands
        .iter()
        .find_map(|c| match c {
            DrawCommand::PushClip(b) => Some(*b),
            _ => None,
        })
        .unwrap();
    assert_eq!(clip.x.0, 20.0);
    assert_eq!(clip.x.0 + clip.width.0, s.plus_rect().x.0 - 20.0);
    // A point under the left button cannot reach a hidden tab.
    assert_eq!(s.hit_test(point(5.0, 20.0)), CaptionHit::ScrollLeft);
    assert_eq!(s.hit_test(point(5.0, 38.0)), CaptionHit::Drag);
}

#[test]
fn draw_and_widgets_work_before_any_layout() {
    let mut s = TabStrip::new(vec![Tab::new("A")]);
    s.add_tab(Tab::new("B"));
    s.add_tab(Tab::new("C"));
    assert_eq!(s.tab_rects().len(), 3);
    draw(&s, ThemeKind::Dark);
    assert!(s.widgets().is_some());
    s.close(s.tab_id(0).unwrap());
    assert_eq!(s.tab_rects().len(), 2);
    draw(&s, ThemeKind::Light);
    s.reorder(s.tab_id(0).unwrap(), 1);
    draw(&s, ThemeKind::Light);
    // A window width that arrives later still reveals the active tab.
    let mut many = TabStrip::new((0..30).map(|i| Tab::new(format!("{i}"))).collect());
    many.add_tab(Tab::new("New"));
    many.layout(bounds(500.0), &measure());
    let b = many.tab_rects()[30];
    assert!(b.x.0 >= 20.0 && b.x.0 + b.width.0 <= many.plus_rect().x.0 - 20.0);
}

#[test]
fn press_tab_and_release_on_its_close_zone_does_nothing() {
    let mut s = strip(3, 800.0);
    let b = s.tab_rects()[1];
    let close_x = b.x.0 + b.width.0 - 5.0;
    down(&mut s, b.x.0 + 20.0, 20.0);
    up(&mut s, close_x, 20.0);
    assert_eq!(s.take_events(), vec![]);
    assert_eq!(s.tabs().len(), 3);
    down(&mut s, close_x, 20.0);
    up(&mut s, close_x, 20.0);
    assert_eq!(
        s.take_events(),
        vec![ShellEvent::CloseTabRequested(s.tab_id(1).unwrap())]
    );
    assert_eq!(s.tabs().len(), 3, "the tab stays until the owner answers");
    s.close(s.tab_id(1).unwrap());
    assert_eq!(s.tabs().len(), 2);
}

#[test]
fn blur_and_lost_capture_cancel_a_drag() {
    for how in ["blur", "capture"] {
        let mut s = strip(3, 800.0);
        down(&mut s, 100.0, 20.0);
        if how == "blur" {
            let (_, ctx) = send(&mut s, InputKind::Blur);
            assert!(ctx.requests.contains(&ViewRequest::CaptureMouse(false)));
        } else {
            let mut ctx = ViewContext::default();
            s.capture_lost(&mut ctx);
            assert!(!ctx.requests.contains(&ViewRequest::CaptureMouse(false)));
        }
        mv(&mut s, 500.0, 20.0);
        mv(&mut s, 500.0, 200.0);
        up(&mut s, 500.0, 200.0);
        assert_eq!(s.take_events(), vec![], "{how}");
        assert_eq!(s.tabs()[0].title, "Tab 0");
        let rest = {
            let mut clean = strip(3, 800.0);
            clean.set_window_active(how != "blur");
            draw(&clean, ThemeKind::Light)
        };
        let after = {
            mv(&mut s, 0.0, 300.0);
            draw(&s, ThemeKind::Light)
        };
        assert_eq!(after, rest, "{how}: nothing still looks pressed");
    }
}

#[test]
fn tear_off_clears_press_and_uses_the_row_height() {
    let mut s = strip(3, 800.0);
    s.set_maximized(true);
    let clean = draw(&s, ThemeKind::Light);
    down(&mut s, 100.0, 20.0);
    mv(&mut s, 100.0, 20.0 + 33.0);
    assert_eq!(
        s.take_events(),
        vec![ShellEvent::TearOffTab {
            tab: s.tab_id(0).unwrap(),
            at: point(100.0, 53.0)
        }]
    );
    mv(&mut s, 100.0, 200.0);
    assert_eq!(draw(&s, ThemeKind::Light), clean, "no press remains");

    let mut restored = strip(3, 800.0);
    down(&mut restored, 100.0, 20.0);
    mv(&mut restored, 100.0, 20.0 + 33.0);
    assert_eq!(restored.take_events(), vec![], "40 tall when restored");
    mv(&mut restored, 100.0, 20.0 + 41.0);
    assert_eq!(restored.take_events().len(), 1);
}

#[test]
fn caption_acts_on_release_over_the_same_button() {
    let mut s = strip(1, 800.0);
    down(&mut s, 790.0, 20.0);
    assert_eq!(s.take_events(), vec![], "not on press");
    up(&mut s, 790.0, 20.0);
    assert_eq!(
        s.take_events(),
        vec![ShellEvent::CaptionCommand(CaptionHit::Close)]
    );
    down(&mut s, 790.0, 20.0);
    mv(&mut s, 700.0, 20.0);
    up(&mut s, 700.0, 20.0);
    assert_eq!(s.take_events(), vec![], "released on another button");
    down(&mut s, 790.0, 20.0);
    up(&mut s, 400.0, 300.0);
    assert_eq!(s.take_events(), vec![], "released away");
}

fn fills_at(list: &DisplayList, x: f32) -> Vec<Color> {
    list.commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::FillRect { rect, color } if rect.x.0 == x && rect.width.0 == 46.0 => {
                Some(*color)
            }
            _ => None,
        })
        .collect()
}
#[test]
fn caption_shows_hover_and_pressed_colors() {
    let t = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
    let mut s = strip(1, 800.0);
    let close_x = 800.0 - 46.0;
    assert!(fills_at(&draw(&s, ThemeKind::Light), close_x).is_empty());
    mv(&mut s, 790.0, 20.0);
    assert_eq!(
        fills_at(&draw(&s, ThemeKind::Light), close_x),
        vec![t.close_hover]
    );
    down(&mut s, 790.0, 20.0);
    assert_eq!(
        fills_at(&draw(&s, ThemeKind::Light), close_x),
        vec![t.close_pressed]
    );
    mv(&mut s, 400.0, 300.0);
    assert!(fills_at(&draw(&s, ThemeKind::Light), close_x).is_empty());
    mv(&mut s, 790.0, 20.0);
    assert_eq!(
        fills_at(&draw(&s, ThemeKind::Light), close_x),
        vec![t.close_pressed]
    );
    let max_x = 800.0 - 92.0;
    up(&mut s, 790.0, 20.0);
    s.take_events();
    mv(&mut s, 720.0, 20.0);
    assert_eq!(fills_at(&draw(&s, ThemeKind::Light), max_x), vec![t.hover]);
    down(&mut s, 720.0, 20.0);
    assert_eq!(
        fills_at(&draw(&s, ThemeKind::Light), max_x),
        vec![t.pressed]
    );
}

#[test]
fn hover_clears_when_the_pointer_leaves() {
    let mut s = strip(1, 800.0);
    let clean = draw(&s, ThemeKind::Light);
    mv(&mut s, 790.0, 20.0);
    assert_ne!(draw(&s, ThemeKind::Light), clean);
    let mut ctx = ViewContext::default();
    s.pointer_left(&mut ctx);
    assert_eq!(draw(&s, ThemeKind::Light), clean);
    assert_eq!(ctx.requests.len(), 1, "only the one button repaints");
}

#[test]
fn mouse_move_invalidates_only_what_changed() {
    let mut s = strip(3, 800.0);
    let (_, first) = mv(&mut s, 100.0, 20.0);
    assert!(!first.requests.is_empty());
    let (_, same) = mv(&mut s, 101.0, 21.0);
    assert!(same.requests.is_empty(), "same part: nothing to do");
    let (_, next) = mv(&mut s, 790.0, 20.0);
    for request in next.requests {
        let ViewRequest::Invalidate(b) = request else {
            panic!("{request:?}");
        };
        assert!(b.width.0 < 800.0, "never the whole row");
    }
}

fn alt(m: Modifiers) -> Modifiers {
    Modifiers { alt: true, ..m }
}
fn opened(s: &mut TabStrip) -> bool {
    !s.take_events().is_empty()
}
#[test]
fn alt_and_its_chords_ask_the_row_for_nothing() {
    let mut s = strip(1, 800.0);
    let none = Modifiers::default();
    key(&mut s, VK_MENU, alt(none));
    assert!(!opened(&mut s), "not on key down");
    key_up(&mut s, VK_MENU, none);
    assert!(!opened(&mut s), "Alt alone belongs to the menu bar");
    key_up(&mut s, VK_MENU, none);
    assert!(!opened(&mut s), "a second release does nothing");
    for chord in [0x73, VK_TAB, b'F' as u16] {
        key(&mut s, VK_MENU, alt(none));
        key(&mut s, chord, alt(none));
        key_up(&mut s, VK_MENU, none);
        assert!(!opened(&mut s), "Alt+{chord:#x} asks for nothing");
    }
    let ctrl = Modifiers { ctrl: true, ..none };
    key(&mut s, 0xa5, alt(ctrl));
    key_up(&mut s, 0xa5, none);
    assert!(!opened(&mut s), "AltGr asks for nothing");
    let shift = Modifiers {
        shift: true,
        ..none
    };
    key(&mut s, VK_MENU, alt(shift));
    key_up(&mut s, VK_MENU, shift);
    assert!(!opened(&mut s), "Shift+Alt asks for nothing");
}

#[test]
fn wheel_works_only_over_the_strip() {
    let wheel = |y: f32, delta_y: f32| InputKind::Wheel {
        at: point(100.0, y),
        delta_x: 0.0,
        delta_y,
        modifiers: Modifiers::default(),
    };
    let mut s = strip(40, 500.0);
    let before = s.tab_rects()[0].x.0;
    assert_eq!(send(&mut s, wheel(500.0, -1.0)).0, Handled::No);
    assert_eq!(s.tab_rects()[0].x.0, before);
    assert_eq!(send(&mut s, wheel(20.0, -1.0)).0, Handled::Yes);
    assert_eq!(s.tab_rects()[0].x.0, before - s.tab_width());
    assert_eq!(send(&mut s, wheel(20.0, 1.0)).0, Handled::Yes);
    assert_eq!(s.tab_rects()[0].x.0, before);
    // Small steps add up to one tab.
    send(&mut s, wheel(20.0, -0.5));
    assert_eq!(s.tab_rects()[0].x.0, before);
    send(&mut s, wheel(20.0, -0.5));
    assert_eq!(s.tab_rects()[0].x.0, before - s.tab_width());
}

fn contrast() -> ContrastColors {
    ContrastColors {
        window: Color::rgb(1, 2, 3),
        window_text: Color::rgb(250, 251, 252),
        highlight: Color::rgb(26, 235, 255),
        highlight_text: Color::rgb(4, 5, 6),
        gray_text: Color::rgb(63, 242, 63),
    }
}
#[test]
fn high_contrast_tells_focus_active_and_hover_apart_with_system_colors() {
    let c = contrast();
    let base = || {
        let mut s = strip(3, 800.0);
        s.set_high_contrast(Some(c));
        s
    };
    let rest = draw(&base(), ThemeKind::Dark);
    let mut focus = base();
    focus.set_focused(Some(CaptionHit::Tab(1)));
    let focus = draw(&focus, ThemeKind::Dark);
    let mut focus_active = base();
    focus_active.set_focused(Some(CaptionHit::Tab(0)));
    let focus_active = draw(&focus_active, ThemeKind::Dark);
    let mut hover = base();
    mv(&mut hover, 300.0, 20.0);
    let hover = draw(&hover, ThemeKind::Dark);
    assert_ne!(rest, focus);
    assert_ne!(rest, focus_active);
    assert_ne!(rest, hover);
    assert_ne!(focus, hover);
    assert_ne!(focus, focus_active);

    let strokes = |l: &DisplayList| {
        l.commands
            .iter()
            .filter(|c| matches!(c, DrawCommand::StrokeRect { .. }))
            .count()
    };
    assert_eq!(strokes(&rest), 0, "no outline on every tab");
    assert_eq!(strokes(&focus), 1);

    let selected = rest.commands.iter().any(|command| {
        matches!(command, DrawCommand::RoundedRect { color, .. } if *color == c.highlight)
    });
    assert!(selected, "the active tab fills with the highlight color");

    let mut close = base();
    mv(&mut close, 790.0, 20.0);
    let close = draw(&close, ThemeKind::Dark);
    assert!(close.commands.iter().any(
        |command| matches!(command, DrawCommand::FillRect { rect, color } if rect.width.0 == 46.0 && *color == c.highlight)
    ));
    let text = close.to_stable_text();
    assert!(!text.contains("#c42b1c"), "no hardcoded red");
    assert!(!text.contains("#ffffffff"), "no hardcoded white");
}

#[test]
fn strip_fills_only_its_row_and_the_active_tab_joins_the_text_area() {
    let theme = Theme::new(ThemeKind::Light);
    let s = strip(3, 800.0);
    let list = draw(&s, ThemeKind::Light);
    assert_eq!(
        list.commands[0],
        DrawCommand::FillRect {
            rect: bounds(800.0),
            color: theme.tab_background
        }
    );
    let DrawCommand::RoundedRect { rect, color, .. } = &list.commands[2] else {
        panic!("{:?}", list.commands[2]);
    };
    assert_eq!(*color, theme.text_background);
    assert!(rect.y.0 + rect.height.0 >= 40.0);
    // No rule and no separator runs across the bottom of the row.
    for command in &list.commands {
        if let DrawCommand::FillRect { rect, color } = command
            && *color == theme.border
        {
            assert_eq!(rect.width.0, 1.0);
        }
    }
}

#[test]
fn plus_and_scroll_buttons_are_24_tall_on_the_tab_center() {
    let mut s = strip(40, 500.0);
    assert_eq!(s.plus_rect().y.0, 12.0);
    assert_eq!(s.plus_rect().height.0, 24.0);
    let tree = s.widgets().unwrap();
    for name in ["Scroll tabs left", "Scroll tabs right", "New tab"] {
        let w = tree.children.iter().find(|w| w.name == name).unwrap();
        assert_eq!((w.bounds.y.0, w.bounds.height.0), (12.0, 24.0), "{name}");
    }
    s.set_maximized(true);
    assert_eq!(s.plus_rect().y.0, 4.0);
    let tree = s.widgets().unwrap();
    let w = tree
        .children
        .iter()
        .find(|w| w.name == "Scroll tabs left")
        .unwrap();
    assert_eq!(w.bounds.y.0, 4.0);
}

#[test]
fn full_screen_uses_one_mode_for_drawing_and_hit_tests() {
    let mut s = strip(1, 800.0);
    s.set_frame_mode(FrameMode::FullScreen);
    assert_eq!(s.row_height(), 32.0);
    assert_eq!(s.hit_test(point(790.0, 36.0)), CaptionHit::None);
    assert_eq!(s.hit_test(point(790.0, 20.0)), CaptionHit::Close);
    assert!(
        draw(&s, ThemeKind::Light)
            .to_stable_text()
            .contains("\"E923\"")
    );
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
            let x = s.plus_rect().x.0 + 10.0;
            match state {
                "hover" => {
                    mv(&mut s, x, 16.0);
                }
                "pressed" => {
                    down(&mut s, x, 16.0);
                }
                "inactive" => s.set_window_active(false),
                "focus" => s.set_focused(Some(CaptionHit::NewTab)),
                "contrast" => s.set_high_contrast(Some(contrast())),
                "maximized" => {
                    s.set_maximized(true);
                    s.layout(
                        Rect {
                            height: Dip(32.0),
                            ..bounds(800.0)
                        },
                        &measure(),
                    );
                }
                _ => {}
            }
            let theme = match kind {
                ThemeKind::Light => "light",
                ThemeKind::Dark => "dark",
            };
            check_list(&draw(&s, kind), &format!("{theme}-{state}.txt"));
        }
    }
}

#[test]
fn press_changes_the_plus_without_prior_move() {
    let mut s = strip(1, 800.0);
    let before = draw(&s, ThemeKind::Light);
    let x = s.plus_rect().x.0 + 10.0;
    down(&mut s, x, 16.0);
    assert_ne!(before, draw(&s, ThemeKind::Light));
}

#[test]
fn dirty_dot_uses_text_font() {
    let mut s = strip(1, 800.0);
    s.tabs_mut()[0].dirty = true;
    let list = draw(&s, ThemeKind::Light);
    assert!(list.commands.iter().any(|command| matches!(command, DrawCommand::TextRun { text, font, .. } if text == "•" && font == "Segoe UI")));
}

/// This test used to expect the token `Segoe Fluent Icons;10px`, which the real renderer
/// does not know, so that the window was blank. Sizes now travel in `font_size`.
#[test]
fn caption_icons_request_ten_pixel_font() {
    let s = strip(1, 800.0);
    let list = draw(&s, ThemeKind::Light);
    let sizes: Vec<(String, f32)> = list
        .commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::Icon {
                icon_id, font_size, ..
            } => Some((icon_id.clone(), font_size.0)),
            _ => None,
        })
        .collect();
    for (id, size) in [
        ("E921", 10.0),
        ("E922", 10.0),
        ("E8BB", 10.0),
        ("E710", 12.0),
    ] {
        assert!(sizes.contains(&(id.to_string(), size)), "{id} at {size}");
    }
    for command in &list.commands {
        if let DrawCommand::TextRun {
            font, font_size, ..
        } = command
        {
            assert_eq!(font, "Segoe UI", "a family name only");
            assert_eq!(font_size.0, 13.0);
        }
    }
}

#[test]
fn tokens_match_the_design_spec_and_the_theme() {
    let theme = Theme::new(ThemeKind::Light);
    let t = ShellTokens::from_theme(&theme);
    assert_eq!(t.row_height, 40.0);
    assert_eq!(t.max_row_height, 32.0);
    assert_eq!(t.tab_height, theme.tab_height.0);
    assert_eq!(t.caption_width, theme.caption_button_width.0);
    assert_eq!(t.row, theme.tab_background);
    assert_eq!(t.active, theme.text_background);
    assert_eq!(t.plus_width, 31.0);
    assert_eq!(t.plus_height, 24.0);
}

#[test]
fn a_click_outside_the_strip_takes_keyboard_focus_away() {
    let mut s = strip(2, 800.0);
    s.focus_strip();
    assert_eq!(down(&mut s, 400.0, 300.0).0, Handled::No);
    assert_eq!(s.focused(), None);
    assert_eq!(s.take_events(), vec![ShellEvent::FocusContent]);
    up(&mut s, 400.0, 300.0);
    assert_eq!(
        key(&mut s, 0x20, Modifiers::default()),
        Handled::No,
        "Space"
    );
    assert_eq!(s.take_events(), vec![], "Space selected nothing");
}

#[test]
fn a_late_answer_cannot_close_or_move_the_wrong_tab() {
    let mut s = strip(3, 800.0);
    let (first, second) = (s.tab_id(0).unwrap(), s.tab_id(1).unwrap());
    let x = s.tab_rects()[1].x.0 + s.tab_width() - 5.0;
    down(&mut s, x, 20.0);
    up(&mut s, x, 20.0);
    assert_eq!(s.take_events(), vec![ShellEvent::CloseTabRequested(second)]);
    // While the owner asks "save changes?", another tab closes, and positions shift.
    s.close(first);
    assert_eq!(s.index_of(second), Some(0));
    s.close(second);
    assert_eq!(s.index_of(second), None);
    assert_eq!(s.tabs().len(), 1);
    assert_eq!(s.tabs()[0].title, "Tab 2", "the right tab went");
    // A second answer finds nothing to do.
    s.close(second);
    s.select(second);
    s.reorder(second, 0);
    assert_eq!(s.tabs()[0].title, "Tab 2");
    let id = s.add_tab(Tab::new("New"));
    assert_eq!(s.tab_id(1), Some(id));
    assert_ne!(id, TabId(0));
    let last = s.tab_id(0).unwrap();
    s.close(id);
    s.close(last);
    assert_ne!(
        s.tab_id(0),
        Some(last),
        "the blank tab that replaces the last is new"
    );
}

#[test]
fn changing_tabs_during_a_press_asks_to_release_the_mouse() {
    let mut s = strip(3, 800.0);
    assert!(!s.take_capture_release());
    down(&mut s, 100.0, 20.0);
    s.add_tab(Tab::new("New"));
    assert!(s.take_capture_release(), "add");
    assert!(!s.take_capture_release(), "once");
    down(&mut s, 100.0, 20.0);
    s.close(s.tab_id(2).unwrap());
    assert!(s.take_capture_release(), "close");
    // With no press, nothing is asked.
    s.add_tab(Tab::new("Other"));
    assert!(!s.take_capture_release());
}

#[test]
fn a_dragged_tab_reaches_a_tab_that_is_scrolled_off() {
    let mut s = strip(20, 500.0);
    let id = s.tab_id(0).unwrap();
    let x = s.tab_rects()[0].x.0 + 10.0;
    down(&mut s, x, 20.0);
    let past = s.plus_rect().x.0 - 5.0;
    for _ in 0..19 {
        mv(&mut s, past, 20.0);
    }
    assert_eq!(s.index_of(id), Some(19));
    up(&mut s, past, 20.0);
    let b = s.tab_rects()[19];
    assert!(
        b.x.0 + b.width.0 <= s.plus_rect().x.0 - 20.0,
        "it stays in view"
    );
    // And back to the start, over the left scroll button.
    let x = s.tab_rects()[19].x.0 + 10.0;
    down(&mut s, x, 20.0);
    for _ in 0..19 {
        mv(&mut s, 5.0, 20.0);
    }
    assert_eq!(s.index_of(id), Some(0));
}

#[test]
fn hover_follows_the_tab_under_the_pointer_after_a_wheel_scroll() {
    let mut s = strip(40, 500.0);
    let (x, y) = (s.tab_rects()[1].x.0 + 10.0, 20.0);
    mv(&mut s, x, y);
    let wheel = InputKind::Wheel {
        at: point(x, y),
        delta_x: 0.0,
        delta_y: -1.0,
        modifiers: Modifiers::default(),
    };
    send(&mut s, wheel);
    assert_eq!(s.hit_test(point(x, y)), CaptionHit::Tab(2));
    let after = draw(&s, ThemeKind::Light);
    let mut fresh = strip(40, 500.0);
    send(
        &mut fresh,
        InputKind::Wheel {
            at: point(x, y),
            delta_x: 0.0,
            delta_y: -1.0,
            modifiers: Modifiers::default(),
        },
    );
    mv(&mut fresh, x, y);
    assert_eq!(
        after,
        draw(&fresh, ThemeKind::Light),
        "the same as moving there"
    );
}

#[test]
fn an_ellipsis_never_splits_what_shows_as_one_character() {
    // Each character is 7 wide, so that the cut lands inside each cluster in turn.
    let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
    let flag = "\u{1f1fa}\u{1f1f8}";
    let tone = "\u{1f44d}\u{1f3fd}";
    let accent = "e\u{301}";
    for (name, whole) in [
        ("family", family),
        ("flag", flag),
        ("tone", tone),
        ("accent", accent),
    ] {
        let title = whole.repeat(40);
        for width in [
            300.0, 320.0, 340.0, 360.0, 380.0, 400.0, 420.0, 440.0, 460.0, 480.0,
        ] {
            let mut s = TabStrip::new(vec![Tab::new(title.clone())]);
            s.layout(bounds(width + 201.0), &measure());
            let list = draw(&s, ThemeKind::Light);
            let shown = list
                .commands
                .iter()
                .find_map(|c| match c {
                    DrawCommand::TextRun { text, .. } if text.ends_with('…') => {
                        Some(text.trim_end_matches('…').to_string())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{name}: no ellipsis at {width}"));
            assert_eq!(shown.len() % whole.len(), 0, "{name} at {width}: {shown:?}");
        }
    }
}

#[test]
fn full_screen_row_never_gives_clicks_to_the_text_area() {
    let mut s = strip(1, 800.0);
    s.set_frame_mode(FrameMode::FullScreen);
    assert_eq!(s.hit_test(point(500.0, 20.0)), CaptionHit::Drag);
    assert_eq!(down(&mut s, 500.0, 20.0).0, Handled::Yes);
    assert_eq!(up(&mut s, 500.0, 20.0).0, Handled::Yes);
    assert_eq!(down(&mut s, 500.0, 40.0).0, Handled::No, "under the row");
    assert_eq!(up(&mut s, 500.0, 40.0).0, Handled::No);
}

#[test]
fn other_buttons_on_the_row_stay_out_of_the_text_area() {
    let mut s = strip(1, 800.0);
    for button in [MouseButton::Right, MouseButton::Middle] {
        for (y, expected) in [(20.0, Handled::Yes), (300.0, Handled::No)] {
            let at = point(100.0, y);
            let modifiers = Modifiers::default();
            assert_eq!(
                send(
                    &mut s,
                    InputKind::MouseDown {
                        at,
                        button,
                        modifiers
                    }
                )
                .0,
                expected
            );
            assert_eq!(
                send(
                    &mut s,
                    InputKind::MouseUp {
                        at,
                        button,
                        modifiers
                    }
                )
                .0,
                expected
            );
        }
    }
    assert_eq!(s.take_events(), vec![]);
}

#[test]
fn the_quick_tab_lookup_agrees_with_a_full_scan() {
    for (count, width) in [(1, 800.0), (3, 800.0), (40, 500.0), (10_000, 900.0)] {
        for scrolled in [None, Some(count / 2), Some(count - 1)] {
            let mut s = strip(count, width);
            if let Some(i) = scrolled {
                s.select(s.tab_id(i).unwrap());
            }
            let rects = s.tab_rects().to_vec();
            let plus = s.plus_rect().x.0;
            let left = if s.overflow() { 20.0 } else { 0.0 };
            let right = if s.overflow() { plus - 20.0 } else { plus };
            let mut x = left;
            while x < right {
                let expected = rects
                    .iter()
                    .position(|b| x >= b.x.0 && x < b.x.0 + b.width.0)
                    .map_or(CaptionHit::Drag, |i| {
                        if x >= rects[i].x.0 + rects[i].width.0 - 32.0 {
                            CaptionHit::TabClose(i)
                        } else {
                            CaptionHit::Tab(i)
                        }
                    });
                assert_eq!(s.hit_test(point(x, 38.0)), expected, "{count} at {x}");
                x += 0.37;
            }
        }
    }
}

#[test]
fn the_strip_has_no_built_in_shortcut() {
    let mut s = strip(1, 800.0);
    let ctrl = Modifiers {
        ctrl: true,
        ..Default::default()
    };
    assert_eq!(key(&mut s, 0x54, ctrl), Handled::No);
    assert_eq!(
        s.take_events(),
        vec![],
        "Ctrl+T lives in the window's key table"
    );
}

fn icons(list: &DisplayList) -> Vec<String> {
    list.commands
        .iter()
        .filter_map(|c| match c {
            DrawCommand::Icon { icon_id, .. } => Some(icon_id.clone()),
            _ => None,
        })
        .collect()
}
#[test]
fn high_contrast_uses_terminals_own_window_button_glyphs() {
    let mut s = strip(1, 800.0);
    s.set_high_contrast(Some(contrast()));
    let found = icons(&draw(&s, ThemeKind::Dark));
    for id in ["EF2D", "EF2E", "EF2C"] {
        assert!(found.contains(&id.to_string()), "{id}: {found:?}");
    }
    s.set_maximized(true);
    assert!(icons(&draw(&s, ThemeKind::Dark)).contains(&"EF2F".to_string()));
    s.set_high_contrast(None);
    let normal = icons(&draw(&s, ThemeKind::Dark));
    assert!(normal.contains(&"E923".to_string()));
    assert!(!normal.contains(&"EF2F".to_string()));
}

#[test]
fn the_plus_is_at_the_center_of_its_box() {
    let s = strip(1, 800.0);
    let list = draw(&s, ThemeKind::Light);
    let at = list
        .commands
        .iter()
        .find_map(|c| match c {
            DrawCommand::Icon { icon_id, at, .. } if icon_id == "E710" => Some(at.x.0),
            _ => None,
        })
        .unwrap();
    // The fixed measurer gives one character 7 wide, and starts round to whole pixels.
    assert_eq!(at, (s.plus_rect().x.0 + 15.5 - 3.5).round());
}

#[test]
fn the_plus_hover_is_one_rounded_fill_of_its_whole_box() {
    let fills = |s: &TabStrip| -> Vec<Rect> {
        draw(s, ThemeKind::Light)
            .commands
            .iter()
            .filter_map(|c| match c {
                DrawCommand::RoundedRect { rect, .. } if rect.width.0 == 31.0 => Some(*rect),
                _ => None,
            })
            .collect()
    };
    let mut s = strip(1, 800.0);
    let plus = s.plus_rect();
    assert!(fills(&s).is_empty());
    mv(&mut s, plus.x.0 + 5.0, 20.0);
    assert_eq!(fills(&s), vec![plus]);
}

// ---- The top row is tabs, then +, then the window buttons.

#[test]
fn the_plus_sits_right_after_the_last_tab_and_the_row_past_it_drags() {
    for (count, width) in [(1, 800.0), (3, 800.0), (5, 1600.0), (4, 500.0)] {
        let s = strip(count, width);
        assert!(!s.overflow(), "{count} tabs in {width}");
        let last = *s.tab_rects().last().unwrap();
        let plus = s.plus_rect();
        assert_eq!(plus.x.0, last.x.0 + last.width.0, "{count} tabs in {width}");
        assert_eq!(plus.width.0, 31.0, "only the + has a place");
        let y = plus.y.0 + plus.height.0 / 2.0;
        assert_eq!(s.hit_test(point(plus.x.0 + 30.5, y)), CaptionHit::NewTab);
        let minimize = width - 138.0;
        let mut x = plus.x.0 + 31.0;
        while x < minimize {
            assert_eq!(s.hit_test(point(x, y)), CaptionHit::Drag, "x {x}");
            x += 1.0;
        }
    }
}

#[test]
fn with_overflow_the_plus_touches_the_window_buttons_and_nothing_overlaps() {
    let s = strip(40, 500.0);
    assert!(s.overflow());
    let plus = s.plus_rect();
    assert_eq!(plus.x.0 + plus.width.0, 500.0 - 138.0);
    let y = plus.y.0 + 12.0;
    assert_eq!(s.hit_test(point(plus.x.0 + 1.0, y)), CaptionHit::NewTab);
    assert_eq!(
        s.hit_test(point(plus.x.0 - 1.0, y)),
        CaptionHit::ScrollRight
    );
    assert_eq!(s.hit_test(point(500.0 - 137.0, y)), CaptionHit::Minimize);
}

#[test]
fn the_row_has_no_down_caret_in_any_state() {
    let plain = strip(2, 800.0);
    let mut hovered = strip(2, 800.0);
    let plus = hovered.plus_rect();
    send(
        &mut hovered,
        InputKind::MouseMove {
            at: point(plus.x.0 + 5.0, plus.y.0 + 5.0),
        },
    );
    let mut focused = strip(2, 800.0);
    focused.set_focused(Some(CaptionHit::NewTab));
    for (state, s) in [("plain", &plain), ("hover", &hovered), ("focus", &focused)] {
        for kind in [ThemeKind::Light, ThemeKind::Dark] {
            let list = draw(s, kind);
            let has_caret = list.commands.iter().any(
                |command| matches!(command, DrawCommand::Icon { icon_id, .. } if icon_id == "E96E"),
            );
            assert!(!has_caret, "{state} {kind:?}");
        }
    }
    let names: Vec<String> = plain
        .widgets()
        .unwrap()
        .children
        .iter()
        .map(|widget| widget.name.clone())
        .collect();
    assert_eq!(
        names,
        [
            "Tab 0",
            "Tab 1",
            "New tab",
            "Minimize",
            "Maximize or restore",
            "Close window",
        ]
    );
}

#[test]
fn tab_from_the_plus_leaves_the_row_and_arrows_wrap_to_the_first_tab() {
    let mut s = strip(1, 800.0);
    s.set_focused(Some(CaptionHit::NewTab));
    send(
        &mut s,
        InputKind::KeyDown {
            virtual_key: VK_RIGHT,
            modifiers: Modifiers::default(),
            repeat: false,
        },
    );
    assert_eq!(s.focused(), Some(CaptionHit::Tab(0)));
    s.set_focused(Some(CaptionHit::NewTab));
    send(
        &mut s,
        InputKind::KeyDown {
            virtual_key: VK_TAB,
            modifiers: Modifiers::default(),
            repeat: false,
        },
    );
    assert_eq!(s.focused(), None);
    assert_eq!(s.take_events(), vec![ShellEvent::FocusContent]);
}
