//! The small animations: exact frames at set times through the real renderer, real posted
//! messages on hidden windows, and the rules that keep motion away from typing.
use qe_api::motion::{
    BAR_MENU_START, CLOSE_FADE_MS, Clock, MENU_CURVE, MENU_OPEN_MS, OPEN_FADE_MS, SLIDE_CURVE,
    SLIDE_MS, SUBMENU_START,
};
use qe_api::{
    Bitmap, Color, CommandId, Dip, DisplayList, DrawCommand, Handled, InputEvent, InputKind,
    Modifiers, MouseButton, PixelRect, Point, Rect, RenderError, Renderer, Theme, ThemeKind, View,
    ViewContext, Widget, WidgetRole,
};
use qe_render::{BitmapRenderer, TextSettings};
use qe_shell::{
    ANIM_TIMER, CaptionHit, ContrastColors, MenuStep, ShellEvent, ShellWindow, Tab, TabStrip,
    WindowConfig,
};
use qe_ui::{MenuItem, MenuModel, ignore_foreground_changes_for_test};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::BOOL;

mod support;
use support::*;

const WIDTH: f32 = 1000.0;
const HEIGHT: f32 = 48.0;
/// A time well after the app's start, so that the list has rested.
const T0: f64 = 5000.0;

fn theme(kind: ThemeKind) -> Theme {
    Theme::new(kind)
}
fn moving_strip(count: usize, clock: &Clock) -> TabStrip {
    let mut s = TabStrip::new((0..count).map(|i| Tab::new(format!("Tab {i}"))).collect());
    s.layout(rect(0.0, 0.0, WIDTH, s.row_height()), &RealMeasurer);
    s.set_motion_clock(clock.clone());
    s.set_motion_off(false);
    clock.set_ms(T0);
    s
}
fn event(s: &mut TabStrip, kind: InputKind) -> Handled {
    s.handle(
        &InputEvent { time_us: 0, kind },
        &mut ViewContext::default(),
    )
}
fn at(x: f32, y: f32) -> Point {
    Point {
        x: Dip(x),
        y: Dip(y),
    }
}
fn press(s: &mut TabStrip, x: f32, y: f32, button: MouseButton) {
    event(
        s,
        InputKind::MouseDown {
            at: at(x, y),
            button,
            modifiers: Modifiers::default(),
        },
    );
}
fn release(s: &mut TabStrip, x: f32, y: f32, button: MouseButton) {
    event(
        s,
        InputKind::MouseUp {
            at: at(x, y),
            button,
            modifiers: Modifiers::default(),
        },
    );
}
fn draw(s: &TabStrip, kind: ThemeKind) -> DisplayList {
    let mut list = DisplayList::default();
    s.draw(&mut list, &theme(kind), &RealMeasurer);
    list
}
/// Where the title of this tab starts, as drawn.
fn text_x(list: &DisplayList, title: &str) -> Option<f32> {
    list.commands.iter().find_map(|c| match c {
        DrawCommand::TextRun { at, text, .. } if text == title => Some(at.x.0),
        _ => None,
    })
}
fn opacity(list: &DisplayList) -> Option<f32> {
    list.commands.iter().find_map(|c| match c {
        DrawCommand::PushOpacity(a) => Some(*a),
        _ => None,
    })
}
fn slid(from: f32, to: f32, ms: f64) -> f32 {
    from + (to - from) * SLIDE_CURVE.at(ms / SLIDE_MS) as f32
}
fn close_enough(a: f32, b: f32, what: &str) {
    assert!((a - b).abs() < 0.01, "{what}: {a} and {b}");
}
fn step(s: &mut TabStrip, clock: &Clock, ms: f64) {
    clock.set_ms(T0 + ms);
    s.step_motion(T0 + ms);
}
fn image_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/images")
        .join(format!("{name}.png"))
}
/// Same rule as the other saved pictures: 1 value in 10,000 may differ by more than 3.
fn check_picture(image: &Bitmap, name: &str) {
    let path = image_path(name);
    if std::env::var_os("QE_SHELL_UPDATE_IMAGES").is_some() {
        std::fs::write(&path, encode_png(image)).unwrap();
    }
    let saved = decode_own_png(&std::fs::read(&path).unwrap());
    let current = raw_pixels(image);
    assert_eq!(saved.len(), current.len(), "{name}");
    let far = saved
        .iter()
        .zip(&current)
        .filter(|(a, b)| a.abs_diff(**b) > 3)
        .count();
    assert!(far <= saved.len() / 10_000, "{name}: {far} values differ");
}
fn picture(s: &TabStrip, kind: ThemeKind) -> Bitmap {
    render_strip(s, &theme(kind), 96, WIDTH, HEIGHT)
}
fn theme_name(kind: ThemeKind) -> &'static str {
    if kind == ThemeKind::Dark {
        "dark"
    } else {
        "light"
    }
}

#[test]
fn a_closed_tab_fades_in_100_ms_and_its_neighbours_slide_in_167_ms() {
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        let clock = Clock::manual();
        let mut s = moving_strip(4, &clock);
        let (x2, x3, plus) = (
            s.tab_rects()[2].x.0,
            s.tab_rects()[3].x.0,
            s.plus_rect().x.0,
        );
        let inset = text_x(&draw(&s, kind), "Tab 2").unwrap() - x2;
        s.close(s.tab_id(1).unwrap());
        // Everything but drawing is final at once.
        assert_eq!(s.tabs().len(), 3);
        let (end2, end3, end_plus) = (
            s.tab_rects()[1].x.0,
            s.tab_rects()[2].x.0,
            s.plus_rect().x.0,
        );
        assert!(end2 < x2 && end_plus < plus);
        assert!(s.is_moving());
        // At time 0, nothing has moved yet.
        let list = draw(&s, kind);
        close_enough(text_x(&list, "Tab 2").unwrap(), x2 + inset, "first frame");
        assert_eq!(opacity(&list), Some(1.0));
        for ms in [16.0, 50.0, 90.0, 150.0] {
            step(&mut s, &clock, ms);
            let list = draw(&s, kind);
            close_enough(
                text_x(&list, "Tab 2").unwrap(),
                slid(x2, end2, ms) + inset,
                "Tab 2",
            );
            close_enough(
                text_x(&list, "Tab 3").unwrap(),
                slid(x3, end3, ms) + inset,
                "Tab 3",
            );
            if ms < CLOSE_FADE_MS {
                let want = 1.0 - (ms / CLOSE_FADE_MS) as f32;
                close_enough(opacity(&list).unwrap(), want, "the closed tab's fade");
                assert_eq!(text_x(&list, "Tab 1").map(|_| ()), Some(()));
            } else {
                assert_eq!(text_x(&list, "Tab 1"), None, "the ghost is gone at 100 ms");
            }
            if ms == 50.0 {
                check_picture(
                    &picture(&s, kind),
                    &format!("motion-close-mid-{}", theme_name(kind)),
                );
            }
        }
        step(&mut s, &clock, SLIDE_MS);
        assert!(!s.is_moving(), "all done at 167 ms");
        let list = draw(&s, kind);
        close_enough(text_x(&list, "Tab 2").unwrap(), end2 + inset, "end");
    }
}

#[test]
fn a_closed_tab_is_never_hit_and_never_in_the_screen_reader_tree() {
    let clock = Clock::manual();
    let mut s = moving_strip(4, &clock);
    let gone = s.tab_rects()[1];
    s.close(s.tab_id(1).unwrap());
    step(&mut s, &clock, 10.0);
    let names: Vec<String> = s
        .widgets()
        .unwrap()
        .children
        .iter()
        .map(|w| w.name.clone())
        .collect();
    assert!(!names.iter().any(|n| n == "Tab 1"), "{names:?}");
    // The tree says where each tab ends, from the first moment.
    let tree = s.widgets().unwrap();
    let tabs: Vec<Rect> = tree
        .children
        .iter()
        .filter(|w| w.role == WidgetRole::Tab)
        .map(|w| w.bounds)
        .collect();
    assert_eq!(tabs, s.tab_rects());
    // Nothing answers for the ghost: its old place belongs to a live tab, by index.
    match s.drawn_hit_test(at(gone.x.0 + 10.0, 20.0)) {
        CaptionHit::Tab(i) | CaptionHit::TabClose(i) => assert!(i < 3),
        other => panic!("{other:?}"),
    }
    assert_eq!(s.tabs().iter().filter(|t| t.title == "Tab 1").count(), 0);
}

#[test]
fn no_motion_if_the_list_changed_under_1000_ms_ago_unless_motion_is_under_way() {
    let clock = Clock::manual();
    let mut s = moving_strip(6, &clock);
    s.close(s.tab_id(0).unwrap());
    step(&mut s, &clock, SLIDE_MS);
    assert!(!s.is_moving());
    // 500 ms after the first close, with nothing moving: the second one jumps.
    clock.set_ms(T0 + 500.0);
    s.close(s.tab_id(0).unwrap());
    assert!(!s.is_moving(), "a quick change has no motion");
    assert_eq!(opacity(&draw(&s, ThemeKind::Light)), None);
    // A third, 999 ms after that one: still a jump.
    clock.set_ms(T0 + 1499.0);
    s.close(s.tab_id(0).unwrap());
    assert!(!s.is_moving());
    // And a whole second after: it moves.
    clock.set_ms(T0 + 2499.0);
    s.close(s.tab_id(0).unwrap());
    assert!(s.is_moving());
}

#[test]
fn a_second_close_during_a_close_goes_on_from_where_the_tab_is_drawn() {
    let clock = Clock::manual();
    let mut s = moving_strip(5, &clock);
    s.close(s.tab_id(1).unwrap());
    step(&mut s, &clock, 40.0);
    let before = text_x(&draw(&s, ThemeKind::Light), "Tab 4").unwrap();
    s.close(s.tab_id(1).unwrap());
    let after = text_x(&draw(&s, ThemeKind::Light), "Tab 4").unwrap();
    close_enough(before, after, "no jump at the second close");
    assert!(s.is_moving());
    // It ends at its final place, 167 ms after the second close.
    step(&mut s, &clock, 40.0 + SLIDE_MS);
    assert!(!s.is_moving());
}

#[test]
fn a_new_tab_fades_in_83_ms_with_its_text_at_once_and_the_plus_slides() {
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        let clock = Clock::manual();
        let mut s = moving_strip(2, &clock);
        let plus = s.plus_rect().x.0;
        let id = s.add_tab(Tab::new("New"));
        let end_plus = s.plus_rect().x.0;
        assert_eq!(s.index_of(id), Some(2));
        assert_eq!(s.active(), 2, "final from the first moment");
        // No wait: the text is there at time 0.
        assert!(text_x(&draw(&s, kind), "New").is_some());
        // Its X waits for the + to pass, so that the two never cross.
        let x_icons = |list: &DisplayList| {
            list.commands
                .iter()
                .filter(|c| matches!(c, DrawCommand::Icon { icon_id, .. } if icon_id == "E8BB"))
                .count()
        };
        let window_close = 1;
        assert_eq!(x_icons(&draw(&s, kind)), 2 + window_close);
        let fill_alpha = |list: &DisplayList, b: Rect| {
            list.commands.iter().find_map(|c| match c {
                DrawCommand::RoundedRect { rect, color, .. } if rect.x.0 == b.x.0 => Some(color.a),
                _ => None,
            })
        };
        let b = s.tab_rects()[2];
        let whole = theme(kind).tab_selected.a;
        assert_eq!(fill_alpha(&draw(&s, kind), b), Some(0));
        step(&mut s, &clock, OPEN_FADE_MS / 2.0);
        let list = draw(&s, kind);
        let half = fill_alpha(&list, b).unwrap();
        assert!(half.abs_diff(whole / 2) <= 1, "{half}");
        assert!(text_x(&list, "New").is_some());
        let drawn_plus = list
            .commands
            .iter()
            .filter_map(|c| match c {
                DrawCommand::Icon { at, icon_id, .. } if icon_id == "E710" => Some(at.x.0),
                _ => None,
            })
            .next()
            .unwrap();
        let moved = slid(plus, end_plus, OPEN_FADE_MS / 2.0) - plus;
        let at_start = {
            let c = moving_strip(2, &Clock::manual());
            draw(&c, kind)
                .commands
                .iter()
                .find_map(|c| match c {
                    DrawCommand::Icon { at, icon_id, .. } if icon_id == "E710" => Some(at.x.0),
                    _ => None,
                })
                .unwrap()
        };
        // Icons start on a whole pixel, in both places, so this can be off by one.
        assert!(
            (drawn_plus - (at_start + moved)).abs() <= 1.0,
            "{drawn_plus}, from {at_start} by {moved}"
        );
        check_picture(
            &picture(&s, kind),
            &format!("motion-new-mid-{}", theme_name(kind)),
        );
        step(&mut s, &clock, SLIDE_MS);
        assert!(!s.is_moving());
        assert_eq!(x_icons(&draw(&s, kind)), 3 + window_close);
    }
}

