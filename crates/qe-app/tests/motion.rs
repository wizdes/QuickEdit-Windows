//! The tab row's motion through the whole app: real posted mouse messages on a hidden window,
//! through the store and `sync_tabs`, on a motion clock that the test sets.
mod support;
use support::*;

const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_MBUTTONDOWN: u32 = 0x0207;
const WM_MBUTTONUP: u32 = 0x0208;
const WM_MOUSELEAVE: u32 = 0x02A3;
const MK_LBUTTON: usize = 1;
const MK_MBUTTON: usize = 0x10;

#[derive(Debug)]
struct Strip {
    scale: f32,
    width: f32,
    held: bool,
    active: usize,
    y: f32,
    close: f32,
    hover: String,
    titles: Vec<String>,
    last: Vec<(f32, f32)>,
    drawn: Vec<(f32, f32)>,
    ghosts: Vec<(f32, f32, String)>,
}
fn places(text: &str) -> Vec<(f32, f32)> {
    text.split(',')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (x, width) = p.split_once(':').expect("x:width");
            (x.parse().expect("x"), width.parse().expect("width"))
        })
        .collect()
}
fn strip(app: &mut App) -> Strip {
    let line = app.ask("strip");
    let field = |name: &str| {
        line.split(' ')
            .find_map(|part| part.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("no {name} in {line}"))
            .to_owned()
    };
    Strip {
        scale: field("scale").parse().expect("scale"),
        width: field("width").parse().expect("width"),
        held: field("held") == "true",
        active: field("active").parse().expect("active"),
        y: field("y").parse().expect("y"),
        close: field("close").parse().expect("close"),
        hover: field("hover"),
        titles: field("titles").split(',').map(decode).collect(),
        last: places(&field("final")),
        drawn: places(&field("drawn")),
        ghosts: field("ghosts")
            .split(',')
            .filter(|g| !g.is_empty())
            .map(|g| {
                let mut parts = g.split(':');
                let x = parts.next().expect("x").parse().expect("x");
                let width = parts.next().expect("width").parse().expect("width");
                (x, width, decode(parts.next().expect("title")))
            })
            .collect(),
    }
}
fn pack(x: i32, y: i32) -> i64 {
    i64::from((y as i16 as u16 as u32) << 16 | (x as i16 as u16 as u32))
}
/// The middle of this tab's X, in client pixels.
fn x_point(row: &Strip, index: usize) -> (i32, i32) {
    let (x, width) = row.last[index];
    (
        ((x + width - row.close / 2.0) * row.scale) as i32,
        ((row.y + 10.0) * row.scale) as i32,
    )
}
/// A click, as Windows delivers it: a move, a press and a release, through the real queue.
fn click(app: &mut App, at: (i32, i32), middle: bool) {
    let (down, up, held) = if middle {
        (WM_MBUTTONDOWN, WM_MBUTTONUP, MK_MBUTTON)
    } else {
        (WM_LBUTTONDOWN, WM_LBUTTONUP, MK_LBUTTON)
    };
    let l = pack(at.0, at.1);
    app.ok(&format!("post {WM_MOUSEMOVE} 0 {l}"));
    app.ok(&format!("post {down} {held} {l}"));
    app.ok(&format!("post {up} 0 {l}"));
}
fn wait_tabs(app: &mut App, count: usize) {
    app.wait_for("the row to have its tabs", |app| {
        strip(app).titles.len() == count
    });
}
/// An app with four named tabs, its list at rest and its motion on.
fn four_tabs(folder: &Folder) -> App {
    let paths: Vec<_> = ["a.txt", "b.txt", "c.txt", "d.txt"]
        .iter()
        .map(|name| folder.file(name, name.as_bytes()))
        .collect();
    let files: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    let mut app = App::start(&folder.root(), &files);
    app.settled();
    app.wait_for("four tabs with their names", |app| {
        let titles = strip(app).titles;
        titles.len() == 4 && titles.iter().all(|t| t.ends_with(".txt"))
    });
    app.ok("motion-on");
    app.ok("motion-time 5000");
    app
}

