use qe_api::{
    Dip, DisplayList, InputEvent, InputKind, Modifiers, Point, Rect, TextMeasurer, TextMetrics,
    Theme, ThemeKind, View, ViewContext, WidgetId, WidgetRole,
};
use qe_ui::*;
use std::time::Instant;

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
fn bounds() -> Rect {
    Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(240.0),
        height: Dip(32.0),
    }
}
fn event(kind: InputKind) -> InputEvent {
    InputEvent { time_us: 0, kind }
}
fn press(v: u16) -> InputEvent {
    key(v, Modifiers::default())
}

#[test]
fn button_checkbox_and_field_keyboard() {
    let mut ctx = ViewContext::default();
    let mut b = Button::new(WidgetId(1), "Save");
    b.layout(bounds(), &Measure);
    b.handle(&event(InputKind::Focus), &mut ctx);
    b.handle(&press(0x20), &mut ctx);
    b.handle(
        &event(InputKind::KeyUp {
            virtual_key: 0x20,
            modifiers: Modifiers::default(),
        }),
        &mut ctx,
    );
    assert_eq!(b.clicks, 1);
    assert_eq!(b.widgets().unwrap().role, WidgetRole::Button);
    let mut cb = Checkbox::new(WidgetId(2), "Check");
    cb.layout(bounds(), &Measure);
    cb.handle(&event(InputKind::Focus), &mut ctx);
    cb.handle(&press(0x20), &mut ctx);
    assert!(cb.checked);
    assert!(cb.widgets().unwrap().states.checked);
    let mut f = TextField::new(WidgetId(3), "Line");
    f.layout(bounds(), &Measure);
    f.handle(&event(InputKind::Focus), &mut ctx);
    for ch in "12".chars() {
        f.handle(&event(InputKind::Char(ch)), &mut ctx);
    }
    f.handle(&press(0x08), &mut ctx);
    assert_eq!(f.text, "1");
    f.handle(&press(0x5a), &mut ViewContext { requests: vec![] });
    assert_eq!(f.text, "1");
    let mut list = DisplayList::default();
    f.draw(&mut list, &Theme::new(ThemeKind::Light), &Measure);
    assert!(list.to_stable_text().contains("text"));
}

#[test]
fn menu_keyboard_nested_and_placement() {
    let mut parent = MenuItem::command("File", qe_api::CommandId::Open);
    parent.submenu = Some(MenuModel {
        items: vec![MenuItem::command("Save", qe_api::CommandId::Save)],
    });
    let mut disabled = MenuItem::command("Exit", qe_api::CommandId::Quit);
    disabled.enabled = false;
    let model = MenuModel {
        items: vec![
            parent,
            disabled,
            MenuItem::command("About", qe_api::CommandId::About),
        ],
    };
    let mut view = MenuView::new(WidgetId(10), "Test", model);
    view.open_keyboard();
    view.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(200.0),
            height: Dip(100.0),
        },
        &Measure,
    );
    let mut ctx = ViewContext::default();
    view.handle(&press(0x28), &mut ctx);
    assert_eq!(view.selected, Some(2));
    view.handle(&press(0x28), &mut ctx);
    assert_eq!(view.selected, Some(0));
    view.handle(&press(0x27), &mut ctx);
    assert!(view.child.is_some());
    view.handle(&press(0x25), &mut ctx);
    assert!(view.child.is_none());
    view.handle(&event(InputKind::Char('a')), &mut ctx);
    assert_eq!(view.selected, Some(2));
    view.handle(&press(0x0d), &mut ctx);
    assert!(
        ctx.requests
            .contains(&qe_api::ViewRequest::Run(qe_api::CommandId::About))
    );
    let work = Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(300.0),
        height: Dip(200.0),
    };
    let anchor = Rect {
        x: Dip(270.0),
        y: Dip(160.0),
        width: Dip(20.0),
        height: Dip(20.0),
    };
    let placed = place_popup(anchor, (100.0, 80.0), work, true);
    assert_eq!(placed.x.0, 170.0);
    assert!(placed.y.0 + placed.height.0 <= 200.0);
}

#[test]
fn alt_truth_table() {
    let alt = |down: bool, ctrl: bool| {
        event(if down {
            InputKind::KeyDown {
                virtual_key: 0xa4,
                modifiers: Modifiers {
                    ctrl,
                    ..Default::default()
                },
                repeat: false,
            }
        } else {
            InputKind::KeyUp {
                virtual_key: 0xa4,
                modifiers: Modifiers {
                    ctrl,
                    ..Default::default()
                },
            }
        })
    };
    let mut t = AltKeyTracker::default();
    assert!(!t.event(&alt(true, false)));
    assert!(t.event(&alt(false, false)));
    assert!(!t.event(&alt(false, false)));
    t.event(&alt(true, true));
    assert!(!t.event(&alt(false, true)));
    t.event(&alt(true, false));
    t.event(&press(0x09));
    assert!(!t.event(&alt(false, false)));
}

