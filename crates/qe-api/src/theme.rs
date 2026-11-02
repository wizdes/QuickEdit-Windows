use crate::{Color, Dip};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeKind {
    Light,
    Dark,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub kind: ThemeKind,
    pub text_background: Color,
    pub text: Color,
    pub under_page: Color,
    pub gutter_text: Color,
    pub gutter_separator: Color,
    pub map_text: Color,
    pub map_box_fill: Color,
    pub map_box_outline: Color,
    pub warning: Color,
    /// Behind each match of a search.
    pub find_match: Color,
    /// Behind the match a search stands on.
    pub current_find_match: Color,
    /// The wavy line under a misspelled word.
    pub spelling_underline: Color,
    pub dim_text: Color,
    pub group_header: Color,
    pub tab_background: Color,
    pub tab_selected: Color,
    pub border: Color,
    pub font_family: &'static str,
    pub font_pt: f32,
    pub top_row_height: Dip,
    /// The flat menu bar below the tabs.
    pub menu_bar_height: Dip,
    pub tab_height: Dip,
    pub caption_button_width: Dip,
    pub map_width: Dip,
    pub map_line_height: Dip,
    pub map_column_width: Dip,
    pub map_block_height: Dip,
    pub gutter_left_padding: Dip,
    pub gutter_right_padding: Dip,
    pub page_margin_pt: f32,
}

impl Theme {
    pub const fn new(kind: ThemeKind) -> Self {
        let dark = matches!(kind, ThemeKind::Dark);
        let text = if dark {
            Color::rgb(255, 255, 255)
        } else {
            Color::rgb(0, 0, 0)
        };
        Self {
            kind,
            text_background: if dark {
                Color::rgb(30, 30, 30)
            } else {
                Color::rgb(255, 255, 255)
            },
            text,
            under_page: if dark {
                Color::rgb(22, 22, 22)
            } else {
                Color::rgb(238, 238, 238)
            },
            gutter_text: Color { a: 158, ..text },
            gutter_separator: Color { a: 64, ..text },
            map_text: Color { a: 115, ..text },
            map_box_fill: Color { a: 26, ..text },
            map_box_outline: Color { a: 56, ..text },
            warning: if dark {
                Color::rgb(255, 138, 120)
            } else {
                Color::rgb(179, 38, 30)
            },
            find_match: if dark {
                Color {
                    r: 150,
                    g: 120,
                    b: 0,
                    a: 170,
                }
            } else {
                Color {
                    r: 255,
                    g: 225,
                    b: 90,
                    a: 200,
                }
            },
            current_find_match: if dark {
                Color {
                    r: 214,
                    g: 110,
                    b: 0,
                    a: 220,
                }
            } else {
                Color {
                    r: 255,
                    g: 160,
                    b: 40,
                    a: 220,
                }
            },
            spelling_underline: if dark {
                Color::rgb(255, 110, 100)
            } else {
                Color::rgb(214, 40, 30)
            },
            dim_text: Color { a: 158, ..text },
            group_header: Color { a: 217, ..text },
            tab_background: if dark {
                Color::rgb(38, 38, 38)
            } else {
                Color::rgb(240, 240, 240)
            },
            tab_selected: if dark {
                Color::rgb(30, 30, 30)
            } else {
                Color::rgb(255, 255, 255)
            },
            border: Color { a: 56, ..text },
            font_family: "Cascadia Mono",
            font_pt: 10.0,
            top_row_height: Dip(32.0),
            menu_bar_height: Dip(32.0),
            tab_height: Dip(32.0),
            caption_button_width: Dip(46.0),
            map_width: Dip(100.0),
            map_line_height: Dip(2.0),
            map_column_width: Dip(1.0),
            map_block_height: Dip(1.4),
            gutter_left_padding: Dip(6.0),
            gutter_right_padding: Dip(8.0),
            page_margin_pt: 72.0,
        }
    }
}

#[cfg(test)]
mod menu_bar_tests {
    use super::{Theme, ThemeKind};

    #[test]
    fn menu_bar_height_is_one_shared_value() {
        assert_eq!(Theme::new(ThemeKind::Light).menu_bar_height.0, 32.0);
        assert_eq!(Theme::new(ThemeKind::Dark).menu_bar_height.0, 32.0);
    }
}
