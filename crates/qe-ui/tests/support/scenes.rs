//! The pictures of every control and state. The gallery and the tests paint these.
use super::RealMeasure;
use qe_api::{
    Color, CommandId, Dip, DisplayList, DrawCommand, InputEvent, InputKind, Modifiers, Rect,
    TextEncoding, Theme, ThemeKind, View, ViewContext, WidgetId,
};
use qe_ui::*;

pub type MakeTheme = fn() -> Theme;
pub const THEMES: [(&str, MakeTheme); 3] = [
    ("light", || Theme::new(ThemeKind::Light)),
    ("dark", || Theme::new(ThemeKind::Dark)),
    ("contrast", || SystemPalette::HIGH_CONTRAST_BLACK.theme()),
];

type Set<T> = fn(&mut T);

pub struct Scene {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub list: DisplayList,
}

pub fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(w),
        height: Dip(h),
    }
}
pub fn event(kind: InputKind) -> InputEvent {
    InputEvent { time_us: 0, kind }
}
pub fn press(virtual_key: u16) -> InputEvent {
    key(virtual_key, Modifiers::default())
}
fn background(list: &mut DisplayList, color: Color, w: u32, h: u32) {
    list.push(DrawCommand::FillRect {
        rect: r(0.0, 0.0, w as f32, h as f32),
        color,
    });
}
fn put(view: &mut dyn View, list: &mut DisplayList, theme: &Theme, bounds: Rect) {
    view.layout(bounds, &RealMeasure);
    view.draw(list, theme, &RealMeasure);
}
fn caption(list: &mut DisplayList, theme: &Theme, text: &str, x: f32, y: f32) {
    let mut label = Label::new(WidgetId(1), text, LabelKind::Hint);
    put(&mut label, list, theme, r(x, y, 200.0, 18.0));
}

