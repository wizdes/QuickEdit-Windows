use qe_api::*;

#[test]
fn snapshot_survives_later_edit() {
    let mut store = testing::StringStore::new("old");
    let before = store.snapshot();
    store.replace(ByteOffset(0)..ByteOffset(3), "new").unwrap();
    let mut copied = String::new();
    assert!(
        before.visit_chunks(ByteOffset(0)..ByteOffset(3), &mut |part| copied
            .push_str(part))
    );
    assert_eq!(copied, "old");
    assert_eq!(before.revision(), 0);
    assert_eq!(store.revision(), 1);
}

#[test]
fn slice_layout_reads_only_requested_bytes() {
    let store = testing::StringStore::new("ab😀cdef\nnext");
    let layout = testing::FixedWidthLayout {
        cell_width: Dip(10.0),
        row_height: Dip(20.0),
    };
    let result = layout
        .layout_slice(&store, ByteOffset(2)..ByteOffset(7), Dip(20.0))
        .unwrap();
    assert_eq!(result.line, LineIndex(0));
    assert_eq!(result.rows[0].range, ByteOffset(2)..ByteOffset(7));
    assert_eq!(
        result.rows[0]
            .caret_stops
            .iter()
            .map(|s| s.byte)
            .collect::<Vec<_>>(),
        vec![ByteOffset(2), ByteOffset(6), ByteOffset(7)]
    );
    assert!(
        layout
            .layout_slice(&store, ByteOffset(3)..ByteOffset(7), Dip(20.0))
            .is_none()
    );
    assert!(
        layout
            .layout_slice(&store, ByteOffset(7)..ByteOffset(12), Dip(20.0))
            .is_none()
    );
}

#[test]
fn slice_layout_handles_a_100_mb_single_line() {
    let store = testing::StringStore::new("x".repeat(100 * 1024 * 1024));
    let layout = testing::FixedWidthLayout {
        cell_width: Dip(10.0),
        row_height: Dip(20.0),
    };
    let start = ByteOffset(50 * 1024 * 1024);
    let end = ByteOffset(start.0 + 4);
    let result = layout.layout_slice(&store, start..end, Dip(40.0)).unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].range, start..end);
    assert_eq!(result.rows[0].caret_stops.len(), 5);
}

#[test]
fn added_draw_commands_have_stable_text() {
    let rect = Rect {
        x: Dip(1.0),
        y: Dip(2.0),
        width: Dip(3.0),
        height: Dip(4.0),
    };
    let point = Point {
        x: Dip(1.0),
        y: Dip(2.0),
    };
    let color = Color::rgb(1, 2, 3);
    let mut list = DisplayList::default();
    list.push(DrawCommand::FillRects {
        rects: vec![rect, rect],
        color,
    });
    list.push(DrawCommand::StrokeRect {
        rect,
        radius: Dip(5.0),
        width: Dip(0.5),
        color,
    });
    list.push(DrawCommand::WavyUnderline {
        from: point,
        to: point,
        color,
    });
    assert_eq!(
        list.to_stable_text(),
        "display-list-v4\nfill-rects [1.000,2.000,3.000,4.000;1.000,2.000,3.000,4.000] #010203ff\nstroke-rect 1.000,2.000,3.000,4.000 5.000 0.500 #010203ff\nwavy-underline 1.000,2.000 1.000,2.000 #010203ff\n"
    );
}

#[test]
fn fake_measurer_returns_metrics() {
    let measure = testing::FixedTextMeasurer {
        char_width: Dip(8.0),
        height: Dip(16.0),
        baseline: Dip(12.0),
    };
    assert_eq!(
        measure.measure("a😀", "body"),
        TextMetrics {
            width: Dip(16.0),
            height: Dip(16.0),
            baseline: Dip(12.0)
        }
    );
}