#[test]
fn other_tabs_slide_during_a_drag_and_hit_tests_stay_final_so_the_drag_does_not_shake() {
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        let clock = Clock::manual();
        let mut s = moving_strip(4, &clock);
        let (w, y) = (s.tab_width(), 20.0);
        press(&mut s, w / 2.0, y, MouseButton::Left);
        // Over Tab 1: it trades places with Tab 0, and only Tab 1 slides.
        event(&mut s, InputKind::MouseMove { at: at(w * 1.5, y) });
        assert_eq!(s.tabs()[1].title, "Tab 0", "the order is final at once");
        let inset = text_x(&draw(&s, kind), "Tab 3").unwrap() - s.tab_rects()[3].x.0;
        assert!(s.is_moving());
        step(&mut s, &clock, 40.0);
        let list = draw(&s, kind);
        close_enough(
            text_x(&list, "Tab 1").unwrap(),
            slid(w, 0.0, 40.0) + inset,
            "the other tab slides",
        );
        close_enough(
            text_x(&list, "Tab 0").unwrap(),
            w + inset,
            "the tab in hand is where the hand is",
        );
        if kind == ThemeKind::Light {
            check_picture(&picture(&s, kind), "motion-reorder-mid-light");
        } else {
            check_picture(&picture(&s, kind), "motion-reorder-mid-dark");
        }
        // The hit test says the final place, so a small move does not swap back.
        assert_eq!(s.hit_test(at(w * 1.5, y)), CaptionHit::Tab(1));
        event(&mut s, InputKind::MouseMove { at: at(w * 1.4, y) });
        assert_eq!(s.tabs()[1].title, "Tab 0", "no shake");
        step(&mut s, &clock, 40.0 + SLIDE_MS);
        assert!(!s.is_moving());
    }
}

#[test]
fn a_click_during_a_slide_lands_on_what_is_drawn_there() {
    let clock = Clock::manual();
    // Three tabs at their widest, so that all slide left by one width.
    let mut s = moving_strip(3, &clock);
    let w = s.tab_width();
    s.close(s.tab_id(0).unwrap());
    s.take_events();
    step(&mut s, &clock, 1.0);
    // Past where anything will end, but the eye still sees the last tab there.
    let x = w * 2.5;
    assert_eq!(
        s.hit_test(at(x, 20.0)),
        CaptionHit::Drag,
        "nothing ends here"
    );
    assert_eq!(s.drawn_hit_test(at(x, 20.0)), CaptionHit::Tab(1));
    press(&mut s, x, 20.0, MouseButton::Left);
    // By the release, the slide is over, and the press still counts.
    step(&mut s, &clock, SLIDE_MS + 5.0);
    release(&mut s, x, 20.0, MouseButton::Left);
    let selected = s.take_events();
    assert_eq!(selected, vec![ShellEvent::SelectTab(s.tab_id(1).unwrap())]);
    // A gap with nothing drawn takes the final answer.
    let mut s = moving_strip(3, &clock);
    clock.set_ms(T0 + 9000.0);
    s.close(s.tab_id(0).unwrap());
    assert_eq!(s.drawn_hit_test(at(10.0, 20.0)), CaptionHit::Tab(0));
}

#[test]
fn the_plus_as_drawn_takes_a_click_during_its_slide() {
    let clock = Clock::manual();
    let mut s = moving_strip(3, &clock);
    let plus = s.plus_rect();
    s.close(s.tab_id(0).unwrap());
    step(&mut s, &clock, 1.0);
    let (x, y) = (plus.x.0 + 5.0, plus.y.0 + 5.0);
    assert_eq!(s.hit_test(at(x, y)), CaptionHit::Drag);
    assert_eq!(s.drawn_hit_test(at(x, y)), CaptionHit::NewTab);
}

#[test]
fn a_window_button_fades_150_ms_in_and_100_ms_out_and_a_press_is_instant() {
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        let clock = Clock::manual();
        let mut s = moving_strip(2, &clock);
        let close = s
            .widgets()
            .unwrap()
            .children
            .iter()
            .find(|w| w.name == "Close window")
            .unwrap()
            .bounds;
        let (x, y) = (close.x.0 + 5.0, close.y.0 + 12.0);
        let red = |list: &DisplayList| {
            list.commands.iter().find_map(|c| match c {
                DrawCommand::FillRect { rect, color } if rect.x.0 == close.x.0 => Some(*color),
                _ => None,
            })
        };
        event(&mut s, InputKind::MouseMove { at: at(x, y) });
        assert!(s.is_moving());
        assert_eq!(red(&draw(&s, kind)).map_or(0, |c| c.a), 0, "no fill at 0");
        step(&mut s, &clock, 75.0);
        let half = red(&draw(&s, kind)).unwrap();
        assert_eq!((half.r, half.g, half.b), (0xc4, 0x2b, 0x1c));
        assert!(half.a.abs_diff(128) <= 1, "{half:?}");
        check_picture(
            &picture(&s, kind),
            &format!("motion-close-button-mid-{}", theme_name(kind)),
        );
        step(&mut s, &clock, 150.0);
        assert!(!s.is_moving());
        assert_eq!(red(&draw(&s, kind)).unwrap().a, 255);
        // Out, in 100 ms from all of it.
        s.pointer_left(&mut ViewContext::default());
        assert!(s.is_moving());
        step(&mut s, &clock, 200.0);
        assert!(red(&draw(&s, kind)).unwrap().a.abs_diff(128) <= 1);
        step(&mut s, &clock, 250.0);
        assert!(!s.is_moving());
        assert_eq!(red(&draw(&s, kind)), None);
        // A press in the middle of a fade shows at once, and ends the fade.
        event(&mut s, InputKind::MouseMove { at: at(x, y) });
        step(&mut s, &clock, 260.0);
        press(&mut s, x, y, MouseButton::Left);
        assert!(!s.is_moving());
        assert_eq!(
            red(&draw(&s, kind)).unwrap(),
            Color {
                a: 230,
                ..Color::rgb(0xc4, 0x2b, 0x1c)
            }
        );
    }
}

#[test]
fn hover_on_tabs_and_the_plus_has_no_fade() {
    let clock = Clock::manual();
    let mut s = moving_strip(2, &clock);
    let b = s.tab_rects()[1];
    event(
        &mut s,
        InputKind::MouseMove {
            at: at(b.x.0 + 5.0, b.y.0 + 5.0),
        },
    );
    assert!(!s.is_moving());
    let plus = s.plus_rect();
    event(
        &mut s,
        InputKind::MouseMove {
            at: at(plus.x.0 + 5.0, plus.y.0 + 5.0),
        },
    );
    assert!(!s.is_moving());
}

#[test]
fn with_motion_off_each_end_state_appears_at_once_with_the_same_list() {
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        // Off is how every strip begins.
        let mut s = TabStrip::new((0..4).map(|i| Tab::new(format!("Tab {i}"))).collect());
        s.layout(rect(0.0, 0.0, WIDTH, s.row_height()), &RealMeasurer);
        assert!(s.motion_off());
        s.close(s.tab_id(1).unwrap());
        assert!(!s.is_moving());
        let mut fresh = TabStrip::new(
            [0, 2, 3]
                .into_iter()
                .map(|i| Tab::new(format!("Tab {i}")))
                .collect(),
        );
        fresh.layout(rect(0.0, 0.0, WIDTH, fresh.row_height()), &RealMeasurer);
        assert_eq!(draw(&s, kind), draw(&fresh, kind));
        s.add_tab(Tab::new("New"));
        assert!(!s.is_moving());
        let close = s
            .widgets()
            .unwrap()
            .children
            .iter()
            .find(|w| w.name == "Close window")
            .unwrap()
            .bounds;
        event(
            &mut s,
            InputKind::MouseMove {
                at: at(close.x.0 + 5.0, close.y.0 + 12.0),
            },
        );
        assert!(!s.is_moving());
        let fill = draw(&s, kind).commands.iter().any(|c| {
            matches!(c, DrawCommand::FillRect { rect, color }
                if rect.x.0 == close.x.0 && *color == Color::rgb(0xc4, 0x2b, 0x1c))
        });
        assert!(fill, "the whole hover at once");
    }
}

#[test]
fn turning_motion_off_during_a_motion_shows_the_end_at_once() {
    let clock = Clock::manual();
    let mut s = moving_strip(3, &clock);
    s.close(s.tab_id(0).unwrap());
    assert!(s.set_motion_off(true));
    assert!(!s.is_moving());
    assert_eq!(opacity(&draw(&s, ThemeKind::Light)), None);
}

#[test]
fn a_frame_repaints_only_what_moves() {
    let clock = Clock::manual();
    // Three tabs at their widest: a close changes no width.
    let mut s = moving_strip(3, &clock);
    let third = s.tab_rects()[1];
    s.close(s.tab_id(1).unwrap());
    clock.set_ms(T0 + 16.0);
    let rects = s.step_motion(T0 + 16.0);
    assert_eq!(rects.len(), 1);
    assert!(rects[0].x.0 >= third.x.0 - 0.01, "{rects:?}");
    assert!(rects[0].width.0 < WIDTH - third.x.0, "{rects:?}");
    // The frame that ends it repaints too, and then none has anything to do.
    clock.set_ms(T0 + 500.0);
    assert!(!s.step_motion(T0 + 500.0).is_empty());
    assert!(!s.is_moving());
    assert!(s.step_motion(T0 + 516.0).is_empty());
}

// ---- The width hold, on plain strips.

fn x_of(s: &TabStrip, index: usize) -> (f32, f32) {
    let b = s.tab_rects()[index];
    (b.x.0 + b.width.0 - 10.0, b.y.0 + 10.0)
}
fn click(s: &mut TabStrip, x: f32, y: f32, button: MouseButton) {
    event(s, InputKind::MouseMove { at: at(x, y) });
    press(s, x, y, button);
    release(s, x, y, button);
    for e in s.take_events() {
        if let ShellEvent::CloseTabRequested(id) = e {
            s.close(id);
        }
    }
}
fn narrow_strip(count: usize) -> TabStrip {
    // Wide enough for 240 each, so the hold shows only after closes in a narrower row.
    let mut s = TabStrip::new((0..count).map(|i| Tab::new(format!("Tab {i}"))).collect());
    s.layout(rect(0.0, 0.0, WIDTH, s.row_height()), &RealMeasurer);
    s
}

#[test]
fn widths_stay_during_a_close_by_mouse_until_the_pointer_leaves_with_motion_off_too() {
    for button in [MouseButton::Left, MouseButton::Middle] {
        let mut s = narrow_strip(8);
        let width = s.tab_width();
        assert!(width < 240.0);
        let (x, y) = x_of(&s, 1);
        click(&mut s, x, y, button);
        assert_eq!(s.tabs().len(), 7);
        assert_eq!(s.tab_width(), width, "the width holds");
        assert!(s.width_is_held());
        // The same place closes the next tab, again and again.
        for count in [6, 5] {
            click(&mut s, x, y, button);
            assert_eq!(s.tabs().len(), count);
            assert_eq!(s.tab_width(), width);
        }
        let plus_held = s.plus_rect().x.0;
        // The pointer leaves.
        s.pointer_left(&mut ViewContext::default());
        assert!(!s.width_is_held());
        assert!(s.tab_width() > width, "the tabs fill the row at once");
        assert!(s.plus_rect().x.0 > plus_held);
    }
}