fn controls(theme: &Theme) -> Scene {
    let (width, height) = (700, 640);
    let t = UiTokens::from_theme(theme);
    let mut l = DisplayList::default();
    background(&mut l, t.background, width, height);

    caption(&mut l, theme, "Buttons", 16.0, 8.0);
    let mut x = 16.0;
    let states: [(&str, Set<Button>); 6] = [
        ("Normal", |_| {}),
        ("Hover", |b| b.hovered = true),
        ("Pressed", |b| b.pressed = true),
        ("Focused", |b| b.focused = true),
        ("Default", |b| b.is_default = true),
        ("Disabled", |b| b.enabled = false),
    ];
    for (name, set) in states {
        let mut b = Button::new(WidgetId(2), name);
        set(&mut b);
        put(&mut b, &mut l, theme, r(x, 28.0, 92.0, 32.0));
        x += 100.0;
    }
    let mut new_tab = Button::new(WidgetId(3), "New");
    new_tab.icon = Some("E710".into());
    put(&mut new_tab, &mut l, theme, r(x, 28.0, 32.0, 32.0));

    caption(&mut l, theme, "Checkboxes", 16.0, 68.0);
    let checks: [(&str, Set<Checkbox>); 8] = [
        ("Off", |_| {}),
        ("Hover", |c| c.hovered = true),
        ("On", |c| c.checked = true),
        ("On, hover", |c| {
            c.checked = true;
            c.hovered = true
        }),
        ("On, pressed", |c| {
            c.checked = true;
            c.pressed = true
        }),
        ("On, focused", |c| {
            c.checked = true;
            c.focused = true
        }),
        ("Disabled", |c| c.enabled = false),
        ("On, disabled", |c| {
            c.checked = true;
            c.enabled = false
        }),
    ];
    for (i, (name, set)) in checks.into_iter().enumerate() {
        let mut c = Checkbox::new(WidgetId(4), name);
        set(&mut c);
        let (col, row) = (i % 4, i / 4);
        put(
            &mut c,
            &mut l,
            theme,
            r(
                16.0 + col as f32 * 150.0,
                88.0 + row as f32 * 30.0,
                140.0,
                28.0,
            ),
        );
    }

    caption(&mut l, theme, "Text fields", 16.0, 150.0);
    let memory: SharedClipboard = MemoryClipboard::shared();
    let field = |name: &str| TextField::new(WidgetId(5), name).with_clipboard(memory.clone());
    let mut empty = field("Line");
    empty.placeholder = "Line number".into();
    put(&mut empty, &mut l, theme, r(16.0, 170.0, 160.0, 32.0));
    let mut focused = field("Name");
    focused.text = "Saturday".into();
    focused.caret = 8;
    focused.anchor = 8;
    focused.focused = true;
    put(&mut focused, &mut l, theme, r(184.0, 170.0, 160.0, 32.0));
    let mut selected = focused.clone();
    selected.anchor = 3;
    put(&mut selected, &mut l, theme, r(352.0, 170.0, 160.0, 32.0));
    let mut disabled = focused.clone();
    disabled.enabled = false;
    put(&mut disabled, &mut l, theme, r(520.0, 170.0, 160.0, 32.0));
    let mut long = focused.clone();
    long.text = "A very long line of text that does not fit in the field".into();
    long.caret = long.text.len();
    long.anchor = long.caret;
    put(&mut long, &mut l, theme, r(16.0, 208.0, 160.0, 32.0));
    let mut ime = focused.clone();
    ime.text = "ab".into();
    ime.caret = 1;
    ime.anchor = 1;
    ime.composition = Some(("かな".into(), 1));
    put(&mut ime, &mut l, theme, r(184.0, 208.0, 160.0, 32.0));
    let mut plain = field("Plain");
    plain.text = "Unfocused".into();
    put(&mut plain, &mut l, theme, r(352.0, 208.0, 160.0, 32.0));

    caption(&mut l, theme, "Lists", 16.0, 248.0);
    let items = || {
        vec![
            Some("UTF-8".to_owned()),
            None,
            Some("UTF-16".to_owned()),
            Some("Mac Roman".to_owned()),
        ]
    };
    let mut closed = ComboBox::new(WidgetId(6), "Encoding", items());
    put(&mut closed, &mut l, theme, r(16.0, 268.0, 160.0, 32.0));
    let mut focused_combo = ComboBox::new(WidgetId(6), "Encoding", items());
    focused_combo.focused = true;
    put(
        &mut focused_combo,
        &mut l,
        theme,
        r(184.0, 268.0, 160.0, 32.0),
    );
    let mut hover_combo = ComboBox::new(WidgetId(6), "Encoding", items());
    hover_combo.hovered = true;
    put(
        &mut hover_combo,
        &mut l,
        theme,
        r(352.0, 268.0, 160.0, 32.0),
    );
    let mut off_combo = ComboBox::new(WidgetId(6), "Encoding", items());
    off_combo.enabled = false;
    put(&mut off_combo, &mut l, theme, r(520.0, 268.0, 160.0, 32.0));

    caption(&mut l, theme, "Tabs", 16.0, 306.0);
    let mut tabs = TabBar::new(
        WidgetId(7),
        "Test",
        vec![
            "New Document".into(),
            "Open and Save".into(),
            "Keyboard".into(),
        ],
    );
    put(&mut tabs, &mut l, theme, r(16.0, 326.0, 330.0, 32.0));
    let mut tabs2 = tabs.clone();
    tabs2.focused = true;
    tabs2.selected = 1;
    tabs2.hovered = Some(2);
    put(&mut tabs2, &mut l, theme, r(352.0, 326.0, 330.0, 32.0));

    caption(&mut l, theme, "Table and scroll bars", 16.0, 366.0);
    let row = |label: &str, detail: &str, section: bool| TableRow {
        label: label.into(),
        detail: detail.into(),
        section,
        indent: usize::from(!section),
    };
    let mut table = Table::new(
        WidgetId(8),
        "Test",
        vec!["Command".into(), "Shortcut".into()],
        vec![
            row("File", "", true),
            row("New", "Ctrl+N", false),
            row("Open…", "Ctrl+O", false),
            row("Save", "Ctrl+S", false),
            row(
                "A long command name that runs into the next column",
                "Ctrl+L",
                false,
            ),
        ],
    );
    table.focused = true;
    put(&mut table, &mut l, theme, r(16.0, 386.0, 420.0, 170.0));
    let mut bar = ScrollBar::new(WidgetId(9), 500.0, 100.0);
    put(&mut bar, &mut l, theme, r(444.0, 386.0, 12.0, 170.0));
    let mut bar2 = ScrollBar::new(WidgetId(9), 10000.0, 100.0);
    bar2.hovered = true;
    bar2.offset = 9900.0;
    put(&mut bar2, &mut l, theme, r(464.0, 386.0, 12.0, 170.0));
    let mut bar3 = ScrollBar::new(WidgetId(9), 500.0, 100.0);
    bar3.focused = true;
    bar3.offset = 200.0;
    put(&mut bar3, &mut l, theme, r(484.0, 386.0, 12.0, 170.0));

    caption(&mut l, theme, "Labels", 16.0, 562.0);
    for (i, (text, kind)) in [
        ("Plain text label", LabelKind::Plain),
        ("Group title", LabelKind::Group),
        (
            "Click a shortcut, then press the new keys.",
            LabelKind::Hint,
        ),
        ("That key is already in use.", LabelKind::Warning),
    ]
    .into_iter()
    .enumerate()
    {
        let mut label = Label::new(WidgetId(10), text, kind);
        let (col, row) = (i % 2, i / 2);
        put(
            &mut label,
            &mut l,
            theme,
            r(
                16.0 + col as f32 * 320.0,
                582.0 + row as f32 * 24.0,
                310.0,
                22.0,
            ),
        );
    }
    Scene {
        name: "controls",
        width,
        height,
        list: l,
    }
}

