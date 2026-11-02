use qe_api::{ByteOffset, Dip, LineIndex};
use qe_editor::VisibleRow;
use qe_views::{Fade, Paper, ViewFrame, gutter_labels, gutter_width, map_runs, zoom_step};

#[test]
fn frame_reserves_side_space_and_page_wrap() {
    let measure = qe_layout::DirectWriteLayout::new().unwrap();
    let mut editor = qe_editor::EditorView::from_text("one\r\ntwo😀").unwrap();
    let mut frame = ViewFrame::new("US", "Cascadia Mono", 10.0);
    let bounds = qe_api::Rect {
        x: Dip(0.0),
        y: Dip(0.0),
        width: Dip(800.0),
        height: Dip(400.0),
    };
    let mut ctx = qe_api::ViewContext::default();
    frame.layout(&mut editor, bounds, &measure, &mut ctx);
    assert_eq!(editor.text_area().width.0, 700.0);
    frame.line_numbers = true;
    frame.sync(&mut editor, &measure, &mut ctx);
    assert!(editor.text_area().x.0 > 0.0);
    assert_eq!(editor.text_area().x.0 + editor.text_area().width.0, 700.0);
    frame.set_page_wrap(&mut editor, true, &mut ctx).unwrap();
    let mut list = qe_api::DisplayList::default();
    let theme = qe_api::Theme::new(qe_api::ThemeKind::Dark);
    frame.draw(&editor, &mut list, &theme, &measure);
    assert!(list.commands.iter().any(|command| matches!(command, qe_api::DrawCommand::FillRect { color, .. } if *color == theme.under_page)));
}

#[test]
fn gutter_numbers_only_first_rows_and_grows() {
    let rows = vec![
        VisibleRow {
            line: LineIndex(0),
            first_byte: ByteOffset(0),
            y: Dip(0.0),
            height: Dip(12.0),
            continuation: false,
        },
        VisibleRow {
            line: LineIndex(0),
            first_byte: ByteOffset(3),
            y: Dip(12.0),
            height: Dip(12.0),
            continuation: true,
        },
        VisibleRow {
            line: LineIndex(1),
            first_byte: ByteOffset(8),
            y: Dip(24.0),
            height: Dip(12.0),
            continuation: false,
        },
    ];
    assert_eq!(gutter_labels(&rows), vec![(1, 0.0), (2, 24.0)]);
    assert!(gutter_width(1000, 7.0) > gutter_width(999, 7.0));
}

#[test]
fn map_runs_keep_tabs_spaces_and_unicode_columns() {
    assert_eq!(map_runs("a\t e\u{301}😀\r\n", 98), vec![(0, 1), (9, 4)]);
    assert!(map_runs("", 98).is_empty());
}

#[test]
fn fade_has_mac_times_and_stops_idle() {
    let mut f = Fade::default();
    assert!(!f.fading());
    f.target(true, 0.0, false);
    assert!((f.step(0.075) - 0.5).abs() < 0.001);
    assert_eq!(f.step(0.15), 1.0);
    assert!(!f.fading());
    f.target(false, 1.0, false);
    assert!((f.step(1.125) - 0.5).abs() < 0.001);
    assert_eq!(f.step(1.25), 0.0);
    assert!(!f.fading());
}

#[test]
fn paper_and_zoom_rules() {
    assert_eq!(Paper::for_region("US").width, 612.0);
    assert_eq!(Paper::for_region("CA").width, 612.0);
    assert_eq!(Paper::for_region("DE").width, 595.0);
    assert_eq!(Paper::for_region("DE").text_width(), 451.0);
    assert_eq!(zoom_step(100, true), 200);
    assert_eq!(zoom_step(1600, true), 1600);
    assert_eq!(zoom_step(25, false), 25);
}