#[test]
fn moving_to_the_menu_bar_or_the_window_buttons_or_the_text_ends_the_hold() {
    let leave_to: [fn(&TabStrip) -> Point; 2] = [
        |s| {
            let b = s.row_rect();
            at(b.x.0 + 5.0, b.y.0 + b.height.0 + 100.0)
        },
        |s| {
            let close = s.widgets().unwrap();
            let b = close
                .children
                .iter()
                .find(|w| w.name == "Minimize")
                .unwrap()
                .bounds;
            at(b.x.0 + 5.0, b.y.0 + 12.0)
        },
    ];
    for to in leave_to {
        let mut s = narrow_strip(8);
        let (x, y) = x_of(&s, 1);
        click(&mut s, x, y, MouseButton::Left);
        assert!(s.width_is_held());
        // Along the row keeps it, even past the +.
        let plus = s.plus_rect();
        event(
            &mut s,
            InputKind::MouseMove {
                at: at(plus.x.0 + 60.0, 20.0),
            },
        );
        assert!(s.width_is_held());
        let point = to(&s);
        event(&mut s, InputKind::MouseMove { at: point });
        assert!(!s.width_is_held());
    }
}

#[test]
fn a_close_by_key_resizes_at_once_even_with_the_pointer_in_the_row() {
    let mut s = narrow_strip(8);
    let width = s.tab_width();
    event(
        &mut s,
        InputKind::MouseMove {
            at: at(100.0, 20.0),
        },
    );
    s.close(s.tab_id(1).unwrap());
    assert!(!s.width_is_held());
    assert!(s.tab_width() > width);
    // A click that a question stopped, then a key: the key's close has no hold.
    let mut s = narrow_strip(8);
    let id = s.tab_id(1).unwrap();
    let (x, y) = x_of(&s, 1);
    event(&mut s, InputKind::MouseMove { at: at(x, y) });
    press(&mut s, x, y, MouseButton::Left);
    release(&mut s, x, y, MouseButton::Left);
    event(
        &mut s,
        InputKind::KeyDown {
            virtual_key: 0x57,
            modifiers: Modifiers {
                ctrl: true,
                ..Default::default()
            },
            repeat: false,
        },
    );
    s.close(id);
    assert!(!s.width_is_held());
    // Any held width ends at a close by key too.
    let mut s = narrow_strip(8);
    let (x, y) = x_of(&s, 1);
    click(&mut s, x, y, MouseButton::Left);
    assert!(s.width_is_held());
    s.close(s.tab_id(0).unwrap());
    assert!(!s.width_is_held());
}

#[test]
fn a_click_whose_pointer_has_left_by_the_close_has_no_hold() {
    let mut s = narrow_strip(8);
    let id = s.tab_id(1).unwrap();
    let (x, y) = x_of(&s, 1);
    event(&mut s, InputKind::MouseMove { at: at(x, y) });
    press(&mut s, x, y, MouseButton::Left);
    release(&mut s, x, y, MouseButton::Left);
    // A save question took its time, and the pointer went away.
    s.pointer_left(&mut ViewContext::default());
    s.close(id);
    assert!(!s.width_is_held());
}

#[test]
fn closing_the_rightmost_tab_lets_the_tabs_grow_and_keeps_the_right_end_in_place() {
    let mut s = narrow_strip(8);
    let width = s.tab_width();
    let end = s.tab_rects()[7].x.0 + width;
    let (x, y) = x_of(&s, 7);
    click(&mut s, x, y, MouseButton::Left);
    assert_eq!(s.tabs().len(), 7);
    let grown = s.tab_width();
    close_enough(grown, width * 8.0 / 7.0, "the width grows");
    close_enough(s.tab_rects()[6].x.0 + grown, end, "the right end stays");
    // The new last tab's X is under the pointer, so a second click closes it.
    click(&mut s, x, y, MouseButton::Left);
    assert_eq!(s.tabs().len(), 6);
    // When there is no more to fill, the hold ends.
    while s.tabs().len() > 1 {
        let (x, y) = x_of(&s, s.tabs().len() - 1);
        click(&mut s, x, y, MouseButton::Left);
        assert!(s.tab_width() <= 240.0);
    }
    assert!(!s.width_is_held());
}

#[test]
fn a_new_size_or_a_new_tab_ends_the_hold() {
    let mut s = narrow_strip(8);
    let (x, y) = x_of(&s, 1);
    click(&mut s, x, y, MouseButton::Left);
    assert!(s.width_is_held());
    s.layout(rect(0.0, 0.0, 700.0, s.row_height()), &RealMeasurer);
    assert!(!s.width_is_held());

    let mut s = narrow_strip(8);
    let (x, y) = x_of(&s, 1);
    click(&mut s, x, y, MouseButton::Left);
    s.add_tab(Tab::new("New"));
    assert!(!s.width_is_held());
}

#[test]
fn the_next_x_lights_up_under_the_pointer_after_a_close_by_mouse() {
    let mut s = narrow_strip(5);
    let (x, y) = x_of(&s, 1);
    click(&mut s, x, y, MouseButton::Left);
    assert_eq!(s.hit_test(at(x, y)), CaptionHit::TabClose(1));
    let list = draw(&s, ThemeKind::Light);
    let hover = list
        .commands
        .iter()
        .any(|c| matches!(c, DrawCommand::RoundedRect { rect, .. } if rect.width.0 == 24.0));
    assert!(
        hover,
        "the X of the tab now under the pointer shows its hover"
    );
}

// ---- Hidden windows and real messages.

#[derive(Clone)]
struct Counted {
    inner: Rc<RefCell<BitmapRenderer>>,
    frames: Rc<Cell<u32>>,
}
impl Counted {
    fn new(frames: Rc<Cell<u32>>) -> Self {
        Self {
            inner: Rc::new(RefCell::new(BitmapRenderer::new(64, 64).unwrap())),
            frames,
        }
    }
}
impl Renderer for Counted {
    fn begin_frame(&mut self) -> Result<(), RenderError> {
        self.frames.set(self.frames.get() + 1);
        self.inner.borrow_mut().begin_frame()
    }
    fn draw(&mut self, list: &DisplayList) -> Result<(), RenderError> {
        Renderer::draw(&mut *self.inner.borrow_mut(), list)
    }
    fn end_frame(&mut self) -> Result<(), RenderError> {
        self.inner.borrow_mut().end_frame()
    }
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        self.inner.borrow_mut().resize(width, height)
    }
    fn set_dpi(&mut self, dpi: u32) -> Result<(), RenderError> {
        self.inner.borrow_mut().set_dpi(dpi)
    }
    fn render_to_bitmap(
        &mut self,
        list: &DisplayList,
        width: u32,
        height: u32,
    ) -> Result<Bitmap, RenderError> {
        self.inner
            .borrow_mut()
            .render_to_bitmap(list, width, height)
    }
    fn invalidate(&mut self, rect: Rect) {
        Renderer::invalidate(&mut *self.inner.borrow_mut(), rect);
    }
    fn invalidate_all(&mut self) {
        Renderer::invalidate_all(&mut *self.inner.borrow_mut());
    }
    fn present_rect(&mut self, dc: *mut c_void, rect: PixelRect) -> Result<(), RenderError> {
        Renderer::present_rect(&mut *self.inner.borrow_mut(), dc, rect)
    }
}