/// The real caret list, opened by keyboard on File, plus a mouse-opened copy.
fn caret_list(theme: &Theme) -> Scene {
    let (width, height) = (720, 520);
    let t = UiTokens::from_theme(theme);
    let mut l = DisplayList::default();
    background(&mut l, t.background, width, height);
    let model = caret_menu();
    let (w, h) = model.preferred_size(&RealMeasure);
    let mut menu = MenuView::new(WidgetId(20), "Test", model.clone()).named("All menus");
    menu.layout(r(16.0, 16.0, w, h), &RealMeasure);
    menu.open_keyboard();
    menu.handle(&press(0x27), &mut ViewContext::default());
    menu.draw(&mut l, theme, &RealMeasure);

    // A short window on the same list, to show its scrolling and a disabled row.
    let mut short = MenuModel {
        items: model.items[..5].to_vec(),
    };
    short.items[2].enabled = false;
    short.items[3].checked = true;
    short.items[3].submenu = None;
    short.items[3].command = Some(CommandId::ShowDocumentMap);
    short.items[3].key_text = "Ctrl+Shift+Alt+M".into();
    short.items[3].label = "A label too long to share a row with its shortcut".into();
    let mut narrow = MenuView::new(WidgetId(21), "Test", short);
    narrow.layout(r(450.0, 340.0, 250.0, 110.0), &RealMeasure);
    narrow.selected = Some(1);
    narrow.first_visible = 1;
    narrow.draw(&mut l, theme, &RealMeasure);
    Scene {
        name: "caret-list",
        width,
        height,
        list: l,
    }
}