#[test]
fn closing_by_mouse_through_the_app_fades_in_place_holds_widths_and_the_next_click_closes_the_next()
{
    for middle in [false, true] {
        let folder = Folder::new("close-by-mouse");
        let mut app = four_tabs(&folder);
        let before = strip(&mut app);
        assert!(before.width < 240.0, "the row has room: {before:?}");
        let at = x_point(&before, 1);
        app.ok(&format!("test-cursor {} {}", at.0, at.1));

        click(&mut app, at, middle);
        wait_tabs(&mut app, 3);
        let closed = strip(&mut app);
        // The closed tab fades where it was. Nothing has jumped.
        assert_eq!(closed.titles, ["a.txt", "c.txt", "d.txt"], "{middle}");
        assert_eq!(
            closed.ghosts,
            [(before.last[1].0, before.last[1].1, "b.txt".to_owned())],
            "the closed tab fades in the second slot, middle={middle}"
        );
        assert_eq!(
            closed.width, before.width,
            "the widths stay, middle={middle}"
        );
        assert!(closed.held, "middle={middle}");
        assert_eq!(closed.drawn[0], before.last[0], "the first tab stays");
        assert_eq!(closed.drawn[1], before.last[2], "c starts where it was");
        assert_eq!(closed.drawn[2], before.last[3], "d starts where it was");
        // The third tab now has its X under the pointer.
        assert_eq!(closed.last[1], before.last[1]);
        assert_eq!(closed.hover, "TabClose(1)", "middle={middle}");
        // The next frames do not move the widths.
        app.ok("motion-time 16");
        assert_eq!(strip(&mut app).width, before.width);
        app.ok("motion-time 300");

        // Again at the same spot: the next tab closes, and is not merely selected.
        click(&mut app, at, middle);
        wait_tabs(&mut app, 2);
        let again = strip(&mut app);
        assert_eq!(again.titles, ["a.txt", "d.txt"], "middle={middle}");
        assert_eq!(again.width, before.width);
        assert!(again.held);
        app.ok("motion-time 300");

        // Leaving the row lets the widths grow.
        app.ok("test-cursor -1 -1");
        app.ok(&format!("post {WM_MOUSELEAVE} 0 0"));
        app.wait_for("the widths to grow", |app| !strip(app).held);
        assert!(strip(&mut app).width > before.width, "middle={middle}");
        app.quit();
    }
}

/// Every tab left in the row starts from where it was drawn, whatever the store did.
fn no_jump(before: &Strip, after: &Strip, what: &str) {
    for (i, title) in after.titles.iter().enumerate() {
        let Some(was) = before.titles.iter().position(|t| t == title) else {
            continue;
        };
        assert_eq!(
            after.drawn[i], before.drawn[was],
            "{what}: {title} jumped: {before:?} and {after:?}"
        );
    }
}

#[test]
fn going_through_sync_tabs_starts_no_slide_that_looks_like_a_jump() {
    let folder = Folder::new("no-jump");
    let mut app = four_tabs(&folder);

    // The active tab closes with no pointer, in the middle of the row.
    app.ok("invoke-tab 1");
    app.ok("motion-time 3000");
    let before = strip(&mut app);
    let closing = before.active;
    assert!(closing < 3, "{before:?}");
    app.ok("tab-x");
    wait_tabs(&mut app, 3);
    let after = strip(&mut app);
    no_jump(&before, &after, "closing the active tab");
    assert_eq!(after.ghosts.len(), 1, "{after:?}");
    assert_eq!(after.ghosts[0].0, before.last[closing].0);
    assert!(!after.held, "a close with no pointer holds nothing");
    assert_eq!(after.active, closing, "the next tab takes over");
    app.ok("motion-time 3000");

    // A new tab: the others slide from their places, and the new one is last.
    let before = strip(&mut app);
    app.run("NewTab");
    wait_tabs(&mut app, 4);
    let after = strip(&mut app);
    no_jump(&before, &after, "a new tab");
    assert_eq!(after.active, 3);
    app.quit();
}

#[test]
fn a_tab_whose_x_click_got_cancel_later_closes_by_menu_with_no_hold() {
    let folder = Folder::new("cancel-then-menu");
    let mut app = four_tabs(&folder);
    app.ok("invoke-tab 1");
    app.ok("motion-time 3000");
    app.type_text("unsaved");
    let row = strip(&mut app);
    assert_eq!(row.active, 1);
    let at = x_point(&row, 1);
    app.ok(&format!("test-cursor {} {}", at.0, at.1));
    click(&mut app, at, false);
    app.press("Cancel");
    app.settled();
    assert_eq!(strip(&mut app).titles.len(), 4);
    // The same tab, by the Close command this time.
    app.run("Close");
    app.press("Don't Save");
    wait_tabs(&mut app, 3);
    let row = strip(&mut app);
    assert!(!row.held, "the mouse did not make this close: {row:?}");
    assert!(row.width > 0.0);
    app.quit();
}

/// The store's tabs in each window, and which one it calls active.
fn store_order(app: &mut App) -> Vec<(Vec<String>, Option<usize>)> {
    app.windows()
        .into_iter()
        .map(|tabs| {
            let active = tabs.iter().position(|t| t.ends_with('*'));
            let names = tabs.iter().map(|t| t.trim_end_matches('*').to_owned());
            (names.collect(), active)
        })
        .collect()
}
/// The row and the store say the same about this window.
fn agree(app: &mut App, window: usize) {
    let store = store_order(app).swap_remove(window);
    let row = strip(app);
    assert_eq!(store.0, row.titles, "the order");
    assert_eq!(store.1, Some(row.active), "the active tab");
}