struct Fix {
    window: ShellWindow,
    frames: Rc<Cell<u32>>,
    painter_calls: Rc<Cell<u32>>,
    clock: Clock,
}
impl Drop for Fix {
    fn drop(&mut self) {
        if let Ok(hwnd) = self.window.hwnd() {
            assert!(
                !unsafe { IsWindowVisible(HWND(hwnd)) }.as_bool(),
                "it showed"
            );
        }
    }
}
fn pump() {
    let mut msg = MSG::default();
    while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
/// Runs the real message queue for this long, timers too.
fn pump_for(ms: u64) {
    let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < end {
        pump();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
fn fixture(motion: Option<bool>) -> Fix {
    ignore_foreground_changes_for_test();
    let frames = Rc::new(Cell::new(0));
    let window = ShellWindow::create_with(
        WindowConfig {
            cell_width: 8.0,
            line_height: 17.0,
            fake_dpi: Some(96),
            record_system_commands: true,
            ..Default::default()
        },
        Box::new(Counted::new(frames.clone())),
        Box::new(RealMeasurer),
    )
    .unwrap();
    window.take_events().unwrap();
    let painter_calls = Rc::new(Cell::new(0));
    let calls = painter_calls.clone();
    window.set_content_painter(move |_, _| calls.set(calls.get() + 1));
    window
        .set_popup_renderers(|width, height| {
            let mut renderer = BitmapRenderer::new(width, height)?;
            renderer.set_text_settings(TextSettings::reference())?;
            Ok(Box::new(renderer) as Box<dyn Renderer>)
        })
        .unwrap();
    let clock = Clock::manual();
    clock.set_ms(T0);
    window.set_motion_clock(clock.clone()).unwrap();
    // This PC's own setting must not decide what the test shows.
    window.set_animation_setting_off(false).unwrap();
    window.force_motion(motion).unwrap();
    Fix {
        window,
        frames,
        painter_calls,
        clock,
    }
}
impl Fix {
    fn hwnd(&self) -> HWND {
        HWND(self.window.hwnd().unwrap())
    }
    fn send(&self, msg: u32, w: usize, l: isize) -> isize {
        unsafe { SendMessageW(self.hwnd(), msg, Some(WPARAM(w)), Some(LPARAM(l))) }.0
    }
    fn post(&self, msg: u32, w: usize, l: isize) {
        unsafe { PostMessageW(Some(self.hwnd()), msg, WPARAM(w), LPARAM(l)) }.unwrap();
    }
    fn tabs(&self, count: usize) {
        self.window
            .with_strip(|s| {
                for i in 1..count {
                    s.add_tab(Tab::new(format!("Tab {i}")));
                }
            })
            .unwrap();
        self.window.finish_motion().unwrap();
        // The list has rested.
        self.clock.set_ms(self.clock.now_ms() + 2000.0);
    }
    fn count(&self) -> usize {
        self.window.with_strip(|s| s.tabs().len()).unwrap()
    }
    fn close_first(&self) {
        let id = self.window.with_strip(|s| s.tab_id(0).unwrap()).unwrap();
        self.window.close_tab(id).unwrap();
    }
    fn answer_closes(&self) {
        for event in self.window.take_events().unwrap() {
            if let ShellEvent::CloseTabRequested(id) = event {
                self.window.close_tab(id).unwrap();
            }
        }
    }
    /// A real click: real posted messages through the real queue.
    fn post_click(&self, msg_down: u32, msg_up: u32, x: i32, y: i32) {
        let l = pack(x, y);
        self.post(WM_MOUSEMOVE, 0, l);
        self.post(msg_down, 1, l);
        self.post(msg_up, 0, l);
        pump();
    }
}
fn pack(x: i32, y: i32) -> isize {
    ((y as i16 as u16 as u32) << 16 | (x as i16 as u16 as u32)) as i32 as isize
}

#[test]
fn repeated_clicks_by_real_posted_messages_hit_the_next_x_under_the_hold() {
    for motion in [Some(false), Some(true)] {
        for (down, up) in [
            (WM_LBUTTONDOWN, WM_LBUTTONUP),
            (WM_MBUTTONDOWN, WM_MBUTTONUP),
        ] {
            let f = fixture(motion);
            f.tabs(8);
            let width = f.window.with_strip(|s| s.tab_width()).unwrap();
            assert!(width < 240.0, "{width}");
            let (x, y) = f
                .window
                .with_strip(|s| {
                    let b = s.tab_rects()[1];
                    ((b.x.0 + b.width.0 - 10.0) as i32, (b.y.0 + 10.0) as i32)
                })
                .unwrap();
            let ids: Vec<_> = f
                .window
                .with_strip(|s| s.tabs().iter().map(Tab::id).collect())
                .unwrap();
            // The real cursor is never on a hidden window, so that Windows sends its leave
            // notice at once. A person's cursor stays here.
            f.window.set_test_cursor(Some((x, y))).unwrap();
            for round in 1..=4_usize {
                f.post_click(down, up, x, y);
                f.answer_closes();
                assert_eq!(f.count(), 8 - round, "round {round}");
                assert_eq!(f.window.with_strip(|s| s.tab_width()).unwrap(), width);
                // Each click closed the tab that came under the pointer, in order.
                assert_eq!(
                    f.window.with_strip(|s| s.tab_id(1).unwrap()).unwrap(),
                    ids[1 + round]
                );
                // A person's next click comes well after the 167 ms slide or its jump.
                f.clock.set_ms(f.clock.now_ms() + 300.0);
                f.window.motion_tick().unwrap();
            }
            // The real message for leaving, with the cursor gone.
            f.window.set_test_cursor(None).unwrap();
            f.post(WM_MOUSELEAVE, 0, 0);
            pump();
            assert!(f.window.with_strip(|s| s.tab_width()).unwrap() > width);
            assert!(!f.window.with_strip(|s| s.width_is_held()).unwrap());
        }
    }
}

#[test]
fn a_fast_second_click_during_the_slide_still_closes_the_next_tab() {
    let f = fixture(Some(true));
    f.tabs(8);
    let (x, y) = f
        .window
        .with_strip(|s| {
            let b = s.tab_rects()[1];
            ((b.x.0 + b.width.0 - 10.0) as i32, (b.y.0 + 10.0) as i32)
        })
        .unwrap();
    f.window.set_test_cursor(Some((x, y))).unwrap();
    f.post_click(WM_LBUTTONDOWN, WM_LBUTTONUP, x, y);
    f.answer_closes();
    assert_eq!(f.count(), 7);
    for ms in [1.0, 16.0, 40.0, 60.0, 100.0] {
        // A copy of the moment: how far the slide has come when the click arrives.
        f.window
            .with_strip(|s| s.step_motion(T0 + 2000.0 + ms))
            .unwrap();
        let hit = f
            .window
            .with_strip(|s| {
                s.drawn_hit_test(Point {
                    x: Dip(x as f32),
                    y: Dip(y as f32),
                })
            })
            .unwrap();
        assert_eq!(hit, CaptionHit::TabClose(1), "at {ms} ms");
    }
}

#[test]
fn windows_asks_about_the_plus_where_it_is_drawn_during_its_slide() {
    let f = fixture(Some(true));
    f.tabs(2);
    let plus = f.window.with_strip(|s| s.plus_rect()).unwrap();
    let mut at = POINT {
        x: plus.x.0 as i32 + 3,
        y: plus.y.0 as i32 + 3,
    };
    let _ = unsafe { ClientToScreen(f.hwnd(), &mut at) };
    let l = pack(at.x, at.y);
    assert_eq!(f.send(WM_NCHITTEST, 0, l), HTCLIENT as isize);
    f.close_first();
    f.clock.set_ms(f.clock.now_ms() + 1.0);
    f.window.motion_tick().unwrap();
    // Its final place is further left. A click here must not start a move of the window.
    assert_eq!(f.send(WM_NCHITTEST, 0, l), HTCLIENT as isize);
    f.clock.set_ms(f.clock.now_ms() + SLIDE_MS);
    f.window.motion_tick().unwrap();
    assert_eq!(f.send(WM_NCHITTEST, 0, l), HTCAPTION as isize);
}

#[test]
fn the_timer_runs_only_while_something_moves_and_nothing_wakes_afterwards() {
    let f = fixture(Some(true));
    f.tabs(3);
    assert!(!f.window.motion_timer_running().unwrap());
    f.close_first();
    assert!(f.window.motion_timer_running().unwrap());
    f.clock.set_ms(f.clock.now_ms() + 50.0);
    f.window.motion_tick().unwrap();
    assert!(f.window.motion_timer_running().unwrap());
    f.clock.set_ms(f.clock.now_ms() + SLIDE_MS);
    f.window.motion_tick().unwrap();
    assert!(
        !f.window.motion_timer_running().unwrap(),
        "killed in the turn of the last frame"
    );
    // Windows itself says that no such timer runs.
    assert!(!f.window.kill_timer(ANIM_TIMER).unwrap());
    let stats = f.window.motion_stats().unwrap();
    let frames = f.frames.get();
    pump_for(120);
    let after = f.window.motion_stats().unwrap();
    assert_eq!(after.ticks, stats.ticks, "zero wake-ups at idle");
    assert_eq!(after.frames, stats.frames, "zero frames at idle");
    assert_eq!(f.frames.get(), frames, "zero renderer frames at idle");
}

#[test]
fn the_real_timer_on_the_real_clock_plays_a_close_to_its_end_and_stops() {
    let f = fixture(Some(false));
    f.tabs(3);
    f.window.set_motion_clock(Clock::real()).unwrap();
    f.window.force_motion(Some(true)).unwrap();
    f.close_first();
    assert!(f.window.motion_timer_running().unwrap());
    let begun = std::time::Instant::now();
    while f.window.motion_timer_running().unwrap()
        && begun.elapsed() < std::time::Duration::from_secs(5)
    {
        pump_for(5);
    }
    assert!(!f.window.motion_timer_running().unwrap());
    let stats = f.window.motion_stats().unwrap();
    assert!(stats.ticks >= 3, "{stats:?}");
    assert!(stats.frames >= 3, "{stats:?}");
    let ticks = stats.ticks;
    pump_for(100);
    assert_eq!(f.window.motion_stats().unwrap().ticks, ticks);
    // The owner never hears of this timer.
    assert!(
        !f.window
            .take_events()
            .unwrap()
            .iter()
            .any(|e| matches!(e, ShellEvent::Timer(_)))
    );
}

#[test]
fn an_animation_frame_runs_no_text_painter_and_copies_only_part_of_the_row() {
    let f = fixture(Some(true));
    f.tabs(2);
    let width = f.window.with_strip(|s| s.row_rect().width.0).unwrap() as i32;
    f.send(WM_PAINT, 0, 0);
    f.close_first();
    // The change itself is an ordinary whole paint.
    f.send(WM_PAINT, 0, 0);
    let (painter, frames) = (f.painter_calls.get(), f.frames.get());
    f.window.invalidated_rects().unwrap();
    for ms in [16.0, 32.0, 48.0, 64.0] {
        f.clock
            .set_ms(f.clock.now_ms() + ms - if ms > 16.0 { ms - 16.0 } else { 0.0 });
        f.window.motion_tick().unwrap();
    }
    assert_eq!(f.painter_calls.get(), painter, "no text painter on a frame");
    assert_eq!(f.frames.get(), frames + 4, "four frames, all real");
    let content_top = f.window.content_rect().unwrap().y.0 as i32;
    let rects = f.window.invalidated_rects().unwrap();
    assert!(!rects.is_empty());
    for rect in rects {
        assert!(rect.bottom <= content_top, "{rect:?}");
        assert!(rect.right - rect.left < width, "only what moved: {rect:?}");
    }
    // Text that changes during motion still paints: the two share one frame.
    f.window.invalidate_content().unwrap();
    f.clock.set_ms(f.clock.now_ms() + 16.0);
    f.send(WM_PAINT, 0, 0);
    assert_eq!(f.painter_calls.get(), painter + 1);
    assert_eq!(f.window.motion_stats().unwrap().frames, 4);
}

/// A name, and how a window comes to end its motion.
type Ending = (&'static str, fn(&Fix));

#[test]
fn hiding_minimizing_a_new_dpi_or_closing_ends_the_motion_at_once() {
    let endings: [Ending; 5] = [
        ("hide", |f| {
            f.send(WM_SHOWWINDOW, 0, 0);
        }),
        ("minimize", |f| {
            f.send(WM_SIZE, SIZE_MINIMIZED as usize, 0);
        }),
        ("dpi", |f| {
            let b = RECT {
                left: 0,
                top: 0,
                right: 800,
                bottom: 600,
            };
            f.send(
                WM_DPICHANGED,
                120 | (120 << 16),
                (&b as *const RECT) as isize,
            );
        }),
        ("close", |f| {
            f.send(WM_CLOSE, 0, 0);
        }),
        ("destroy", |f| {
            f.window.destroy().unwrap();
        }),
    ];
    for (name, end) in endings {
        let f = fixture(Some(true));
        f.tabs(4);
        f.close_first();
        assert!(f.window.motion_timer_running().unwrap(), "{name}");
        end(&f);
        if name == "destroy" {
            continue;
        }
        assert!(!f.window.motion_timer_running().unwrap(), "{name}");
        assert!(!f.window.with_strip(|s| s.is_moving()).unwrap(), "{name}");
    }
}

#[test]
fn a_setting_off_or_high_contrast_or_a_hidden_window_means_motion_off() {
    // A hidden window: every window of every other test here that did not ask for motion.
    let f = fixture(None);
    f.tabs(3);
    f.close_first();
    assert!(!f.window.motion_timer_running().unwrap());
    assert!(f.window.with_strip(|s| s.motion_off()).unwrap());

    // The setting, with the window otherwise free to move.
    let f = fixture(Some(true));
    f.tabs(3);
    f.close_first();
    assert!(f.window.motion_timer_running().unwrap());
    f.window.set_animation_setting_off(true).unwrap();
    assert!(!f.window.motion_timer_running().unwrap(), "ends at once");
    f.close_first();
    assert!(!f.window.motion_timer_running().unwrap());

    // High contrast.
    let f = fixture(Some(true));
    f.tabs(3);
    f.close_first();
    let colors = ContrastColors {
        window: Color::rgb(0, 0, 0),
        window_text: Color::rgb(255, 255, 255),
        highlight: Color::rgb(26, 235, 255),
        highlight_text: Color::rgb(0, 0, 0),
        gray_text: Color::rgb(63, 242, 63),
    };
    f.window.apply_theme(true, Some(colors)).unwrap();
    assert!(!f.window.motion_timer_running().unwrap());
    f.close_first();
    assert!(!f.window.motion_timer_running().unwrap());
    assert!(f.window.with_strip(|s| s.motion_off()).unwrap());
}

#[test]
fn wm_settingchange_reads_the_setting_again() {
    let f = fixture(Some(true));
    f.window.set_animation_setting_off(true).unwrap();
    let mut animate = BOOL(1);
    unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&raw mut animate).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .unwrap();
    f.send(WM_SETTINGCHANGE, 0, 0);
    assert_eq!(
        f.window.with_strip(|s| s.motion_off()).unwrap(),
        !animate.as_bool()
    );
}

#[test]
fn a_window_with_motion_keeps_its_old_paint_when_nothing_moves() {
    // A hover with no motion: the ordinary path still runs the painter.
    let f = fixture(Some(false));
    f.send(WM_PAINT, 0, 0);
    let painter = f.painter_calls.get();
    f.window
        .with_strip(|s| s.row_rect())
        .map(|row| f.window.invalidate_rect(row).unwrap())
        .unwrap();
    f.send(WM_PAINT, 0, 0);
    assert_eq!(f.painter_calls.get(), painter + 1);
}

// ---- Menus.

unsafe extern "system" fn note_popup(hwnd: HWND, list: LPARAM) -> BOOL {
    let mut name = [0u16; 64];
    let length = unsafe { GetClassNameW(hwnd, &mut name) } as usize;
    if String::from_utf16_lossy(&name[..length]) == "QuickEditPopup" {
        let list = unsafe { &mut *(list.0 as *mut Vec<HWND>) };
        list.push(hwnd);
    }
    BOOL(1)
}
fn popups(owner: HWND) -> Vec<HWND> {
    let mut list: Vec<HWND> = Vec::new();
    let thread = unsafe { GetWindowThreadProcessId(owner, None) };
    let _ =
        unsafe { EnumThreadWindows(thread, Some(note_popup), LPARAM((&raw mut list) as isize)) };
    list
}
fn height(popup: HWND) -> i32 {
    let mut b = RECT::default();
    unsafe { GetWindowRect(popup, &mut b) }.unwrap();
    b.bottom - b.top
}
fn model() -> MenuModel {
    let mut sub = MenuModel::default();
    for i in 0..8 {
        sub.items
            .push(MenuItem::command(format!("Inner {i}"), CommandId::ALL[i]));
    }
    let mut menu = MenuModel::default();
    menu.items.push(MenuItem::submenu("More", sub));
    for i in 0..10 {
        menu.items
            .push(MenuItem::command(format!("Row {i}"), CommandId::ALL[i]));
    }
    menu
}
fn anchor() -> Rect {
    rect(10.0, 40.0, 30.0, 20.0)
}
/// The rows of the open menu alone. The bar's labels have the same role.
fn menu_rows(tree: &Widget, out: &mut Vec<Widget>) {
    for child in &tree.children {
        if child.role == WidgetRole::Menu {
            out.extend(
                child
                    .children
                    .iter()
                    .filter(|row| row.role == WidgetRole::MenuItem)
                    .cloned(),
            );
        } else {
            menu_rows(child, out);
        }
    }
}

#[test]
fn a_menu_reveals_from_half_its_height_over_167_ms_with_its_rows_final_at_once() {
    let f = fixture(Some(true));
    f.window.open_menu(model(), anchor()).unwrap();
    let popup = popups(f.hwnd())[0];
    let mut rows = Vec::new();
    menu_rows(&f.window.widgets().unwrap(), &mut rows);
    assert_eq!(rows.len(), 11, "the screen reader has every row at once");
    let whole = {
        let mut lowest = 0.0_f32;
        for row in &rows {
            lowest = lowest.max(row.bounds.y.0 + row.bounds.height.0);
        }
        lowest - rows[0].bounds.y.0
    };
    assert!(whole > 100.0);
    // The first frame shows half. It waits for nothing, and the timer runs.
    let full = {
        let mut b = RECT::default();
        unsafe { GetClientRect(popup, &mut b) }.unwrap();
        b.bottom
    };
    assert!(f.window.motion_timer_running().unwrap());
    let total = (full as f32 / BAR_MENU_START).round() as i32;
    for ms in [40.0_f64, 100.0, 150.0] {
        f.clock.set_ms(T0 + ms);
        f.window.motion_tick().unwrap();
        let part =
            BAR_MENU_START + (1.0 - BAR_MENU_START) * MENU_CURVE.at(ms / MENU_OPEN_MS) as f32;
        let want = (total as f32 * part).round() as i32;
        assert!(
            (height(popup) - want).abs() <= 1,
            "at {ms}: {} and {want}",
            height(popup)
        );
    }
    f.clock.set_ms(T0 + MENU_OPEN_MS);
    f.window.motion_tick().unwrap();
    assert!(
        (height(popup) - total).abs() <= 1,
        "{} and {total}",
        height(popup)
    );
    assert!(!f.window.motion_timer_running().unwrap());
    f.window.close_menu().unwrap();
}

#[test]
fn a_row_not_yet_shown_takes_a_click_from_the_first_moment() {
    let f = fixture(Some(true));
    f.window.open_menu(model(), anchor()).unwrap();
    let popup = popups(f.hwnd())[0];
    let mut rows = Vec::new();
    menu_rows(&f.window.widgets().unwrap(), &mut rows);
    let last = rows.last().unwrap().bounds;
    let mut b = RECT::default();
    unsafe { GetWindowRect(popup, &mut b) }.unwrap();
    let mut screen = POINT {
        x: (last.x.0 + 20.0) as i32,
        y: (last.y.0 + last.height.0 / 2.0) as i32,
    };
    unsafe {
        let _ = ClientToScreen(f.hwnd(), &mut screen);
    }
    assert!(screen.y >= b.bottom, "the row is past what shows now");
    let _ = unsafe { ScreenToClient(popup, &mut screen) };
    let l = pack(screen.x, screen.y);
    for (msg, w) in [(WM_MOUSEMOVE, 0), (WM_LBUTTONDOWN, 1), (WM_LBUTTONUP, 0)] {
        unsafe { PostMessageW(Some(popup), msg, WPARAM(w), LPARAM(l)) }.unwrap();
    }
    pump();
    let ran: Vec<ShellEvent> = f.window.take_events().unwrap();
    assert!(
        ran.iter()
            .any(|e| *e == ShellEvent::RunCommand(CommandId::ALL[9])),
        "{ran:?}"
    );
    assert!(!f.window.menu_is_open().unwrap());
    assert!(
        !f.window.motion_timer_running().unwrap(),
        "nothing left to run"
    );
}

#[test]
fn a_submenu_starts_at_067_and_a_key_during_the_reveal_still_goes_to_the_menu() {
    let f = fixture(Some(true));
    // A key opens this menu, so that the first row is selected.
    f.post(WM_KEYDOWN, 0x41, 1);
    f.window.with_strip(|s| s.tab_id(0)).map(|_| ()).unwrap();
    pump();
    f.window.take_events().unwrap();
    f.window.open_menu(model(), anchor()).unwrap();
    let root = popups(f.hwnd())[0];
    assert_eq!(popups(f.hwnd()).len(), 1);
    // Keys go first, whatever the timer does: the top row is selected, and Right opens it.
    f.send(WM_KEYDOWN, 0x27, 1);
    let all = popups(f.hwnd());
    assert_eq!(all.len(), 2, "the submenu opened during the reveal");
    let inner = all.into_iter().find(|p| *p != root).unwrap();
    let mut rows = Vec::new();
    menu_rows(&f.window.widgets().unwrap(), &mut rows);
    let shown = height(inner);
    f.clock.set_ms(T0 + 1000.0);
    f.window.motion_tick().unwrap();
    f.clock.set_ms(T0 + 2000.0);
    f.window.motion_tick().unwrap();
    let total = height(inner);
    assert!(
        (shown as f32 - total as f32 * SUBMENU_START).abs() <= 1.0,
        "{shown} of {total}"
    );
    assert!(!f.window.motion_timer_running().unwrap());
    f.window.close_menu().unwrap();
}

#[test]
fn moving_along_the_bar_shows_the_next_menu_whole_with_no_second_reveal() {
    let f = fixture(Some(true));
    f.window.set_event_handler(|handle, event| {
        if let ShellEvent::OpenMenuGroup(..) = &event
            && let ShellEvent::OpenMenuGroup(_, rect) = event
        {
            handle.open_menu(model(), rect).unwrap();
        }
    });
    // F10 and Enter would do as well: Alt+F is the shortest way to open by key.
    f.post(WM_SYSKEYDOWN, 0x12, 1 << 29 | 1);
    f.post(WM_SYSKEYDOWN, 0x46, 1 << 29 | 1);
    pump();
    let first = popups(f.hwnd())[0];
    let shown = height(first);
    assert!(
        f.window.motion_timer_running().unwrap(),
        "a key opens with the motion too"
    );
    f.clock.set_ms(T0 + 40.0);
    f.window.motion_tick().unwrap();
    assert!(height(first) > shown, "it reveals");
    // Up out of the top row is no help here. The Right arrow on a plain row moves along.
    f.send(WM_KEYDOWN, 0x28, 1);
    f.send(WM_KEYDOWN, 0x27, 1);
    let next = popups(f.hwnd());
    assert_eq!(next.len(), 1);
    assert_ne!(next[0], first, "another menu");
    let whole = height(next[0]);
    assert!(
        (whole as f32 - shown as f32 / BAR_MENU_START).abs() <= 1.0,
        "{whole}, from {shown}"
    );
    assert!(
        !f.window.motion_timer_running().unwrap(),
        "no second reveal"
    );
    f.window.close_menu().unwrap();
    pump();
}

#[test]
fn with_motion_off_a_menu_shows_whole_at_once_and_it_closes_at_once() {
    let f = fixture(Some(false));
    f.window.open_menu(model(), anchor()).unwrap();
    let popup = popups(f.hwnd())[0];
    let shown = height(popup);
    assert!(!f.window.motion_timer_running().unwrap());
    let mut rows = Vec::new();
    menu_rows(&f.window.widgets().unwrap(), &mut rows);
    let last = rows.last().unwrap().bounds;
    assert!(shown as f32 >= last.height.0 * 10.0);
    f.window.close_menu().unwrap();
    assert!(popups(f.hwnd()).is_empty(), "no fade outlives the menu");
}

#[test]
fn a_mid_reveal_frame_shows_the_top_rows_where_they_will_stay() {
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        let view = model();
        let t = theme(kind);
        let mut menu = qe_ui::MenuView::new(qe_api::WidgetId(20_000), "Menu", view);
        let size = menu.model.preferred_size(&RealMeasurer);
        menu.layout(rect(0.0, 0.0, size.0, size.1), &RealMeasurer);
        let mut list = DisplayList::default();
        menu.draw_level(0, &mut list, &t, &RealMeasurer);
        let (w, h) = (size.0.round() as u32, size.1.round() as u32);
        let mut renderer = BitmapRenderer::new(w, h).unwrap();
        renderer
            .set_text_settings(TextSettings::reference())
            .unwrap();
        renderer.begin_frame().unwrap();
        renderer.draw(&list).unwrap();
        renderer.end_frame().unwrap();
        let whole = renderer.bitmap().unwrap();
        // The window at 40 ms: its pixels are the top of the same bitmap.
        let part =
            BAR_MENU_START + (1.0 - BAR_MENU_START) * MENU_CURVE.at(40.0 / MENU_OPEN_MS) as f32;
        let shown = (h as f32 * part).round() as u32;
        let top = Bitmap {
            width: w,
            height: shown,
            bgra: whole.bgra[..(w * shown * 4) as usize].to_vec(),
        };
        check_picture(&top, &format!("motion-menu-mid-{}", theme_name(kind)));
    }
}

// ---- The order of a person's pointer, and what must end a hold. ----

impl Fix {
    /// Where this part of the row is, in client pixels, found by the row's own hit test.
    fn find(&self, wanted: CaptionHit) -> (i32, i32) {
        self.window
            .with_strip(|s| {
                let y = s.tab_rects()[0].y.0 + 10.0;
                (0..2000)
                    .map(|x| at(x as f32, y))
                    .find(|p| s.hit_test(*p) == wanted)
                    .map(|p| (p.x.0 as i32, p.y.0 as i32))
                    .unwrap_or_else(|| panic!("no {wanted:?} in the row"))
            })
            .unwrap()
    }
    fn screen(&self, client: (i32, i32)) -> isize {
        let mut p = POINT {
            x: client.0,
            y: client.1,
        };
        assert!(unsafe { ClientToScreen(self.hwnd(), &mut p) }.as_bool());
        pack(p.x, p.y)
    }
    fn hover(&self) -> CaptionHit {
        self.window.with_strip(|s| s.hover_hit()).unwrap()
    }
    fn held(&self) -> bool {
        self.window.with_strip(|s| s.width_is_held()).unwrap()
    }
    /// Only the notices that this test posts arrive, in the order that Windows sends them.
    fn by_hand(&self) {
        self.window.set_leave_by_test(true).unwrap();
    }
}
const TEXT_POINT: (i32, i32) = (300, 300);

#[test]
fn a_jump_to_a_window_button_keeps_its_hover_when_the_old_leave_notice_comes_behind_the_move() {
    let f = fixture(Some(true));
    f.by_hand();
    let close = f.find(CaptionHit::Close);
    f.window.set_test_cursor(Some(close)).unwrap();
    f.post(WM_MOUSEMOVE, 0, pack(TEXT_POINT.0, TEXT_POINT.1));
    pump();
    assert_eq!(f.window.leave_notice_armed().unwrap(), "client");
    // The order Windows uses: the move on the new part, then the notice for the old part.
    f.post(WM_NCMOUSEMOVE, HTCLOSE as usize, f.screen(close));
    f.post(WM_MOUSELEAVE, 0, 0);
    pump();
    assert_eq!(f.hover(), CaptionHit::Close, "the old notice cleared it");
    assert_eq!(f.window.leave_notice_armed().unwrap(), "non-client");
    // The fade plays to its end, with no second move.
    f.clock.set_ms(T0 + 200.0);
    f.window.motion_tick().unwrap();
    assert_eq!(f.hover(), CaptionHit::Close);
    // The real leave of the window still clears it.
    f.window.set_test_cursor(None).unwrap();
    f.post(WM_NCMOUSELEAVE, 0, 0);
    pump();
    assert_eq!(f.hover(), CaptionHit::None);
    assert_eq!(f.window.leave_notice_armed().unwrap(), "none");
}

#[test]
fn the_same_jump_with_the_notice_first_also_ends_lit() {
    let f = fixture(Some(true));
    f.by_hand();
    let close = f.find(CaptionHit::Close);
    f.post(WM_MOUSEMOVE, 0, pack(TEXT_POINT.0, TEXT_POINT.1));
    pump();
    f.post(WM_MOUSELEAVE, 0, 0);
    f.post(WM_NCMOUSEMOVE, HTCLOSE as usize, f.screen(close));
    pump();
    assert_eq!(f.hover(), CaptionHit::Close);
}

#[test]
fn a_jump_after_a_menu_closed_by_keys_keeps_the_hover_too() {
    let f = fixture(Some(true));
    f.by_hand();
    let close = f.find(CaptionHit::Close);
    f.post(WM_MOUSEMOVE, 0, pack(TEXT_POINT.0, TEXT_POINT.1));
    pump();
    f.window.open_menu(model(), anchor()).unwrap();
    f.send(WM_KEYDOWN, 0x1B, 1);
    pump();
    assert!(!f.window.menu_is_open().unwrap());
    assert_eq!(f.window.leave_notice_armed().unwrap(), "none");
    f.post(WM_NCMOUSEMOVE, HTCLOSE as usize, f.screen(close));
    f.post(WM_MOUSELEAVE, 0, 0);
    pump();
    assert_eq!(f.hover(), CaptionHit::Close);
}

#[test]
fn the_hold_survives_the_old_notice_on_the_empty_row_and_ends_at_the_window_buttons() {
    let f = fixture(Some(true));
    f.by_hand();
    f.tabs(8);
    let width = f.window.with_strip(|s| s.tab_width()).unwrap();
    let x = f.find(CaptionHit::TabClose(1));
    f.window.set_test_cursor(Some(x)).unwrap();
    f.post_click(WM_LBUTTONDOWN, WM_LBUTTONUP, x.0, x.1);
    f.answer_closes();
    assert!(f.held());
    // From a tab to the empty part of the row: the old notice must not end the hold.
    let (right, y) = f
        .window
        .with_strip(|s| {
            let plus = s.plus_rect();
            (plus.x.0 + plus.width.0 + 20.0, s.tab_rects()[0].y.0 + 10.0)
        })
        .unwrap();
    let empty = (right as i32, y as i32);
    f.window.set_test_cursor(Some(empty)).unwrap();
    f.post(WM_NCMOUSEMOVE, HTCAPTION as usize, f.screen(empty));
    f.post(WM_MOUSELEAVE, 0, 0);
    pump();
    assert!(f.held());
    assert_eq!(f.window.with_strip(|s| s.tab_width()).unwrap(), width);
    // On to the window buttons: the move ends the hold, and the button stays lit.
    let close = f.find(CaptionHit::Close);
    f.window.set_test_cursor(Some(close)).unwrap();
    f.post(WM_NCMOUSEMOVE, HTCLOSE as usize, f.screen(close));
    pump();
    assert!(!f.held());
    assert_eq!(f.hover(), CaptionHit::Close);
}

#[test]
fn a_notice_of_the_old_kind_does_not_tell_the_text_that_a_pointer_now_on_it_has_left() {
    let f = fixture(Some(true));
    f.by_hand();
    let close = f.find(CaptionHit::Close);
    f.post(WM_NCMOUSEMOVE, HTCLOSE as usize, f.screen(close));
    pump();
    f.window.take_events().unwrap();
    f.post(WM_MOUSEMOVE, 0, pack(TEXT_POINT.0, TEXT_POINT.1));
    f.post(WM_NCMOUSELEAVE, 0, 0);
    pump();
    let events = f.window.take_events().unwrap();
    assert!(
        !events.contains(&ShellEvent::ContentPointerLeft),
        "{events:?}"
    );
    assert_eq!(f.hover(), CaptionHit::None, "the move already cleared it");
    // The other way, the text really was left.
    f.post(WM_NCMOUSEMOVE, HTCLOSE as usize, f.screen(close));
    f.post(WM_MOUSELEAVE, 0, 0);
    pump();
    let events = f.window.take_events().unwrap();
    assert!(
        events.contains(&ShellEvent::ContentPointerLeft),
        "{events:?}"
    );
}

// ---- A menu's first frame. ----

#[test]
fn a_menu_level_is_drawn_before_it_shows_and_its_first_frame_is_copied_at_once() {
    let f = fixture(Some(true));
    f.post(WM_KEYDOWN, 0x41, 1);
    pump();
    f.window.take_events().unwrap();
    f.window.open_menu(model(), anchor()).unwrap();
    assert_eq!(
        f.window.menu_steps().unwrap(),
        [MenuStep::Drawn(0), MenuStep::Shown(0), MenuStep::Copied(0)],
        "the rows must be ready before the level shows"
    );
    assert_eq!(unsafe { GetCapture() }, popups(f.hwnd())[0]);
    // A submenu: the open level draws again, and the new one shows after its own drawing.
    f.send(WM_KEYDOWN, 0x27, 1);
    let steps = f.window.menu_steps().unwrap();
    let new = &steps[3..];
    assert_eq!(
        new,
        [
            MenuStep::Drawn(0),
            MenuStep::Copied(0),
            MenuStep::Drawn(1),
            MenuStep::Shown(1),
            MenuStep::Copied(1)
        ]
    );
    assert!(f.window.problems().unwrap().is_empty());
    f.window.close_menu().unwrap();
}

#[test]
fn opening_a_menu_paints_no_document_and_the_first_show_is_at_the_reveal_start_height() {
    let f = fixture(Some(true));
    let painted = f.painter_calls.get();
    f.window.open_menu(model(), anchor()).unwrap();
    assert_eq!(f.painter_calls.get(), painted, "no paint of the document");
    let popup = popups(f.hwnd())[0];
    let shown = height(popup);
    f.clock.set_ms(T0 + MENU_OPEN_MS);
    f.window.motion_tick().unwrap();
    let whole = height(popup);
    assert!(
        (shown as f32 - whole as f32 * BAR_MENU_START).abs() <= 1.0,
        "{shown} of {whole}"
    );
    f.window.close_menu().unwrap();
}

#[test]
fn a_menu_opened_during_a_press_in_the_row_takes_the_mouse_with_no_reentered_problem() {
    let f = fixture(Some(true));
    unsafe {
        SetCapture(f.hwnd());
    }
    f.window.open_menu(model(), anchor()).unwrap();
    assert_eq!(unsafe { GetCapture() }, popups(f.hwnd())[0]);
    assert!(
        f.window.problems().unwrap().is_empty(),
        "{:?}",
        f.window.problems()
    );
    f.window.close_menu().unwrap();
}

// ---- R5: a message during a reveal's window move finds no borrow. ----

thread_local! {
    static OLD_PROC: Cell<isize> = const { Cell::new(0) };
    static CLOSE_MENU_OF: RefCell<Option<qe_shell::ShellHandle>> = const { RefCell::new(None) };
}
unsafe extern "system" fn closes_the_menu_when_moved(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    if msg == WM_WINDOWPOSCHANGING
        && let Some(handle) = CLOSE_MENU_OF.with(|slot| slot.borrow_mut().take())
    {
        handle.close_menu().unwrap();
    }
    let old = OLD_PROC.with(Cell::get);
    unsafe { CallWindowProcW(std::mem::transmute::<isize, WNDPROC>(old), hwnd, msg, w, l) }
}
fn close_on_next_move(f: &Fix) {
    let popup = popups(f.hwnd())[0];
    CLOSE_MENU_OF.with(|slot| *slot.borrow_mut() = Some(f.window.handle()));
    let old = unsafe {
        SetWindowLongPtrW(
            popup,
            GWLP_WNDPROC,
            closes_the_menu_when_moved as *const () as isize,
        )
    };
    OLD_PROC.with(|slot| slot.set(old));
}

#[test]
fn a_menu_closed_by_a_message_during_a_reveal_frame_stays_closed() {
    let f = fixture(Some(true));
    f.window.open_menu(model(), anchor()).unwrap();
    close_on_next_move(&f);
    f.clock.set_ms(T0 + 50.0);
    f.window.motion_tick().unwrap();
    assert!(!f.window.menu_is_open().unwrap());
    pump();
    assert!(!f.window.motion_timer_running().unwrap());
}

#[test]
fn a_menu_closed_by_a_message_while_motion_finishes_stays_closed() {
    let f = fixture(Some(true));
    f.window.open_menu(model(), anchor()).unwrap();
    close_on_next_move(&f);
    f.window.finish_motion().unwrap();
    assert!(!f.window.menu_is_open().unwrap());
    assert!(!f.window.motion_timer_running().unwrap());
}

// ---- R1: a click lands on what is drawn. ----

#[test]
fn with_no_hold_a_press_on_a_sliding_tabs_title_selects_it_and_does_not_close_it() {
    let clock = Clock::manual();
    let mut s = moving_strip(4, &clock);
    s.add_tab(Tab::new("New"));
    s.take_events();
    step(&mut s, &clock, 8.0);
    let last = s.tab_rects()[2];
    let drawn = s.drawn_tab_places()[2];
    assert!(drawn.width.0 > last.width.0, "it is still narrowing");
    let x = last.x.0 + last.width.0 - 10.0;
    assert_eq!(s.hit_test(at(x, 20.0)), CaptionHit::TabClose(2));
    assert_eq!(
        s.drawn_hit_test(at(x, 20.0)),
        CaptionHit::Tab(2),
        "drawn from {} wide {}",
        drawn.x.0,
        drawn.width.0
    );
    press(&mut s, x, 20.0, MouseButton::Left);
    step(&mut s, &clock, SLIDE_MS + 5.0);
    release(&mut s, x, 20.0, MouseButton::Left);
    assert!(
        !s.take_events()
            .iter()
            .any(|e| matches!(e, ShellEvent::CloseTabRequested(_))),
        "the tab must not close"
    );
    assert_eq!(s.tabs().len(), 5);
}

#[test]
fn after_a_close_by_key_a_press_in_the_gap_on_the_fading_x_closes_nothing() {
    let clock = Clock::manual();
    let mut s = narrow_moving_strip(8, &clock);
    let y = 20.0;
    s.close(s.tab_id(1).unwrap());
    s.take_events();
    assert!(!s.width_is_held());
    step(&mut s, &clock, 1.0);
    // Where nothing is drawn, and a tab's X will end. The tab that fades was there.
    let places = s.drawn_tab_places();
    let x = (0..1000)
        .map(|x| x as f32)
        .find(|x| {
            matches!(s.hit_test(at(*x, y)), CaptionHit::TabClose(_))
                && !places.iter().any(|b| *x >= b.x.0 && *x < b.x.0 + b.width.0)
        })
        .expect("a gap over a final X");
    assert!(
        !matches!(s.drawn_hit_test(at(x, y)), CaptionHit::TabClose(_)),
        "{:?}",
        s.drawn_hit_test(at(x, y))
    );
    press(&mut s, x, y, MouseButton::Left);
    step(&mut s, &clock, SLIDE_MS + 5.0);
    release(&mut s, x, y, MouseButton::Left);
    assert!(
        !s.take_events()
            .iter()
            .any(|e| matches!(e, ShellEvent::CloseTabRequested(_)))
    );
}

#[test]
fn under_a_hold_a_press_during_the_slide_still_closes_the_next_tab() {
    let clock = Clock::manual();
    let mut s = narrow_moving_strip(8, &clock);
    let (x, y) = x_of(&s, 1);
    click(&mut s, x, y, MouseButton::Left);
    assert!(s.width_is_held());
    for ms in [1.0, 8.0, 40.0, 100.0] {
        step(&mut s, &clock, ms);
        assert_eq!(s.drawn_hit_test(at(x, y)), CaptionHit::TabClose(1), "{ms}");
    }
}

// ---- R2: a hold ends with the pointer. ----

fn held_strip() -> (TabStrip, f32, f32) {
    let mut s = narrow_strip(8);
    let (x, y) = x_of(&s, 1);
    click(&mut s, x, y, MouseButton::Left);
    assert!(s.width_is_held());
    (s, x, y)
}

#[test]
fn a_drag_released_outside_the_row_ends_the_hold() {
    let (mut s, x, y) = held_strip();
    press(&mut s, x - 30.0, y, MouseButton::Left);
    event(&mut s, InputKind::MouseMove { at: at(1300.0, y) });
    assert!(s.width_is_held(), "the press still has the mouse");
    release(&mut s, 1300.0, y, MouseButton::Left);
    assert!(!s.width_is_held());
}

#[test]
fn a_tear_off_ends_the_hold() {
    let (mut s, x, y) = held_strip();
    press(&mut s, x - 30.0, y, MouseButton::Left);
    event(
        &mut s,
        InputKind::MouseMove {
            at: at(x - 30.0, y + 300.0),
        },
    );
    assert!(!s.width_is_held());
}

#[test]
fn a_lost_capture_or_a_blur_outside_the_row_ends_the_hold_and_inside_it_does_not() {
    for blur in [false, true] {
        let finish = |s: &mut TabStrip| {
            if blur {
                event(s, InputKind::Blur);
            } else {
                s.capture_lost(&mut ViewContext::default());
            }
        };
        let (mut s, x, y) = held_strip();
        press(&mut s, x - 30.0, y, MouseButton::Left);
        event(&mut s, InputKind::MouseMove { at: at(1500.0, y) });
        finish(&mut s);
        assert!(!s.width_is_held(), "outside, blur={blur}");
        let (mut s, x, y) = held_strip();
        press(&mut s, x - 30.0, y, MouseButton::Left);
        finish(&mut s);
        assert!(s.width_is_held(), "still on the row, blur={blur}");
    }
}

#[test]
fn a_middle_press_released_outside_the_row_ends_the_hold() {
    let (mut s, x, y) = held_strip();
    press(&mut s, x - 30.0, y, MouseButton::Middle);
    event(&mut s, InputKind::MouseMove { at: at(1300.0, y) });
    release(&mut s, 1300.0, y, MouseButton::Middle);
    assert!(!s.width_is_held());
}

#[test]
fn the_leave_notice_is_armed_again_when_a_hold_starts_and_after_a_press_used_it_up() {
    let f = fixture(Some(true));
    f.by_hand();
    f.tabs(8);
    let x = f.find(CaptionHit::TabClose(1));
    f.window.set_test_cursor(Some(x)).unwrap();
    f.post_click(WM_LBUTTONDOWN, WM_LBUTTONUP, x.0, x.1);
    // The notice came during the press, and no move followed the release.
    f.post(WM_MOUSELEAVE, 0, 0);
    pump();
    assert_eq!(f.window.leave_notice_armed().unwrap(), "none");
    f.answer_closes();
    assert!(f.held());
    assert_eq!(
        f.window.leave_notice_armed().unwrap(),
        "client",
        "a hold starts with a notice on its way"
    );
    // A press that uses it up, then ends.
    let l = pack(x.0, x.1 + 1);
    f.post(WM_RBUTTONDOWN, 2, l);
    pump();
    unsafe {
        SetCapture(f.hwnd());
    }
    f.post(WM_MOUSELEAVE, 0, 0);
    pump();
    assert_eq!(f.window.leave_notice_armed().unwrap(), "none");
    unsafe { ReleaseCapture() }.unwrap();
    pump();
    assert_eq!(f.window.leave_notice_armed().unwrap(), "client");
    // That notice ends the hold, with the cursor gone.
    f.window.set_test_cursor(None).unwrap();
    f.post(WM_MOUSELEAVE, 0, 0);
    pump();
    assert!(!f.held());
}

// ---- R3: a close the mouse did not make has no hold. ----

#[test]
fn a_key_or_a_press_after_an_unanswered_x_click_means_the_later_close_has_no_hold() {
    let keys_or_press: [(u32, usize); 2] = [(WM_KEYDOWN, 0x12), (WM_RBUTTONDOWN, 2)];
    for (msg, w) in keys_or_press {
        let f = fixture(Some(true));
        f.tabs(8);
        let x = f.find(CaptionHit::TabClose(1));
        f.window.set_test_cursor(Some(x)).unwrap();
        f.post_click(WM_LBUTTONDOWN, WM_LBUTTONUP, x.0, x.1);
        // The owner said no to this click, for now: a question was up, and got Cancel.
        let closes: Vec<_> = f.window.take_events().unwrap();
        assert!(
            closes
                .iter()
                .any(|e| matches!(e, ShellEvent::CloseTabRequested(_)))
        );
        let id = f.window.with_strip(|s| s.tab_id(1).unwrap()).unwrap();
        f.post(msg, w, pack(x.0, x.1));
        pump();
        f.window.take_events().unwrap();
        f.window.close_tab(id).unwrap();
        assert!(!f.held(), "message {msg}");
    }
}

#[test]
fn a_key_that_reaches_only_a_menu_after_an_unanswered_x_click_means_no_hold() {
    let f = fixture(Some(true));
    f.tabs(8);
    let x = f.find(CaptionHit::TabClose(1));
    f.window.set_test_cursor(Some(x)).unwrap();
    f.post_click(WM_LBUTTONDOWN, WM_LBUTTONUP, x.0, x.1);
    f.window.take_events().unwrap();
    let id = f.window.with_strip(|s| s.tab_id(1).unwrap()).unwrap();
    f.window.open_menu(model(), anchor()).unwrap();
    f.send(WM_KEYDOWN, 0x1B, 1);
    pump();
    assert!(!f.window.menu_is_open().unwrap());
    f.window.take_events().unwrap();
    f.window.close_tab(id).unwrap();
    assert!(!f.held());
}

#[test]
fn an_owner_can_say_that_a_close_by_mouse_was_not_answered_with_a_close() {
    let mut s = narrow_strip(8);
    let (x, y) = x_of(&s, 1);
    press(&mut s, x, y, MouseButton::Left);
    release(&mut s, x, y, MouseButton::Left);
    s.take_events();
    s.forget_pointer_close();
    s.close(s.tab_id(1).unwrap());
    assert!(!s.width_is_held());
}

// ---- R4 ----

#[test]
fn closing_many_tabs_at_once_fades_only_a_few() {
    let clock = Clock::manual();
    let mut s = moving_strip(300, &clock);
    for _ in 0..250 {
        s.close(s.tab_id(0).unwrap());
    }
    s.take_events();
    let list = draw(&s, ThemeKind::Dark);
    let fades = list
        .commands
        .iter()
        .filter(|c| matches!(c, DrawCommand::PushOpacity(_)))
        .count();
    assert!(fades <= 8, "{fades} fading tabs in one frame");
    assert!(fades >= 1);
}

fn narrow_moving_strip(count: usize, clock: &Clock) -> TabStrip {
    let mut s = moving_strip(count, clock);
    assert!(s.tab_width() < 240.0);
    s.take_events();
    s
}

// ---- S2: a menu used up the leave notice. ----

fn hold_then_menu(f: &Fix) {
    f.by_hand();
    f.tabs(8);
    let x = f.find(CaptionHit::TabClose(1));
    f.window.set_test_cursor(Some(x)).unwrap();
    f.post_click(WM_LBUTTONDOWN, WM_LBUTTONUP, x.0, x.1);
    f.answer_closes();
    assert!(f.held());
    f.window.open_menu(model(), anchor()).unwrap();
}

#[test]
fn a_pointer_that_left_the_window_during_a_menu_ends_the_hold_when_the_menu_closes() {
    let f = fixture(Some(true));
    hold_then_menu(&f);
    f.window.set_test_cursor(Some((-50, -50))).unwrap();
    f.send(WM_KEYDOWN, 0x1B, 1);
    pump();
    assert!(!f.window.menu_is_open().unwrap());
    assert!(!f.held(), "the hold outlived the pointer");
}

#[test]
fn a_pointer_still_on_the_row_keeps_the_hold_with_a_notice_on_its_way_after_a_menu() {
    let f = fixture(Some(true));
    hold_then_menu(&f);
    f.send(WM_KEYDOWN, 0x1B, 1);
    pump();
    assert!(!f.window.menu_is_open().unwrap());
    // This comes first: a call into the row can arm the notice too.
    assert_eq!(f.window.leave_notice_armed().unwrap(), "client");
    assert!(f.held());
    // That notice ends it, with no more moves on the window.
    f.window.set_test_cursor(None).unwrap();
    f.post(WM_MOUSELEAVE, 0, 0);
    pump();
    assert!(!f.held());
}

#[test]
fn a_menu_dismissed_by_another_app_leaves_no_stale_pointer_for_the_blur() {
    let f = fixture(Some(true));
    hold_then_menu(&f);
    f.window.set_test_cursor(Some((-50, -50))).unwrap();
    // Another program took the mouse.
    unsafe { ReleaseCapture() }.unwrap();
    pump();
    f.post(WM_MOUSEMOVE, 0, pack(-50, -50));
    pump();
    assert!(!f.window.menu_is_open().unwrap());
    assert!(!f.held());
}

// ---- S3 and S4: no click closes a tab that no one aimed at. ----

/// The fewest tabs that overflow the row, so that one close ends the overflow.
fn just_overflowing() -> TabStrip {
    let mut count = 2;
    loop {
        let s = narrow_strip(count);
        if s.overflow() {
            return s;
        }
        count += 1;
    }
}
fn wheel_right(s: &mut TabStrip, notches: i32) {
    for _ in 0..notches {
        event(
            s,
            InputKind::Wheel {
                at: at(200.0, 20.0),
                delta_x: 1.0,
                delta_y: 0.0,
                modifiers: Modifiers::default(),
            },
        );
    }
}
fn ids_of(s: &TabStrip) -> Vec<qe_shell::TabId> {
    s.tabs().iter().map(Tab::id).collect()
}
/// Under the pointer now: the X of this tab.
fn x_of_is_under(s: &TabStrip, x: f32, y: f32, id: qe_shell::TabId) {
    match s.hit_test(at(x, y)) {
        CaptionHit::TabClose(i) => assert_eq!(s.tab_id(i), Some(id), "the X of another tab"),
        other => panic!("{other:?} is under the pointer, and the next X should be"),
    }
}
/// Which visible tab comes last, by final place, before the X buttons.
fn visible_x(s: &TabStrip, index: usize) -> (f32, f32) {
    let b = s.tab_rects()[index];
    assert!(
        b.x.0 >= 0.0 && b.x.0 + b.width.0 <= 1000.0,
        "{index} is off"
    );
    x_of(s, index)
}

#[test]
fn a_close_by_mouse_that_ends_the_overflow_keeps_the_row_until_the_pointer_leaves() {
    let mut s = just_overflowing();
    wheel_right(&mut s, 1);
    assert!(s.overflow());
    let count = s.tabs().len();
    let ids = ids_of(&s);
    let index = count - 3;
    let (x, y) = visible_x(&s, index);
    let slot = s.tab_rects()[index];
    click(&mut s, x, y, MouseButton::Left);
    assert_eq!(s.tabs().len(), count - 1);
    assert_eq!(s.tab_rects()[index].x.0, slot.x.0, "the row moved");
    x_of_is_under(&s, x, y, ids[index + 1]);
    assert!(s.width_is_held());
    click(&mut s, x, y, MouseButton::Left);
    assert_eq!(s.tabs().len(), count - 2);
    x_of_is_under(&s, x, y, ids[index + 2]);
    // The pointer leaves: the row settles with no arrows.
    s.pointer_left(&mut ViewContext::default());
    assert!(!s.width_is_held());
    assert!(!s.overflow());
    assert!(s.tab_rects()[0].x.0 <= 1.0, "{:?}", s.tab_rects()[0]);
}

#[test]
fn a_close_by_mouse_with_the_active_tab_scrolled_off_does_not_move_the_row() {
    let mut s = narrow_strip(40);
    let ids = ids_of(&s);
    s.select(ids[0]);
    wheel_right(&mut s, 12);
    let index = 16;
    let (x, y) = visible_x(&s, index);
    let slot = s.tab_rects()[index];
    click(&mut s, x, y, MouseButton::Left);
    assert_eq!(s.tabs().len(), 39);
    assert_eq!(s.tab_rects()[index].x.0, slot.x.0, "the row moved");
    x_of_is_under(&s, x, y, ids[index + 1]);
    assert_eq!(
        s.tab_id(s.active()),
        Some(ids[0]),
        "the same tab stays active"
    );
    click(&mut s, x, y, MouseButton::Left);
    x_of_is_under(&s, x, y, ids[index + 2]);
    // The same close by key still shows the active tab.
    let mut s = narrow_strip(40);
    let ids = ids_of(&s);
    s.select(ids[0]);
    wheel_right(&mut s, 12);
    s.close(ids[16]);
    assert!(s.tab_rects()[0].x.0 >= 0.0, "the active tab is in view");
}

#[test]
fn a_close_by_mouse_at_the_end_of_the_scroll_keeps_the_next_x_under_the_pointer() {
    let mut s = narrow_strip(40);
    let ids = ids_of(&s);
    wheel_right(&mut s, 60);
    let last = s.tab_rects().len() - 1;
    let index = last - 3;
    let (x, y) = visible_x(&s, index);
    click(&mut s, x, y, MouseButton::Left);
    x_of_is_under(&s, x, y, ids[index + 1]);
    assert!(s.width_is_held());
    // The rightmost tab: the row keeps its right end, and the new last X comes under it.
    let mut s = narrow_strip(40);
    let ids = ids_of(&s);
    wheel_right(&mut s, 60);
    let (x, y) = visible_x(&s, 39);
    click(&mut s, x, y, MouseButton::Left);
    x_of_is_under(&s, x, y, ids[38]);
}

#[test]
fn a_held_row_lets_go_when_a_tab_off_the_view_is_selected_by_key() {
    let mut s = just_overflowing();
    wheel_right(&mut s, 1);
    let index = s.tabs().len() - 3;
    let (x, y) = visible_x(&s, index);
    click(&mut s, x, y, MouseButton::Left);
    assert!(s.width_is_held());
    s.select(s.tab_id(0).unwrap());
    assert!(s.tab_rects()[0].x.0 >= 0.0);
}

// ---- S5 ----

#[test]
fn a_middle_click_where_nothing_is_drawn_does_nothing() {
    let clock = Clock::manual();
    let mut s = narrow_moving_strip(8, &clock);
    s.close(s.tab_id(1).unwrap());
    s.take_events();
    assert!(!s.width_is_held());
    step(&mut s, &clock, 1.0);
    let y = 20.0;
    let places = s.drawn_tab_places();
    let x = (0..1000)
        .map(|x| x as f32)
        .find(|x| {
            matches!(
                s.hit_test(at(*x, y)),
                CaptionHit::Tab(_) | CaptionHit::TabClose(_)
            ) && !places.iter().any(|b| *x >= b.x.0 && *x < b.x.0 + b.width.0)
        })
        .expect("a gap over a final tab");
    press(&mut s, x, y, MouseButton::Middle);
    assert!(!s.holds_mouse(), "nothing was pressed");
    release(&mut s, x, y, MouseButton::Middle);
    let events = s.take_events();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ShellEvent::CloseTabRequested(_))),
        "{events:?}"
    );
    // On a drawn tab it still closes that tab.
    let drawn = s.drawn_tab_places()[0];
    let x = drawn.x.0 + 10.0;
    let first = s.tab_id(0).unwrap();
    press(&mut s, x, y, MouseButton::Middle);
    release(&mut s, x, y, MouseButton::Middle);
    assert!(
        s.take_events()
            .contains(&ShellEvent::CloseTabRequested(first))
    );
}

