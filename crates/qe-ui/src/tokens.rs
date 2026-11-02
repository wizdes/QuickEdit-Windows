use qe_api::{Color, Dip, Theme, ThemeKind};

/// The family name for `TextRun.font`. The renderer takes the size from `font_size`.
pub const UI_FONT: &str = "Segoe UI";
/// The same font and size as a measuring token. The number is in points: 9.75 pt is 13 dips.
pub const UI_MEASURE_TOKEN: &str = "Segoe UI 9.75";
pub const ICON_FONT: &str = "Segoe Fluent Icons";
/// The icon font as a measuring token. The number is in points: 9 pt is 12 dips.
pub const ICON_MEASURE_TOKEN: &str = "Segoe Fluent Icons 9";

/// The system colors of a Windows high contrast theme.
///
/// `qe_api::Theme` has no place for system colors yet. `theme` stores them in its
/// fields, with an opaque border and opaque dim text, which no normal theme has.
/// `UiTokens::from_theme` looks for that mark, so each control's `draw` reaches
/// these colors through the `Theme` it already gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemPalette {
    pub window: Color,
    pub window_text: Color,
    pub highlight: Color,
    pub highlight_text: Color,
    pub button_face: Color,
    pub gray_text: Color,
    pub hotlight: Color,
}
impl SystemPalette {
    /// The colors of the Windows "High Contrast Black" theme, for tests and pictures.
    pub const HIGH_CONTRAST_BLACK: Self = Self {
        window: Color::rgb(0, 0, 0),
        window_text: Color::rgb(255, 255, 255),
        highlight: Color::rgb(26, 235, 255),
        highlight_text: Color::rgb(0, 0, 0),
        button_face: Color::rgb(0, 0, 0),
        gray_text: Color::rgb(63, 242, 63),
        hotlight: Color::rgb(255, 255, 0),
    };
    /// The colors of the Windows "High Contrast White" theme, for tests and pictures.
    pub const HIGH_CONTRAST_WHITE: Self = Self {
        window: Color::rgb(255, 255, 255),
        window_text: Color::rgb(0, 0, 0),
        highlight: Color::rgb(55, 0, 110),
        highlight_text: Color::rgb(255, 255, 255),
        button_face: Color::rgb(255, 255, 255),
        gray_text: Color::rgb(96, 0, 0),
        hotlight: Color::rgb(0, 0, 159),
    };
    pub fn theme(self) -> Theme {
        let dark = luminance(self.window) < 0.5;
        let mut theme = Theme::new(if dark {
            ThemeKind::Dark
        } else {
            ThemeKind::Light
        });
        theme.text = self.window_text;
        theme.text_background = self.window;
        theme.under_page = self.button_face;
        theme.gutter_text = self.window_text;
        theme.gutter_separator = self.window_text;
        theme.map_text = self.window_text;
        theme.map_box_fill = self.highlight_text;
        theme.map_box_outline = self.hotlight;
        theme.warning = self.hotlight;
        theme.dim_text = self.gray_text;
        theme.group_header = self.window_text;
        theme.tab_background = self.button_face;
        theme.tab_selected = self.highlight;
        theme.border = self.window_text;
        theme
    }
    fn from_theme(theme: &Theme) -> Self {
        Self {
            window: theme.text_background,
            window_text: theme.text,
            highlight: theme.tab_selected,
            highlight_text: theme.map_box_fill,
            button_face: theme.tab_background,
            gray_text: theme.dim_text,
            hotlight: theme.map_box_outline,
        }
    }
    /// Every color that a control may use under this palette.
    pub fn colors(self) -> [Color; 7] {
        [
            self.window,
            self.window_text,
            self.highlight,
            self.highlight_text,
            self.button_face,
            self.gray_text,
            self.hotlight,
        ]
    }
}

/// True for a `Theme` made by `SystemPalette::theme`.
pub fn is_high_contrast(theme: &Theme) -> bool {
    theme.border.a == 255 && theme.dim_text.a == 255
}