fn alert(theme: &Theme) -> Scene {
    let (width, height) = (720, 200);
    let t = UiTokens::from_theme(theme);
    let mut l = DisplayList::default();
    background(&mut l, t.background, width, height);
    let model = AlertModel::new(
        "Save changes?",
        "Do you want to save the changes you made to Saturday notes.txt? Your changes will be lost if you don't save them.",
        vec!["Save".into(), "Cancel".into(), "Discard".into()],
    );
    let mut frame = DialogFrame::from_alert(WidgetId(30), &model, r(16.0, 16.0, 400.0, 150.0));
    frame.draw(&mut l, theme, &RealMeasure);
    l.push(DrawCommand::StrokeRect {
        rect: r(16.0, 16.0, 400.0, 150.0),
        radius: Dip(0.0),
        width: Dip(1.0),
        color: t.popup_border,
    });
    // Three buttons in a narrow frame stay inside it.
    let mut narrow = DialogFrame::from_alert(WidgetId(40), &model, r(440.0, 16.0, 260.0, 150.0));
    frame.handle(&press(0x09), &mut ViewContext::default());
    narrow.handle(&press(0x09), &mut ViewContext::default());
    narrow.draw(&mut l, theme, &RealMeasure);
    l.push(DrawCommand::StrokeRect {
        rect: r(440.0, 16.0, 260.0, 150.0),
        radius: Dip(0.0),
        width: Dip(1.0),
        color: t.popup_border,
    });
    Scene {
        name: "alert",
        width,
        height,
        list: l,
    }
}

/// An open list inside a dialog paints above the controls under it.
fn combo_in_dialog(theme: &Theme) -> Scene {
    let (width, height) = (420, 320);
    let t = UiTokens::from_theme(theme);
    let mut l = DisplayList::default();
    background(&mut l, t.background, width, height);
    let mut frame = DialogFrame::new(WidgetId(50), "Open");
    frame.bounds = r(16.0, 16.0, 388.0, 288.0);
    let mut items: Vec<Option<String>> = vec![Some("Automatic".into()), None];
    items.extend(
        TextEncoding::DEFAULT_LIST
            .iter()
            .map(|e| Some(e.name().to_owned())),
    );
    let mut combo = ComboBox::new(WidgetId(51), "Encoding", items);
    combo.bounds = r(32.0, 32.0, 240.0, 32.0);
    combo.disabled_items.insert(4);
    frame.add(Box::new(combo));
    let mut below = Button::new(WidgetId(52), "Below the list");
    below.bounds = r(32.0, 80.0, 240.0, 32.0);
    frame.add(Box::new(below));
    let mut ok = Button::new(WidgetId(53), "OK");
    ok.bounds = r(296.0, 256.0, 92.0, 32.0);
    frame.add(Box::new(ok));
    let mut ctx = ViewContext::default();
    frame.handle(&press(0x73), &mut ctx);
    frame.handle(&press(0x28), &mut ctx);
    frame.draw(&mut l, theme, &RealMeasure);
    Scene {
        name: "combo-in-dialog",
        width,
        height,
        list: l,
    }
}

fn encoding_panel(theme: &Theme) -> Scene {
    let (width, height) = (368, 219);
    let t = UiTokens::from_theme(theme);
    let mut l = DisplayList::default();
    background(&mut l, t.background, width, height);
    let bounds = r(16.0, 16.0, ENCODING_PANEL_WIDTH, ENCODING_PANEL_HEIGHT);
    let mut panel = EncodingPanel::new(WidgetId(60), TextEncoding::DEFAULT_LIST, bounds);
    let mut ctx = ViewContext::default();
    panel.handle(&press(0x28), &mut ctx);
    panel.handle(&press(0x28), &mut ctx);
    panel.draw(&mut l, theme, &RealMeasure);
    Scene {
        name: "encoding-panel",
        width,
        height,
        list: l,
    }
}

pub fn all(theme: &Theme) -> Vec<Scene> {
    vec![
        controls(theme),
        caret_list(theme),
        alert(theme),
        combo_in_dialog(theme),
        encoding_panel(theme),
    ]
}