// ---- S8: the reveal starts when the menu shows, and the document is not touched. ----

#[test]
fn the_reveal_starts_at_the_show_even_if_drawing_the_first_frame_took_time() {
    let f = fixture(Some(true));
    let clock = f.clock.clone();
    f.window
        .set_popup_renderers(move |width, height| {
            // A slow first frame.
            clock.set_ms(clock.now_ms() + 60.0);
            let mut renderer = BitmapRenderer::new(width, height)?;
            renderer.set_text_settings(TextSettings::reference())?;
            Ok(Box::new(renderer) as Box<dyn Renderer>)
        })
        .unwrap();
    f.window.open_menu(model(), anchor()).unwrap();
    let popup = popups(f.hwnd())[0];
    let shown = height(popup);
    f.window.motion_tick().unwrap();
    assert_eq!(
        height(popup),
        shown,
        "the reveal lost the time spent drawing"
    );
    f.window.close_menu().unwrap();
}

#[test]
fn opening_a_menu_marks_no_part_of_the_document_for_repaint() {
    let f = fixture(Some(true));
    let content = f.window.content_rect().unwrap();
    f.window.invalidated_rects().unwrap();
    f.window.open_menu(model(), anchor()).unwrap();
    let top = content.y.0 as i32;
    let touched: Vec<RECT> = f
        .window
        .invalidated_rects()
        .unwrap()
        .into_iter()
        .filter(|b| b.bottom > top)
        .collect();
    assert!(touched.is_empty(), "{touched:?}");
    f.window.close_menu().unwrap();
}