fn channel(value: u8) -> f32 {
    let v = f32::from(value) / 255.0;
    if v <= 0.03928 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn luminance(c: Color) -> f32 {
    0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
}
/// A translucent color laid over an opaque one.
pub fn over(top: Color, under: Color) -> Color {
    let a = u32::from(top.a);
    let mix = |t: u8, u: u8| ((u32::from(t) * a + u32::from(u) * (255 - a) + 127) / 255) as u8;
    Color::rgb(
        mix(top.r, under.r),
        mix(top.g, under.g),
        mix(top.b, under.b),
    )
}
/// The WCAG contrast ratio of two colors. A translucent `a` is first laid over `b`.
pub fn contrast_ratio(a: Color, b: Color) -> f32 {
    let (x, y) = (luminance(over(a, b)), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// All measurements and colors used by painted controls.
///
/// Colors that `Theme` has no field for are set here. The report for root lists the
/// `Theme` fields that would let them move there, as design-spec section 3 asks.
#[derive(Clone, Copy, Debug)]
pub struct UiTokens {
    pub high_contrast: bool,
    pub font: &'static str,
    pub measure_font: &'static str,
    pub icon_font: &'static str,
    pub icon_measure_font: &'static str,
    pub text_size: Dip,
    pub icon_size: Dip,
    pub row: Dip,
    pub menu_row: Dip,
    pub separator_row: Dip,
    pub check_row: Dip,
    pub popup_rows: usize,
    pub control_height: Dip,
    pub alert_button_width: Dip,
    pub check_size: Dip,
    pub caret_box: Dip,
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
    pub hover_text: Color,
    pub pressed: Color,
    pub pressed_text: Color,
    pub selected: Color,
    pub selected_text: Color,
    pub border: Color,
    pub popup_border: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub accent_pressed: Color,
    pub on_accent: Color,
    pub disabled: Color,
    pub disabled_fill: Color,
    pub disabled_border: Color,
    pub warning: Color,
    pub focus: Color,
}

impl UiTokens {
    pub fn from_theme(theme: &Theme) -> Self {
        if is_high_contrast(theme) {
            return Self::high_contrast(theme);
        }
        let dark = theme.kind == ThemeKind::Dark;
        let ink = theme.text;
        Self {
            high_contrast: false,
            font: UI_FONT,
            measure_font: UI_MEASURE_TOKEN,
            icon_font: ICON_FONT,
            icon_measure_font: ICON_MEASURE_TOKEN,
            text_size: Dip(13.0),
            icon_size: Dip(12.0),
            row: Dip(28.0),
            menu_row: Dip(28.0),
            separator_row: Dip(9.0),
            check_row: Dip(24.0),
            popup_rows: 8,
            control_height: Dip(32.0),
            alert_button_width: Dip(92.0),
            check_size: Dip(16.0),
            caret_box: Dip(32.0),
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
            hover_text: ink,
            pressed: Color { a: 28, ..ink },
            pressed_text: ink,
            selected: if dark {
                Color::rgb(51, 76, 102)
            } else {
                Color::rgb(221, 236, 250)
            },
            selected_text: ink,
            // `theme.border` is about 1.7:1 light and 2.0:1 dark. A control's edge
            // needs 3:1, so it uses a stronger mix of the same ink.
            border: Color {
                a: if dark { 110 } else { 120 },
                ..ink
            },
            popup_border: theme.border,
            accent: if dark {
                Color::rgb(130, 190, 255)
            } else {
                Color::rgb(0, 95, 184)
            },
            accent_hover: if dark {
                Color::rgb(153, 205, 255)
            } else {
                Color::rgb(0, 84, 165)
            },
            accent_pressed: if dark {
                Color::rgb(107, 170, 240)
            } else {
                Color::rgb(0, 72, 143)
            },
            on_accent: theme.text_background,
            disabled: theme.dim_text,
            disabled_fill: theme.text_background,
            disabled_border: theme.border,
            warning: theme.warning,
            focus: if dark {
                Color::rgb(153, 205, 255)
            } else {
                Color::rgb(0, 95, 184)
            },
        }
    }

    /// The tokens for a system palette. Called with a normal theme, it gives the
    /// strongest colors that theme has.
    pub fn high_contrast(theme: &Theme) -> Self {
        let encoded = is_high_contrast(theme);
        let mut plain = *theme;
        plain.border.a = 56;
        let mut tokens = Self::from_theme(&plain);
        let p = if encoded {
            SystemPalette::from_theme(theme)
        } else {
            SystemPalette {
                window: theme.text_background,
                window_text: theme.text,
                highlight: tokens.accent,
                highlight_text: theme.text_background,
                button_face: theme.text_background,
                gray_text: Color {
                    a: 255,
                    ..theme.dim_text
                },
                hotlight: tokens.accent,
            }
        };
        tokens.high_contrast = true;
        tokens.text = p.window_text;
        tokens.background = p.window;
        tokens.surface = p.button_face;
        tokens.hover = p.highlight;
        tokens.hover_text = p.highlight_text;
        tokens.pressed = p.hotlight;
        tokens.pressed_text = p.window;
        tokens.selected = p.highlight;
        tokens.selected_text = p.highlight_text;
        tokens.border = p.window_text;
        tokens.popup_border = p.window_text;
        tokens.accent = p.highlight;
        tokens.accent_hover = p.highlight;
        tokens.accent_pressed = p.hotlight;
        tokens.on_accent = p.highlight_text;
        tokens.disabled = p.gray_text;
        tokens.disabled_fill = p.window;
        tokens.disabled_border = p.gray_text;
        tokens.warning = p.hotlight;
        tokens.focus = p.window_text;
        tokens
    }

    /// Sizes do not depend on the theme, so input handling can read them from here.
    pub fn sizes() -> Self {
        Self::from_theme(&Theme::new(ThemeKind::Light))
    }
}