#[test]
fn table_virtualizes_and_has_rows() {
    let rows = (0..500)
        .map(|i| TableRow {
            label: format!("Row {i}"),
            detail: String::new(),
            section: i % 30 == 0,
            indent: i % 3,
        })
        .collect();
    let mut table = Table::new(WidgetId(20), "Test", vec!["Command".into()], rows);
    table.layout(
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(300.0),
            height: Dip(200.0),
        },
        &Measure,
    );
    table.handle(&event(InputKind::Focus), &mut ViewContext::default());
    let start = Instant::now();
    let mut list = DisplayList::default();
    table.draw(&mut list, &Theme::new(ThemeKind::Light), &Measure);
    let elapsed = start.elapsed();
    println!("500-row table draw: {elapsed:?}");
    assert!(elapsed.as_millis() < 1, "table draw took {elapsed:?}");
    assert!(table.visible_range().len() < 10);
    assert!(list.commands.len() < 25);
    assert_eq!(
        table.widgets().unwrap().children.len(),
        table.visible_range().len()
    );
}

#[test]
fn theme_lists_and_uia() {
    let mut b = Button::new(WidgetId(30), "OK");
    b.layout(bounds(), &Measure);
    for kind in [ThemeKind::Light, ThemeKind::Dark] {
        let mut list = DisplayList::default();
        b.draw(&mut list, &Theme::new(kind), &Measure);
        assert!(list.commands.len() > 2)
    }
    let mut tabs = TabBar::new(
        WidgetId(31),
        "Test",
        vec!["New Document".into(), "Keyboard".into()],
    );
    tabs.layout(bounds(), &Measure);
    let tree = tabs.widgets().unwrap();
    assert_eq!(tree.role, WidgetRole::TabList);
    assert_eq!(tree.children.len(), 2);
    assert!(tree.children[0].states.selected);
    let mut combo = ComboBox::new(
        WidgetId(32),
        "Encoding",
        vec![Some("UTF-8".into()), None, Some("UTF-16".into())],
    );
    combo.layout(bounds(), &Measure);
    combo.handle(&event(InputKind::Focus), &mut ViewContext::default());
    combo.handle(&press(0x28), &mut ViewContext::default());
    assert_eq!(combo.selected, 2);
    assert_eq!(combo.widgets().unwrap().value.as_deref(), Some("UTF-16"));
    let mut scroll = ScrollBar::new(WidgetId(33), 500.0, 50.0);
    scroll.layout(bounds(), &Measure);
    assert!(scroll.widgets().is_some());
    let _ = Point::default();
    // Each theme paints the control differently, and the focused tab is one node.
    let mut light = DisplayList::default();
    let mut dark = DisplayList::default();
    let mut contrast = DisplayList::default();
    b.draw(&mut light, &Theme::new(ThemeKind::Light), &Measure);
    b.draw(&mut dark, &Theme::new(ThemeKind::Dark), &Measure);
    b.draw(
        &mut contrast,
        &SystemPalette::HIGH_CONTRAST_BLACK.theme(),
        &Measure,
    );
    assert_ne!(light, dark);
    assert_ne!(dark, contrast);
    let mut ctx = ViewContext::default();
    tabs.handle(&event(InputKind::Focus), &mut ctx);
    let tree = tabs.widgets().unwrap();
    assert_eq!(focused_nodes(&tree).len(), 1);
    assert_eq!(focused_nodes(&tree)[0].role, WidgetRole::Tab);
    assert_ne!(tree.name, "Settings tabs");
    assert_eq!(tree.children[0].name, "New Document");
    combo.open = true;
    let tree = combo.widgets().unwrap();
    assert!(tree.states.expanded);
    assert_eq!(tree.children[0].children.len(), 2, "the two named rows");
    assert_ne!(scroll.widgets().unwrap().value.as_deref(), Some("0"));
}

#[test]
fn submenu_starts_on_the_selected_row_after_menu_padding() {
    let mut parent = MenuItem::command("File", qe_api::CommandId::Open);
    parent.submenu = Some(MenuModel {
        items: vec![MenuItem::command("Save", qe_api::CommandId::Save)],
    });
    let mut menu = MenuView::new(
        WidgetId(40),
        "Test",
        MenuModel {
            items: vec![parent],
        },
    );
    menu.layout(
        Rect {
            x: Dip(10.0),
            y: Dip(20.0),
            width: Dip(200.0),
            height: Dip(80.0),
        },
        &Measure,
    );
    menu.open_keyboard();
    menu.handle(&press(0x27), &mut ViewContext::default());
    assert_eq!(menu.child.as_ref().unwrap().bounds.y.0, 28.0);
}