// ---- T1 to T6: a kept row never hides what the person works on. ----

/// A row kept beyond its normal scroll limit: at the end of the scroll, after one X.
fn kept_row() -> (TabStrip, f32, f32) {
    let mut s = narrow_strip(40);
    wheel_right(&mut s, 60);
    let index = s.tabs().len() - 4;
    let (x, y) = visible_x(&s, index);
    click(&mut s, x, y, MouseButton::Left);
    assert!(s.width_is_held());
    (s, x, y)
}
fn in_view(s: &TabStrip) -> Vec<usize> {
    (0..s.tabs().len())
        .filter(|i| s.tab_shows_whole(*i))
        .collect()
}

#[test]
fn a_kept_row_never_shows_no_tabs_and_never_takes_the_selected_tab_out_of_view() {
    let mut s = narrow_strip(40);
    let ids = ids_of(&s);
    s.select(ids[39]);
    wheel_right(&mut s, 60);
    let index = in_view(&s)[0];
    let (x, y) = visible_x(&s, index);
    for round in 0..30 {
        if !matches!(s.hit_test(at(x, y)), CaptionHit::TabClose(_)) {
            break;
        }
        click(&mut s, x, y, MouseButton::Left);
        assert!(!in_view(&s).is_empty(), "round {round}: no tabs show");
        assert!(
            s.tab_shows_whole(s.active()),
            "round {round}: the selected tab {} is out of view, {:?}",
            s.active(),
            s.tab_rects()[s.active()]
        );
    }
}