#[test]
fn a_drag_a_move_between_windows_a_close_and_a_restart_keep_the_order_and_the_active_tab() {
    let folder = Folder::new("order");
    let mut app = four_tabs(&folder);
    let row = strip(&mut app);
    let y = ((row.y + 10.0) * row.scale) as i32;
    let middle = |place: (f32, f32)| ((place.0 + place.1 / 2.0) * row.scale) as i32;
    let (from, to) = (pack(middle(row.last[0]), y), pack(middle(row.last[2]), y));
    // A real drag of the first tab to the third place.
    app.ok(&format!("post {WM_MOUSEMOVE} 0 {from}"));
    app.ok(&format!("post {WM_LBUTTONDOWN} {MK_LBUTTON} {from}"));
    app.ok(&format!("post {WM_MOUSEMOVE} {MK_LBUTTON} {to}"));
    app.ok(&format!("post {WM_LBUTTONUP} 0 {to}"));
    app.wait_for("the drag to reach the store", |app| {
        app.windows()[0].first().is_some_and(|t| t == "b.txt")
    });
    let row = strip(&mut app);
    assert_eq!(row.titles, ["b.txt", "c.txt", "a.txt", "d.txt"]);
    assert_eq!(row.active, 2, "the dragged tab");
    agree(&mut app, 0);
    app.ok("motion-time 3000");

    // The active tab closes, and the store and the row agree on who is next.
    app.ok("tab-x");
    wait_tabs(&mut app, 3);
    let row = strip(&mut app);
    assert_eq!(row.titles, ["b.txt", "c.txt", "d.txt"]);
    agree(&mut app, 0);

    // The active tab moves to a new window's first place.
    let row = strip(&mut app);
    let moved = row.titles[row.active].clone();
    app.run("New");
    app.wait_for("a second window", |app| app.windows().len() == 2);
    app.ok("use-window 0");
    app.ok("tab-into 0");
    app.wait_for("the tab to move", |app| {
        store_order(app)[0].0.len() == 2 && store_order(app)[1].0.len() == 2
    });
    let windows = store_order(&mut app);
    assert!(!windows[0].0.contains(&moved), "{windows:?}");
    assert_eq!(windows[1].0[0], moved, "{windows:?}");
    assert_eq!(windows[1].1, Some(0), "the moved tab is active there");
    app.ok("use-window 1");
    agree(&mut app, 1);
    app.quit();

    // A restored session: the row shows the store's order, with the store's active tab.
    let mut app = App::start(&folder.root(), &[]);
    app.settled();
    let windows = store_order(&mut app);
    let tabs: usize = windows.iter().map(|w| w.0.len()).sum();
    assert_eq!(tabs, 3, "{windows:?}");
    let row = strip(&mut app);
    let at = windows.iter().position(|w| w.0 == row.titles);
    assert!(at.is_some(), "{windows:?} and {row:?}");
    assert_eq!(windows[at.unwrap()].1, Some(row.active));
    app.quit();
}

#[test]
fn bad_input_on_the_test_channel_gets_an_error() {
    let folder = Folder::new("bad-channel");
    let mut app = App::start(&folder.root(), &[]);
    app.ok("motion-on");
    for line in [
        "motion-time",
        "motion-time soon",
        "motion-time 1 2",
        "test-cursor",
        "test-cursor 5",
        "test-cursor 5 six",
        "test-cursor 1 2 3",
    ] {
        let answer = app.ask(line);
        assert!(answer.starts_with("error"), "{line}: {answer}");
    }
    app.ok("test-cursor 5 6");
    app.ok("test-cursor none");
    app.ok("motion-time 16");
    app.quit();
}

#[test]
fn the_only_tab_of_a_row_goes_through_sync_tabs_with_the_new_one_added_first() {
    // The first blank tab gives way to an opened file: one tab gone and one new in one change.
    let folder = Folder::new("only-tab");
    let file = folder.file("only.txt", b"only");
    let mut app = App::start(&folder.root(), &[]);
    app.settled();
    assert_eq!(strip(&mut app).titles.len(), 1);
    app.ok("motion-on");
    app.ok("motion-time 5000");
    app.ok(&format!("open {}", encode(&file.to_string_lossy())));
    app.wait_for("the file's tab", |app| strip(app).titles == ["only.txt"]);
    agree(&mut app, 0);
    assert_eq!(strip(&mut app).titles.len(), 1);

    // The X of the only tab closes the window, and with it the app, with a clean exit.
    app.ok("motion-time 300");
    let row = strip(&mut app);
    let at = x_point(&row, 0);
    app.ok(&format!("test-cursor {} {}", at.0, at.1));
    click(&mut app, at, false);
    app.wait_exit();
}

#[test]
fn new_tab_title_middle_click_closes_the_drawn_tab() {
    let folder = Folder::new("new-title-overlap");
    let mut app = four_tabs(&folder);
    app.run("NewTab");
    wait_tabs(&mut app, 5);
    app.ok("motion-time 8");
    let row = strip(&mut app);
    let (x, _) = row.drawn[4];
    let at = (
        ((x + 20.0) * row.scale) as i32,
        ((row.y + 10.0) * row.scale) as i32,
    );
    click(&mut app, at, true);
    wait_tabs(&mut app, 4);
    assert_eq!(strip(&mut app).titles, ["a.txt", "b.txt", "c.txt", "d.txt"]);
    app.quit();
}