#[test]
fn backspace_removes_a_joined_emoji_as_one_step() {
    let mut field = TextField::new(WidgetId(41), "Text");
    field.layout(bounds(), &Measure);
    let mut ctx = ViewContext::default();
    field.handle(&event(InputKind::Focus), &mut ctx);
    for ch in "👩‍💻".chars() {
        field.handle(&event(InputKind::Char(ch)), &mut ctx);
    }
    field.handle(&press(0x08), &mut ctx);
    assert_eq!(field.text, "");
}

#[test]
fn saved_display_lists() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/lists");
    for (theme_name, theme) in [
        ("light", Theme::new(ThemeKind::Light)),
        ("dark", Theme::new(ThemeKind::Dark)),
        ("contrast", SystemPalette::HIGH_CONTRAST_BLACK.theme()),
    ] {
        let mut samples: Vec<(&str, Box<dyn View>)> = Vec::new();
        let mut normal = Button::new(WidgetId(50), "Save");
        normal.layout(bounds(), &Measure);
        samples.push(("button-normal", Box::new(normal.clone())));
        normal.hovered = true;
        samples.push(("button-hover", Box::new(normal.clone())));
        normal.pressed = true;
        samples.push(("button-pressed", Box::new(normal.clone())));
        normal.pressed = false;
        normal.focused = true;
        samples.push(("button-focused", Box::new(normal.clone())));
        normal.focused = false;
        normal.enabled = false;
        samples.push(("button-disabled", Box::new(normal)));
        let mut cb = Checkbox::new(WidgetId(51), "Check spelling");
        cb.layout(bounds(), &Measure);
        samples.push(("checkbox-off", Box::new(cb.clone())));
        cb.checked = true;
        samples.push(("checkbox-on", Box::new(cb.clone())));
        cb.hovered = true;
        samples.push(("checkbox-on-hover", Box::new(cb.clone())));
        cb.pressed = true;
        samples.push(("checkbox-on-pressed", Box::new(cb.clone())));
        cb.hovered = false;
        cb.pressed = false;
        cb.focused = true;
        samples.push(("checkbox-focused", Box::new(cb.clone())));
        cb.focused = false;
        cb.enabled = false;
        samples.push(("checkbox-disabled", Box::new(cb)));
        let mut field = TextField::new(WidgetId(52), "Line");
        field.placeholder = "Line number".into();
        field.layout(bounds(), &Measure);
        samples.push(("field-placeholder", Box::new(field.clone())));
        field.text = "123".into();
        field.caret = 3;
        field.anchor = 3;
        field.focused = true;
        samples.push(("field-focused", Box::new(field.clone())));
        field.anchor = 0;
        samples.push(("field-selected", Box::new(field.clone())));
        field.anchor = 3;
        field.composition = Some(("あ".into(), 1));
        samples.push(("field-ime", Box::new(field.clone())));
        field.composition = None;
        field.enabled = false;
        samples.push(("field-disabled", Box::new(field)));
        let mut combo = ComboBox::new(
            WidgetId(53),
            "Encoding",
            vec![Some("UTF-8".into()), None, Some("UTF-16".into())],
        );
        combo.layout(bounds(), &Measure);
        samples.push(("combo-closed", Box::new(combo.clone())));
        combo.enabled = false;
        samples.push(("combo-disabled", Box::new(combo.clone())));
        combo.enabled = true;
        combo.open = true;
        samples.push(("combo-open", Box::new(combo)));
        let mut tabs = TabBar::new(WidgetId(54), "Test", vec!["New".into(), "Keyboard".into()]);
        tabs.layout(bounds(), &Measure);
        samples.push(("tabs", Box::new(tabs)));
        let mut table = Table::new(
            WidgetId(55),
            "Test",
            vec!["Command".into()],
            vec![
                TableRow {
                    label: "File".into(),
                    detail: String::new(),
                    section: true,
                    indent: 0,
                },
                TableRow {
                    label: "Save".into(),
                    detail: "Ctrl+S".into(),
                    section: false,
                    indent: 1,
                },
            ],
        );
        table.layout(
            Rect {
                x: Dip(0.0),
                y: Dip(0.0),
                width: Dip(240.0),
                height: Dip(100.0),
            },
            &Measure,
        );
        samples.push(("table", Box::new(table.clone())));
        table.focused = true;
        samples.push(("table-focused", Box::new(table)));
        let mut scroll = ScrollBar::new(WidgetId(56), 500.0, 100.0);
        scroll.layout(
            Rect {
                x: Dip(0.0),
                y: Dip(0.0),
                width: Dip(12.0),
                height: Dip(100.0),
            },
            &Measure,
        );
        samples.push(("scroll-rest", Box::new(scroll.clone())));
        scroll.hovered = true;
        samples.push(("scroll-hover", Box::new(scroll)));
        for (i, label_kind) in [
            LabelKind::Plain,
            LabelKind::Group,
            LabelKind::Hint,
            LabelKind::Warning,
        ]
        .into_iter()
        .enumerate()
        {
            let mut label = Label::new(WidgetId(57 + i as u64), "Sample", label_kind);
            label.layout(bounds(), &Measure);
            samples.push((
                match i {
                    0 => "label",
                    1 => "group",
                    2 => "hint",
                    _ => "warning",
                },
                Box::new(label),
            ));
        }
        let mut menu = MenuView::new(
            WidgetId(61),
            "Test",
            MenuModel {
                items: vec![
                    MenuItem::command("Open", qe_api::CommandId::Open),
                    MenuItem::separator(),
                    MenuItem::command("Save", qe_api::CommandId::Save),
                ],
            },
        );
        menu.layout(
            Rect {
                x: Dip(0.0),
                y: Dip(0.0),
                width: Dip(180.0),
                height: Dip(100.0),
            },
            &Measure,
        );
        samples.push(("menu", Box::new(menu.clone())));
        menu.open_keyboard();
        samples.push(("menu-keyboard", Box::new(menu)));
        for (name, view) in samples {
            let mut list = DisplayList::default();
            view.draw(&mut list, &theme, &Measure);
            let path = root.join(format!("{name}-{theme_name}.txt"));
            let actual = list.to_stable_text();
            if std::env::var_os("QE_UI_UPDATE_LISTS").is_some() {
                std::fs::create_dir_all(&root).unwrap();
                std::fs::write(&path, &actual).unwrap();
            }
            let saved = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(actual, saved, "{}", path.display());
            // A saved list stands for a picture, so no list may be only a header.
            assert!(!list.commands.is_empty(), "{name}");
        }
    }
}