#[test]
fn a_kept_row_with_the_selected_tab_scrolled_away_still_shows_some_tab() {
    let mut s = narrow_strip(40);
    let ids = ids_of(&s);
    s.select(ids[0]);
    wheel_right(&mut s, 60);
    let index = in_view(&s)[0];
    let (x, y) = visible_x(&s, index);
    for round in 0..30 {
        if !matches!(s.hit_test(at(x, y)), CaptionHit::TabClose(_)) {
            break;
        }
        click(&mut s, x, y, MouseButton::Left);
        assert!(!in_view(&s).is_empty(), "round {round}: no tabs show");
    }
}

#[test]
fn a_middle_click_on_a_cut_off_selected_tab_leaves_the_next_selected_tab_whole() {
    let mut s = narrow_strip(40);
    let ids = ids_of(&s);
    s.select(ids[20]);
    for _ in 0..30 {
        if !s.tab_shows_whole(20) && s.tab_rects()[20].x.0 > 100.0 {
            break;
        }
        event(
            &mut s,
            InputKind::Wheel {
                at: at(200.0, 20.0),
                delta_x: -1.0,
                delta_y: 0.0,
                modifiers: Modifiers::default(),
            },
        );
    }
    let cut = s.tab_rects()[20];
    assert!(!s.tab_shows_whole(20) && cut.x.0 > 100.0, "{cut:?}");
    click(&mut s, cut.x.0 + 10.0, 20.0, MouseButton::Middle);
    assert_eq!(s.tabs().len(), 39);
    assert!(
        s.tab_shows_whole(s.active()),
        "{:?}",
        s.tab_rects()[s.active()]
    );
}

