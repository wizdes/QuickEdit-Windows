use qe_api::*;

#[test]
fn reference_text_tracks_mixed_breaks_and_utf16() {
    let mut text = testing::StringStore::new("a😀\r\nb\u{2028}c");
    assert_eq!(text.line_count(), 3);
    assert_eq!(text.byte_to_utf16(ByteOffset(5)), Some(Utf16Offset(3)));
    assert_eq!(text.utf16_to_byte(Utf16Offset(3)), Some(ByteOffset(5)));
    let change = text.replace(ByteOffset(1)..ByteOffset(5), "x").unwrap();
    assert_eq!(change.new_len, 1);
    assert_eq!(text.revision(), 1);
}

#[test]
fn default_shortcuts_are_distinct_and_altgr_safe() {
    let mut seen = std::collections::HashSet::new();
    for command in CommandId::ALL {
        if let Some(key) = command.default_key() {
            assert!(seen.insert(key), "duplicate shortcut: {command:?}");
            assert!(
                !(key.ctrl && key.alt && key.is_letter()),
                "AltGr clash: {command:?}"
            );
        }
    }
}

#[test]
fn fixed_layout_uses_character_boundaries() {
    let text = testing::StringStore::new("a😀b");
    let layout = testing::FixedWidthLayout {
        cell_width: Dip(10.0),
        row_height: Dip(20.0),
    };
    let line = layout.layout_line(&text, LineIndex(0), Dip(20.0)).unwrap();
    assert_eq!(line.rows.len(), 2);
    assert_eq!(
        layout.hit_test_point(
            &line,
            Point {
                x: Dip(20.0),
                y: Dip(0.0)
            }
        ),
        ByteOffset(5)
    );
    assert_eq!(
        layout.caret_rect(&line, ByteOffset(1)).unwrap().x,
        Dip(10.0)
    );
}

#[test]
fn reference_store_rejects_invalid_ranges_and_read_only_edits() {
    let mut text = testing::StringStore::new("😀\r\nx\u{85}");
    assert_eq!(text.line_count(), 3);
    assert_eq!(
        text.line_range(LineIndex(0)),
        Some(ByteOffset(0)..ByteOffset(6))
    );
    assert_eq!(text.utf16_to_byte(Utf16Offset(1)), None);
    assert_eq!(
        text.replace(ByteOffset(1)..ByteOffset(4), "x"),
        Err(EditError::InvalidRange)
    );
    text.set_read_only(true);
    assert_eq!(
        text.replace(ByteOffset(0)..ByteOffset(4), "x"),
        Err(EditError::ReadOnly)
    );
}

#[test]
fn replay_parses_and_checks_results() {
    let script = "# demo\nevent\t10\tchar\t\\u{1F600}\nexpect\ttext\t\\u{1F600}\n";
    let steps = parse_replay(script).unwrap();
    let value = std::cell::RefCell::new(String::new());
    run_replay(
        &steps,
        |event| {
            if let InputKind::Char(c) = event.kind {
                value.borrow_mut().push(c);
            }
        },
        |_| Some(value.borrow().clone()),
    )
    .unwrap();
    assert_eq!(parse_replay("event\t1\tunknown").unwrap_err().line, 1);
    assert!(parse_replay("event\t1\tchar\t\\u{1F600").is_err());
}

#[test]
fn theme_and_display_text_are_stable() {
    assert_eq!(
        Theme::new(ThemeKind::Light).text_background,
        Color::rgb(255, 255, 255)
    );
    assert_eq!(
        Theme::new(ThemeKind::Dark).text_background,
        Color::rgb(30, 30, 30)
    );
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRect {
        rect: Rect {
            x: Dip(1.0),
            y: Dip(2.0),
            width: Dip(3.0),
            height: Dip(4.0),
        },
        color: Color::rgb(0, 0, 0),
    });
    assert_eq!(
        list.to_stable_text(),
        "display-list-v1\nfill 1.000,2.000,3.000,4.000 #000000ff\n"
    );
}

#[test]
fn settings_and_menu_paths_cover_all_entries() {
    let mut paths = std::collections::HashSet::new();
    for command in CommandId::ALL {
        assert!(
            paths.insert(command.menu_path()),
            "duplicate path: {command:?}"
        );
        assert!(command.menu_path().contains('/'));
    }
    assert_eq!(
        SettingKey::PlainTextFont.mac_default(),
        SettingValue::Text("System fixed-pitch 13 pt")
    );
    assert_eq!(
        SettingKey::WindowWidthCharacters.mac_default(),
        SettingValue::Integer(90)
    );
    assert_eq!(SettingKey::TabWidth.mac_default(), SettingValue::Integer(8));
    assert!(SettingKey::ALL.len() >= 19);
}

#[test]
fn replay_supports_every_event_kind() {
    let script = concat!(
        "event\t1\tkey_down\t65\tCS\tfalse\n",
        "event\t2\tkey_up\t65\tCS\n",
        "event\t3\time_start\n",
        "event\t4\time_update\tx\t1\n",
        "event\t5\time_end\tx\n",
        "event\t6\tmouse_move\t1\t2\n",
        "event\t7\tmouse_down\t1\t2\tleft\t-\n",
        "event\t8\tmouse_up\t1\t2\tleft\t-\n",
        "event\t9\twheel\t1\t2\t0\t120\t-\n",
        "event\t10\tfocus\n",
        "event\t11\tblur\n",
        "event\t12\tdpi\t144\n",
        "event\t13\tresize\t800\t600\n"
    );
    assert_eq!(parse_replay(script).unwrap().len(), 13);
}

#[test]
fn all_mac_line_breaks_and_chunk_reads_work() {
    let text = testing::StringStore::new("a\nb\rc\r\nd\u{85}e\u{2028}f\u{2029}g");
    assert_eq!(text.line_count(), 7);
    let mut read = String::new();
    assert!(
        text.visit_chunks(ByteOffset(2)..ByteOffset(6), &mut |chunk| read
            .push_str(chunk))
    );
    assert_eq!(read, "b\rc\r");
    for line in 0..text.line_count() {
        let start = text.line_range(LineIndex(line)).unwrap().start;
        assert_eq!(text.byte_to_position(start).unwrap().line, LineIndex(line));
        assert_eq!(
            text.position_to_byte(TextPosition {
                line: LineIndex(line),
                column: Utf16Offset(0)
            }),
            Some(start)
        );
    }
}

#[test]
fn replay_rejects_malformed_key_repeat_and_extra_fields() {
    assert!(parse_replay("event\t1\tkey_down\t65\t-\tmaybe").is_err());
    assert!(parse_replay("event\t1\tfocus\textra").is_err());
}