#[test]
fn replay_files_drive_controls() {
    use qe_api::{ReplayStep, parse_replay};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/replays");
    let mut button = Button::new(WidgetId(70), "Save");
    button.layout(bounds(), &Measure);
    let steps = parse_replay(&std::fs::read_to_string(root.join("button.txt")).unwrap()).unwrap();
    for step in steps {
        match step {
            ReplayStep::Event(e) => {
                button.handle(&e, &mut ViewContext::default());
            }
            ReplayStep::Expect { name, value } => {
                assert_eq!(name, "clicks");
                assert_eq!(button.clicks.to_string(), value);
            }
        }
    }
    let mut field = TextField::new(WidgetId(71), "Line");
    field.layout(bounds(), &Measure);
    let steps = parse_replay(&std::fs::read_to_string(root.join("field.txt")).unwrap()).unwrap();
    for step in steps {
        match step {
            ReplayStep::Event(e) => {
                field.handle(&e, &mut ViewContext::default());
            }
            ReplayStep::Expect { name, value } => {
                assert_eq!(name, "text");
                assert_eq!(field.text, value);
            }
        }
    }
}

#[test]
fn alert_dialog_has_three_buttons_and_keyboard_order() {
    let alert = AlertModel::new(
        "Save changes?",
        "The file has edits.",
        vec![
            "Save".into(),
            "Cancel".into(),
            "Discard".into(),
            "Extra".into(),
        ],
    );
    assert_eq!(alert.buttons.len(), 3);
    assert_eq!(
        alert.dropped_buttons,
        ["Extra"],
        "a dropped button is reported"
    );
    let mut frame = DialogFrame::from_alert(
        WidgetId(80),
        &alert,
        Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(400.0),
            height: Dip(150.0),
        },
    );
    let tree = frame.widgets().unwrap();
    assert_eq!(tree.role, WidgetRole::Dialog);
    assert_eq!(tree.children.len(), 4);
    assert_eq!(tree.children[1].name, "Save");
    assert!(
        tree.children[1].states.focused,
        "the first button has focus"
    );
    assert_eq!(tree.children[1].id, WidgetId(82));
    assert_eq!(duplicate_ids(&tree), vec![]);
    let mut ctx = ViewContext::default();
    frame.handle(&press(0x09), &mut ctx);
    assert_eq!(frame.focus_index, 2);
    let tree = frame.widgets().unwrap();
    assert!(!tree.children[1].states.focused, "Save lost focus");
    assert!(tree.children[2].states.focused, "Cancel got focus");
    assert!(
        ctx.requests
            .contains(&qe_api::ViewRequest::SetFocus(WidgetId(83)))
    );
    frame.handle(&press(0x1b), &mut ctx);
    assert!(frame.closed);
    assert_eq!(frame.result(), Some(DialogResult::Dismissed));
}