#[test]
fn a_new_frame_mode_ends_a_kept_row_for_good() {
    let mut s = just_overflowing();
    wheel_right(&mut s, 1);
    let index = s.tabs().len() - 3;
    let (x, y) = visible_x(&s, index);
    click(&mut s, x, y, MouseButton::Left);
    assert!(s.width_is_held());
    s.set_maximized(true);
    assert!(!s.width_is_held());
    assert!(!s.overflow(), "the arrows stayed");
    assert!(s.tab_rects()[0].x.0 >= 0.0, "{:?}", s.tab_rects()[0]);
    s.pointer_left(&mut ViewContext::default());
    assert!(!s.overflow());
    assert!(s.tab_rects()[0].x.0 >= 0.0, "{:?}", s.tab_rects()[0]);
}

#[test]
fn leaving_a_kept_row_rearranges_it() {
    let (mut s, ..) = kept_row();
    let kept_first = s.tab_rects()[0].x.0;
    s.pointer_left(&mut ViewContext::default());
    assert!(!s.width_is_held());
    assert!(s.tab_rects()[0].x.0 > kept_first, "the row fell back");
}

#[test]
fn a_wheel_or_an_arrow_on_a_kept_row_ends_the_keeping_first() {
    for arrow in [false, true] {
        let (mut s, ..) = kept_row();
        let kept_first = s.tab_rects()[0].x.0;
        if arrow {
            let y = s.tab_rects()[0].y.0 + 10.0;
            let x = (0..1000)
                .map(|x| x as f32)
                .find(|x| s.hit_test(at(*x, y)) == CaptionHit::ScrollRight)
                .expect("the right arrow");
            click(&mut s, x, y, MouseButton::Left);
        } else {
            wheel_right(&mut s, 1);
        }
        assert!(!s.width_is_held(), "arrow={arrow}");
        // The normal limit holds again, and the row is what a plain row of 39 shows.
        let mut plain = narrow_strip(39);
        wheel_right(&mut plain, 60);
        assert_eq!(
            s.tab_rects()[0].x.0,
            plain.tab_rects()[0].x.0,
            "arrow={arrow}, kept at {kept_first}"
        );
    }
}

#[test]
fn a_new_menu_that_fails_after_the_old_one_ended_still_ends_a_hold() {
    let f = fixture(Some(true));
    hold_then_menu(&f);
    f.window.set_test_cursor(Some((-50, -50))).unwrap();
    f.window.fail_next_menu().unwrap();
    assert!(f.window.open_menu(model(), anchor()).is_err());
    assert!(!f.window.menu_is_open().unwrap());
    assert_eq!(f.window.leave_notice_armed().unwrap(), "none");
    assert!(!f.held(), "nothing is left to end this hold");
    // With the pointer still on the row, a notice is on its way instead.
    let f = fixture(Some(true));
    hold_then_menu(&f);
    f.window.fail_next_menu().unwrap();
    assert!(f.window.open_menu(model(), anchor()).is_err());
    assert_eq!(f.window.leave_notice_armed().unwrap(), "client");
}
