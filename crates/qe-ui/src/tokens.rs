use qe_api::{Color, Dip, Theme, ThemeKind};

/// All measurements and colors used by painted controls.
#[derive(Clone, Copy, Debug)]
pub struct UiTokens {
    pub font: &'static str,
    pub icon_font: &'static str,
    pub text_size: Dip,
    pub row: Dip,
    pub menu_row: Dip,
    pub control_height: Dip,
    pub alert_button_width: Dip,
    pub check_size: Dip,
    pub radius: Dip,
    pub popup_radius: Dip,
    pub focus_width: Dip,
    pub inset: Dip,
    pub spacing: Dip,
    pub scrollbar_thin: Dip,
    pub scrollbar_wide: Dip,
    pub submenu_delay_ms: u64,
    pub text: Color,
    pub background: Color,
    pub surface: Color,
    pub hover: Color,
    pub pressed: Color,
    pub selected: Color,
    pub border: Color,
    pub accent: Color,
    pub disabled: Color,
    pub warning: Color,
    pub focus: Color,
}

impl UiTokens {
    pub fn from_theme(theme: &Theme) -> Self {
        let dark = theme.kind == ThemeKind::Dark;
        let ink = theme.text;
        Self {
            font: "Segoe UI",
            icon_font: "Segoe Fluent Icons",
            text_size: Dip(13.0),
            row: Dip(28.0),
            menu_row: Dip(28.0),
            control_height: Dip(32.0),
            alert_button_width: Dip(92.0),
            check_size: Dip(16.0),
            radius: Dip(4.0),
            popup_radius: Dip(8.0),
            focus_width: Dip(2.0),
            inset: Dip(8.0),
            spacing: Dip(6.0),
            scrollbar_thin: Dip(3.0),
            scrollbar_wide: Dip(8.0),
            submenu_delay_ms: 250,
            text: ink,
            background: theme.text_background,
            surface: if dark {
                Color::rgb(43, 43, 43)
            } else {
                Color::rgb(250, 250, 250)
            },
            hover: Color { a: 15, ..ink },
            pressed: Color { a: 28, ..ink },
            selected: if dark {
                Color::rgb(51, 76, 102)
            } else {
                Color::rgb(221, 236, 250)
            },
            border: theme.border,
            accent: if dark {
                Color::rgb(130, 190, 255)
            } else {
                Color::rgb(0, 95, 184)
            },
            disabled: theme.dim_text,
            warning: theme.warning,
            focus: if dark {
                Color::rgb(153, 205, 255)
            } else {
                Color::rgb(0, 95, 184)
            },
        }
    }

    pub fn high_contrast(theme: &Theme) -> Self {
        let mut tokens = Self::from_theme(theme);
        tokens.hover = tokens.selected;
        tokens.pressed = tokens.accent;
        tokens.border = tokens.text;
        tokens.focus = tokens.text;
        tokens
    }
}