#[test]
fn renderer_contract_supports_saved_image_tests() {
    struct FakeRenderer;
    impl Renderer for FakeRenderer {
        fn begin_frame(&mut self) -> Result<(), RenderError> {
            Ok(())
        }
        fn draw(&mut self, _: &DisplayList) -> Result<(), RenderError> {
            Ok(())
        }
        fn end_frame(&mut self) -> Result<(), RenderError> {
            Ok(())
        }
        fn resize(&mut self, _: u32, _: u32) -> Result<(), RenderError> {
            Ok(())
        }
        fn set_dpi(&mut self, _: u32) -> Result<(), RenderError> {
            Ok(())
        }
        fn render_to_bitmap(
            &mut self,
            _: &DisplayList,
            width: u32,
            height: u32,
        ) -> Result<Bitmap, RenderError> {
            Ok(Bitmap {
                width,
                height,
                bgra: vec![0; width as usize * height as usize * 4],
            })
        }
    }
    let mut renderer = FakeRenderer;
    let list = DisplayList::default();
    renderer.resize(2, 3).unwrap();
    renderer.set_dpi(144).unwrap();
    renderer.begin_frame().unwrap();
    renderer.draw(&list).unwrap();
    renderer.end_frame().unwrap();
    let image = renderer.render_to_bitmap(&list, 2, 3).unwrap();
    assert_eq!((image.width, image.height, image.bgra.len()), (2, 3, 24));
    assert_eq!(RenderError::new("device lost").to_string(), "device lost");
}

#[test]
fn view_collects_requests_and_draws() {
    struct SmallView {
        bounds: Rect,
    }
    impl View for SmallView {
        fn handle(&mut self, _: &InputEvent, ctx: &mut ViewContext) -> Handled {
            ctx.run(CommandId::Save);
            ctx.invalidate(self.bounds);
            ctx.set_cursor(MouseCursor::Hand);
            ctx.capture_mouse(true);
            ctx.request_timer(25);
            ctx.set_focus(WidgetId(7));
            Handled::Yes
        }
        fn draw(&self, list: &mut DisplayList, theme: &Theme, _: &dyn TextMeasurer) {
            list.push(DrawCommand::FillRect {
                rect: self.bounds,
                color: theme.text_background,
            });
        }
        fn layout(&mut self, bounds: Rect, _: &dyn TextMeasurer) {
            self.bounds = bounds;
        }
        fn widgets(&self) -> Option<Widget> {
            None
        }
    }
    let measure = testing::FixedTextMeasurer {
        char_width: Dip(8.0),
        height: Dip(16.0),
        baseline: Dip(12.0),
    };
    let bounds = Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(10.0),
        height: Dip(10.0),
    };
    let mut view = SmallView { bounds };
    view.layout(bounds, &measure);
    let mut ctx = ViewContext::default();
    assert_eq!(
        view.handle(
            &InputEvent {
                time_us: 0,
                kind: InputKind::Focus
            },
            &mut ctx
        ),
        Handled::Yes
    );
    assert_eq!(ctx.requests.len(), 6);
    assert_eq!(ctx.requests[0], ViewRequest::Run(CommandId::Save));
    let mut list = DisplayList::default();
    view.draw(&mut list, &Theme::new(ThemeKind::Light), &measure);
    assert_eq!(list.commands.len(), 1);
    assert!(view.widgets().is_none());
}

#[test]
fn encoding_defaults_match_mac_inventory() {
    assert_eq!(
        TextEncoding::DEFAULT_LIST,
        &[
            TextEncoding::Utf16,
            TextEncoding::Utf8,
            TextEncoding::MacRoman,
            TextEncoding::Windows1252,
            TextEncoding::MacJapanese,
            TextEncoding::ShiftJis,
            TextEncoding::MacChineseTraditional,
            TextEncoding::MacKorean,
            TextEncoding::MacChineseSimplified,
            TextEncoding::Gb18030
        ]
    );
    assert_eq!(TextEncoding::Windows1252.name(), "Windows-1252");
    assert_eq!(EncodingChoice::Automatic, EncodingChoice::Automatic);
    assert!(TextEncoding::ALL.len() > TextEncoding::DEFAULT_LIST.len());
}

#[test]
fn windows_command_and_setting_choices_are_kept() {
    assert_eq!(
        CommandId::ShowSpelling.menu_path(),
        "Edit/Spelling/Show Spelling"
    );
    assert_eq!(CommandId::Print.menu_path(), "File/Print");
    assert_eq!(
        CommandId::SaveAs.default_key(),
        Some(KeyBinding::new(0x7B, false, false, false))
    );
    assert_eq!(
        SettingKey::PlainTextFont.default_value(),
        SettingValue::Text("Cascadia Mono 10")
    );
    assert_eq!(
        SettingKey::NumberPagesWhenPrinting.default_value(),
        SettingValue::Bool(false)
    );
    assert!(
        !CommandId::ALL
            .iter()
            .any(|command| command.menu_path().contains("Text Replacement"))
    );
    assert!(
        !SettingKey::ALL
            .iter()
            .any(|setting| setting.name() == "TextReplacement")
    );
}
