use qe_api::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub title: String,
    pub dirty: bool,
}
impl Tab {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            dirty: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ShellEvent {
    NewTab,
    CloseTab(usize),
    SelectTab(usize),
    ReorderTab { from: usize, to: usize },
    OpenCaretList(Rect),
    TearOffTab { index: usize, at: Point },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionHit {
    None,
    Drag,
    Tab(usize),
    NewTab,
    Caret,
    ScrollLeft,
    ScrollRight,
    Minimize,
    Maximize,
    Close,
    ResizeLeft,
    ResizeRight,
    ResizeTop,
    ResizeBottom,
    ResizeTopLeft,
    ResizeTopRight,
    ResizeBottomLeft,
    ResizeBottomRight,
}

#[derive(Clone, Copy)]
pub struct ShellTokens {
    pub row_height: f32,
    pub max_row_height: f32,
    pub tab_height: f32,
    pub radius: f32,
    pub max_width: f32,
    pub min_width: f32,
    pub split_height: f32,
    pub split_y: f32,
    pub split_part: f32,
    pub separator_width: f32,
    pub scroll_width: f32,
    pub caption_width: f32,
    pub edge: f32,
    pub icon: f32,
    pub caption_icon: f32,
    pub focus_width: f32,
    pub drag_threshold: f32,
    pub text_inset: f32,
    pub close_width: f32,
    pub text_y: f32,
    pub icon_y: f32,
    pub text_font_size: f32,
    pub icon_font_size: f32,
    pub min_window_width: f32,
    pub new_window_columns: f32,
    pub new_window_rows: f32,
    pub row: Color,
    pub active: Color,
    pub text: Color,
    pub dim: Color,
    pub hover: Color,
    pub pressed: Color,
    pub separator: Color,
    pub close_hover: Color,
    pub close_text: Color,
    pub focus: Color,
    pub icon_font: &'static str,
    pub caption_font: &'static str,
    pub text_font: &'static str,
}
impl ShellTokens {
    pub fn from_theme(theme: &Theme) -> Self {
        let dark = theme.kind == ThemeKind::Dark;
        Self {
            row_height: 40.0,
            max_row_height: 32.0,
            tab_height: 32.0,
            radius: 8.0,
            max_width: 240.0,
            min_width: 80.0,
            split_height: 24.0,
            split_y: 4.0,
            split_part: 31.0,
            separator_width: 1.0,
            scroll_width: 20.0,
            caption_width: 46.0,
            edge: 8.0,
            icon: 12.0,
            caption_icon: 10.0,
            focus_width: 2.0,
            drag_threshold: 12.0,
            text_inset: 16.0,
            close_width: 32.0,
            text_y: 10.0,
            icon_y: 10.0,
            text_font_size: 13.0,
            icon_font_size: 12.0,
            min_window_width: 500.0,
            new_window_columns: 90.0,
            new_window_rows: 30.0,
            row: theme.tab_background,
            active: theme.text_background,
            text: theme.text,
            dim: theme.dim_text,
            hover: if dark {
                Color::rgb(66, 66, 66)
            } else {
                Color::rgb(224, 224, 224)
            },
            pressed: if dark {
                Color::rgb(82, 82, 82)
            } else {
                Color::rgb(208, 208, 208)
            },
            separator: theme.border,
            close_hover: Color::rgb(196, 43, 28),
            close_text: Color::rgb(255, 255, 255),
            focus: if dark {
                Color::rgb(134, 187, 255)
            } else {
                Color::rgb(0, 95, 184)
            },
            icon_font: "Segoe Fluent Icons",
            caption_font: "Segoe Fluent Icons;10px",
            text_font: "Segoe UI",
        }
    }
    pub fn split_width(self) -> f32 {
        self.split_part * 2.0 + self.separator_width
    }
}
fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(w),
        height: Dip(h),
    }
}
fn p(x: f32, y: f32) -> Point {
    Point {
        x: Dip(x),
        y: Dip(y),
    }
}
fn inside(at: Point, b: Rect) -> bool {
    at.x.0 >= b.x.0 && at.y.0 >= b.y.0 && at.x.0 < b.x.0 + b.width.0 && at.y.0 < b.y.0 + b.height.0
}
fn glyph(list: &mut DisplayList, x: f32, y: f32, ch: char, color: Color, t: ShellTokens) {
    list.push(DrawCommand::TextRun {
        at: p(x, y),
        text: ch.to_string(),
        font: t.icon_font.into(),
        color,
    });
}
fn caption_glyph(list: &mut DisplayList, x: f32, y: f32, ch: char, color: Color, t: ShellTokens) {
    list.push(DrawCommand::TextRun {
        at: p(x, y),
        text: ch.to_string(),
        font: t.caption_font.into(),
        color,
    });
}
fn ellipsis(title: &str, max: f32, measure: &dyn TextMeasurer, font: &str) -> String {
    if measure.measure(title, font).width.0 <= max {
        return title.into();
    }
    let mut out = String::new();
    for ch in title.chars() {
        let candidate = format!("{out}{ch}…");
        if measure.measure(&candidate, font).width.0 > max {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

pub struct TabStrip {
    tabs: Vec<Tab>,
    active: usize,
    bounds: Rect,
    tab_width: f32,
    tab_rects: Vec<Rect>,
    split: Rect,
    overflow: bool,
    scroll: usize,
    hover: CaptionHit,
    pressed: CaptionHit,
    focused: Option<CaptionHit>,
    window_active: bool,
    maximized: bool,
    high_contrast: bool,
    drag: Option<(usize, Point)>,
    events: Vec<ShellEvent>,
}
impl TabStrip {
    pub fn new(mut tabs: Vec<Tab>) -> Self {
        if tabs.is_empty() {
            tabs.push(Tab::new("Untitled"));
        }
        Self {
            tabs,
            active: 0,
            bounds: Rect::default(),
            tab_width: ShellTokens::from_theme(&Theme::new(ThemeKind::Light)).max_width,
            tab_rects: Vec::new(),
            split: Rect::default(),
            overflow: false,
            scroll: 0,
            hover: CaptionHit::None,
            pressed: CaptionHit::None,
            focused: None,
            window_active: true,
            maximized: false,
            high_contrast: false,
            drag: None,
            events: Vec::new(),
        }
    }
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }
    pub fn tabs_mut(&mut self) -> &mut [Tab] {
        &mut self.tabs
    }
    pub fn add_tab(&mut self, tab: Tab) {
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }
    pub fn active(&self) -> usize {
        self.active
    }
    pub fn tab_rects(&self) -> &[Rect] {
        &self.tab_rects
    }
    pub fn tab_width(&self) -> f32 {
        self.tab_width
    }
    pub fn split_rect(&self) -> Rect {
        self.split
    }
    pub fn overflow(&self) -> bool {
        self.overflow
    }
    pub fn set_window_active(&mut self, value: bool) {
        self.window_active = value;
    }
    pub fn set_maximized(&mut self, value: bool) {
        self.maximized = value;
    }
    fn row_height(&self, t: ShellTokens) -> f32 {
        if self.maximized {
            t.max_row_height
        } else {
            t.row_height
        }
    }
    pub fn set_high_contrast(&mut self, value: bool) {
        self.high_contrast = value;
    }
    pub fn set_focused(&mut self, value: Option<CaptionHit>) {
        self.focused = value;
    }
    pub fn take_events(&mut self) -> Vec<ShellEvent> {
        std::mem::take(&mut self.events)
    }
    pub fn select(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
            self.events.push(ShellEvent::SelectTab(index));
        }
    }
    pub fn reorder(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = if self.active == from {
            to
        } else if from < self.active && to >= self.active {
            self.active - 1
        } else if from > self.active && to <= self.active {
            self.active + 1
        } else {
            self.active
        };
        self.events.push(ShellEvent::ReorderTab { from, to });
    }
    pub fn close(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.events.push(ShellEvent::CloseTab(index));
        if self.tabs.len() > 1 {
            self.tabs.remove(index);
            self.active = self.active.min(self.tabs.len() - 1);
        } else {
            self.tabs[0] = Tab::new("Untitled");
            self.active = 0;
        }
    }
    pub fn hit_test(&self, at: Point) -> CaptionHit {
        let t = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
        if !inside(at, self.bounds) || at.y.0 >= self.bounds.y.0 + self.row_height(t) {
            return CaptionHit::None;
        }
        let x = at.x.0;
        let end = self.bounds.x.0 + self.bounds.width.0;
        if x >= end - t.caption_width {
            return CaptionHit::Close;
        }
        if x >= end - 2.0 * t.caption_width {
            return CaptionHit::Maximize;
        }
        if x >= end - 3.0 * t.caption_width {
            return CaptionHit::Minimize;
        }
        if self.overflow {
            if x < self.bounds.x.0 + t.scroll_width {
                return CaptionHit::ScrollLeft;
            }
            if x >= self.split.x.0 - t.scroll_width && x < self.split.x.0 {
                return CaptionHit::ScrollRight;
            }
        }
        if inside(at, self.split) {
            return if x < self.split.x.0 + t.split_part {
                CaptionHit::NewTab
            } else {
                CaptionHit::Caret
            };
        }
        let clip_end = self.split.x.0 - if self.overflow { t.scroll_width } else { 0.0 };
        for (i, b) in self.tab_rects.iter().enumerate() {
            if x < clip_end && inside(at, *b) {
                return CaptionHit::Tab(i);
            }
        }
        CaptionHit::Drag
    }
    fn close_at(&self, index: usize, at: Point) -> bool {
        let t = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
        self.tab_rects
            .get(index)
            .is_some_and(|b| at.x.0 >= b.x.0 + b.width.0 - t.close_width)
    }
    fn activate(&mut self, hit: CaptionHit) {
        match hit {
            CaptionHit::NewTab => self.events.push(ShellEvent::NewTab),
            CaptionHit::Caret => self.events.push(ShellEvent::OpenCaretList(self.split)),
            CaptionHit::Tab(i) => self.select(i),
            CaptionHit::ScrollLeft => self.scroll = self.scroll.saturating_sub(1),
            CaptionHit::ScrollRight => self.scroll = (self.scroll + 1).min(self.tabs.len() - 1),
            _ => {}
        }
    }
}
impl View for TabStrip {
    fn layout(&mut self, bounds: Rect, _measure: &dyn TextMeasurer) {
        self.bounds = bounds;
        let t = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
        let available = (bounds.width.0 - 3.0 * t.caption_width - t.split_width()).max(0.0);
        self.tab_width = (available / self.tabs.len() as f32).clamp(t.min_width, t.max_width);
        self.overflow = self.tabs.len() as f32 * t.min_width > available;
        let left = bounds.x.0 + if self.overflow { t.scroll_width } else { 0.0 };
        let visible = if self.overflow {
            (available - 2.0 * t.scroll_width).max(t.min_width)
        } else {
            available
        };
        self.scroll = self.scroll.min(self.tabs.len() - 1);
        self.tab_rects = (0..self.tabs.len())
            .map(|i| {
                r(
                    left + (i as isize - self.scroll as isize) as f32 * self.tab_width,
                    bounds.y.0 + self.row_height(t) - t.tab_height,
                    self.tab_width,
                    t.tab_height,
                )
            })
            .collect();
        self.split = r(
            if self.overflow {
                left + visible + t.scroll_width
            } else {
                left + self.tabs.len() as f32 * self.tab_width
            },
            bounds.y.0 + t.split_y,
            t.split_width(),
            t.split_height,
        );
    }
    fn handle(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        let t = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
        match event.kind {
            InputKind::KeyDown {
                virtual_key: 0x54,
                modifiers,
                ..
            } if modifiers.ctrl => {
                self.events.push(ShellEvent::NewTab);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: 0x12, ..
            } => {
                self.events.push(ShellEvent::OpenCaretList(self.split));
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: 0x09 | 0x25 | 0x27,
                modifiers,
                ..
            } => {
                let count = self.tabs.len() + 2;
                let current = match self.focused {
                    Some(CaptionHit::Tab(i)) => Some(i),
                    Some(CaptionHit::NewTab) => Some(self.tabs.len()),
                    Some(CaptionHit::Caret) => Some(self.tabs.len() + 1),
                    _ => None,
                };
                let backwards = modifiers.shift
                    || matches!(
                        event.kind,
                        InputKind::KeyDown {
                            virtual_key: 0x25,
                            ..
                        }
                    );
                let next = match current {
                    None => {
                        if backwards {
                            count - 1
                        } else {
                            0
                        }
                    }
                    Some(i) if backwards => (i + count - 1) % count,
                    Some(i) => (i + 1) % count,
                };
                let hit = if next < self.tabs.len() {
                    CaptionHit::Tab(next)
                } else if next == self.tabs.len() {
                    CaptionHit::NewTab
                } else {
                    CaptionHit::Caret
                };
                self.focused = Some(hit);
                ctx.set_focus(WidgetId(if next < self.tabs.len() {
                    100 + next as u64
                } else if next == self.tabs.len() {
                    1
                } else {
                    2
                }));
                ctx.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::KeyDown {
                virtual_key: 0x0d | 0x20,
                ..
            } => {
                if let Some(hit) = self.focused {
                    self.activate(hit);
                    ctx.invalidate(self.bounds);
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            InputKind::MouseMove { at } => {
                let hit = self.hit_test(at);
                if hit != self.hover {
                    self.hover = hit;
                    ctx.invalidate(self.bounds);
                }
                if let Some((index, start)) = self.drag {
                    if (at.y.0 - start.y.0).abs() > t.row_height {
                        self.events.push(ShellEvent::TearOffTab { index, at });
                        self.drag = None;
                        ctx.capture_mouse(false);
                    } else if (at.x.0 - start.x.0).abs() > t.drag_threshold
                        && let CaptionHit::Tab(to) = hit
                        && to != index
                    {
                        self.reorder(index, to);
                        self.drag = Some((to, at));
                        ctx.invalidate(self.bounds);
                    }
                }
                Handled::Yes
            }
            InputKind::MouseDown {
                at,
                button: MouseButton::Left,
                ..
            } => {
                self.pressed = self.hit_test(at);
                if let CaptionHit::Tab(i) = self.pressed
                    && !self.close_at(i, at)
                {
                    self.drag = Some((i, at));
                    ctx.capture_mouse(true);
                }
                ctx.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                ..
            } => {
                let hit = self.hit_test(at);
                if self.pressed == hit {
                    if let CaptionHit::Tab(i) = hit {
                        if self.close_at(i, at) {
                            self.close(i);
                        } else {
                            self.activate(hit);
                        }
                    } else {
                        self.activate(hit);
                    }
                }
                self.drag = None;
                self.pressed = CaptionHit::None;
                ctx.capture_mouse(false);
                ctx.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::Wheel { delta_y, .. } if self.overflow => {
                self.activate(if delta_y < 0.0 {
                    CaptionHit::ScrollRight
                } else {
                    CaptionHit::ScrollLeft
                });
                ctx.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::Blur => {
                self.window_active = false;
                ctx.invalidate(self.bounds);
                Handled::Yes
            }
            InputKind::Focus => {
                self.window_active = true;
                ctx.invalidate(self.bounds);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        let mut t = ShellTokens::from_theme(theme);
        if self.high_contrast {
            t.row = theme.text_background;
            t.hover = theme.tab_background;
            t.pressed = theme.tab_background;
            t.focus = theme.text;
        }
        list.push(DrawCommand::FillRect {
            rect: self.bounds,
            color: t.row,
        });
        let clip_end = self.split.x.0 - if self.overflow { t.scroll_width } else { 0.0 };
        list.push(DrawCommand::PushClip(r(
            self.bounds.x.0,
            self.bounds.y.0,
            (clip_end - self.bounds.x.0).max(0.0),
            self.row_height(t),
        )));
        for (i, tab) in self.tabs.iter().enumerate() {
            let b = self.tab_rects[i];
            if b.x.0 + b.width.0 < self.bounds.x.0 || b.x.0 > clip_end {
                continue;
            }
            let color = if i == self.active {
                t.active
            } else if self.pressed == CaptionHit::Tab(i) {
                t.pressed
            } else if self.hover == CaptionHit::Tab(i) {
                t.hover
            } else {
                t.row
            };
            list.push(DrawCommand::RoundedRect {
                rect: b,
                radius: Dip(t.radius),
                color,
            });
            if i == self.active {
                list.push(DrawCommand::FillRect {
                    rect: r(b.x.0, b.y.0 + b.height.0 - t.radius, b.width.0, t.radius),
                    color: t.active,
                });
            }
            let fg = if self.window_active { t.text } else { t.dim };
            list.push(DrawCommand::TextRun {
                at: p(b.x.0 + t.text_inset, b.y.0 + t.text_y),
                text: ellipsis(
                    &tab.title,
                    (b.width.0 - t.text_inset - t.close_width).max(0.0),
                    measure,
                    t.text_font,
                ),
                font: t.text_font.into(),
                color: fg,
            });
            let close_x = b.x.0 + b.width.0 - (t.close_width + t.icon) / 2.0;
            if tab.dirty && self.hover != CaptionHit::Tab(i) {
                list.push(DrawCommand::TextRun {
                    at: p(close_x, b.y.0 + t.icon_y),
                    text: "•".into(),
                    font: t.text_font.into(),
                    color: fg,
                });
            } else {
                glyph(list, close_x, b.y.0 + t.icon_y, '\u{e8bb}', fg, t);
            }
            if self.focused == Some(CaptionHit::Tab(i)) || self.high_contrast {
                list.push(DrawCommand::StrokeRect {
                    rect: b,
                    radius: Dip(t.radius),
                    width: Dip(t.focus_width),
                    color: t.focus,
                });
            }
        }
        list.push(DrawCommand::PopClip);
        if self.overflow {
            for (hit, x, ch) in [
                (CaptionHit::ScrollLeft, self.bounds.x.0, '\u{e76b}'),
                (
                    CaptionHit::ScrollRight,
                    self.split.x.0 - t.scroll_width,
                    '\u{e76c}',
                ),
            ] {
                let b = r(x, self.bounds.y.0, t.scroll_width, t.tab_height);
                if self.hover == hit || self.pressed == hit {
                    list.push(DrawCommand::FillRect {
                        rect: b,
                        color: if self.pressed == hit {
                            t.pressed
                        } else {
                            t.hover
                        },
                    });
                }
                glyph(
                    list,
                    x + (t.scroll_width - t.icon) / 2.0,
                    self.bounds.y.0 + t.icon_y,
                    ch,
                    t.text,
                    t,
                );
            }
        }
        for (hit, x, ch) in [
            (CaptionHit::NewTab, self.split.x.0, '\u{e710}'),
            (
                CaptionHit::Caret,
                self.split.x.0 + t.split_part + t.separator_width,
                '\u{e96e}',
            ),
        ] {
            let b = r(x, self.split.y.0, t.split_part, t.split_height);
            if self.hover == hit || self.pressed == hit {
                list.push(DrawCommand::FillRect {
                    rect: b,
                    color: if self.pressed == hit {
                        t.pressed
                    } else {
                        t.hover
                    },
                });
            }
            glyph(
                list,
                x + (t.split_part - t.icon) / 2.0,
                self.split.y.0 + (t.split_height - t.icon) / 2.0,
                ch,
                if self.window_active { t.text } else { t.dim },
                t,
            );
            if self.focused == Some(hit) || self.high_contrast {
                list.push(DrawCommand::StrokeRect {
                    rect: b,
                    radius: Dip(0.0),
                    width: Dip(t.focus_width),
                    color: t.focus,
                });
            }
        }
        list.push(DrawCommand::Line {
            from: p(self.split.x.0 + t.split_part, self.split.y.0),
            to: p(
                self.split.x.0 + t.split_part,
                self.split.y.0 + t.split_height,
            ),
            width: Dip(t.separator_width),
            color: t.separator,
        });
        for (i, (hit, ch)) in [
            (CaptionHit::Minimize, '\u{e921}'),
            (
                CaptionHit::Maximize,
                if self.maximized {
                    '\u{e923}'
                } else {
                    '\u{e922}'
                },
            ),
            (CaptionHit::Close, '\u{e8bb}'),
        ]
        .into_iter()
        .enumerate()
        {
            let x = self.bounds.x.0 + self.bounds.width.0 - (3 - i) as f32 * t.caption_width;
            let b = r(x, self.bounds.y.0, t.caption_width, self.row_height(t));
            if self.hover == hit || self.pressed == hit {
                list.push(DrawCommand::FillRect {
                    rect: b,
                    color: if hit == CaptionHit::Close {
                        t.close_hover
                    } else if self.pressed == hit {
                        t.pressed
                    } else {
                        t.hover
                    },
                });
            }
            caption_glyph(
                list,
                x + (t.caption_width - t.caption_icon) / 2.0,
                self.bounds.y.0 + (self.row_height(t) - t.caption_icon) / 2.0,
                ch,
                if hit == CaptionHit::Close && self.hover == hit {
                    t.close_text
                } else if self.window_active {
                    t.text
                } else {
                    t.dim
                },
                t,
            );
        }
    }
    fn widgets(&self) -> Option<Widget> {
        let mut children = Vec::new();
        let t = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
        for (i, tab) in self.tabs.iter().enumerate() {
            let tab_rect = self.tab_rects[i];
            children.push(Widget {
                id: WidgetId(100 + i as u64),
                bounds: tab_rect,
                role: WidgetRole::Tab,
                name: tab.title.clone(),
                value: None,
                states: WidgetStates {
                    enabled: true,
                    selected: i == self.active,
                    focused: self.focused == Some(CaptionHit::Tab(i)),
                    ..Default::default()
                },
                children: vec![Widget {
                    id: WidgetId(1000 + i as u64),
                    bounds: r(
                        tab_rect.x.0 + tab_rect.width.0 - t.close_width,
                        tab_rect.y.0,
                        t.close_width,
                        tab_rect.height.0,
                    ),
                    role: WidgetRole::Button,
                    name: "Close tab".into(),
                    value: None,
                    states: WidgetStates {
                        enabled: true,
                        ..Default::default()
                    },
                    children: Vec::new(),
                }],
            });
        }
        for (id, name, b) in [
            (
                1,
                "New tab",
                r(
                    self.split.x.0,
                    self.split.y.0,
                    t.split_part,
                    self.split.height.0,
                ),
            ),
            (
                2,
                "Tab list",
                r(
                    self.split.x.0 + t.split_part + t.separator_width,
                    self.split.y.0,
                    t.split_part,
                    self.split.height.0,
                ),
            ),
        ] {
            children.push(Widget {
                id: WidgetId(id),
                bounds: b,
                role: WidgetRole::Button,
                name: name.into(),
                value: None,
                states: WidgetStates {
                    enabled: true,
                    ..Default::default()
                },
                children: Vec::new(),
            });
        }
        let caption_start = self.bounds.x.0 + self.bounds.width.0 - 3.0 * t.caption_width;
        for (id, name, offset) in [
            (3, "Minimize", 0.0),
            (4, "Maximize or restore", 1.0),
            (5, "Close window", 2.0),
        ] {
            children.push(Widget {
                id: WidgetId(id),
                bounds: r(
                    caption_start + offset * t.caption_width,
                    self.bounds.y.0,
                    t.caption_width,
                    self.row_height(t),
                ),
                role: WidgetRole::Button,
                name: name.into(),
                value: None,
                states: WidgetStates {
                    enabled: true,
                    ..Default::default()
                },
                children: Vec::new(),
            });
        }
        if self.overflow {
            for (id, name, x) in [
                (6, "Scroll tabs left", self.bounds.x.0),
                (7, "Scroll tabs right", self.split.x.0 - t.scroll_width),
            ] {
                children.push(Widget {
                    id: WidgetId(id),
                    bounds: r(x, self.bounds.y.0, t.scroll_width, self.row_height(t)),
                    role: WidgetRole::Button,
                    name: name.into(),
                    value: None,
                    states: WidgetStates {
                        enabled: true,
                        ..Default::default()
                    },
                    children: Vec::new(),
                });
            }
        }
        Some(Widget {
            id: WidgetId(0),
            bounds: self.bounds,
            role: WidgetRole::TabList,
            name: "Tabs".into(),
            value: None,
            states: WidgetStates {
                enabled: true,
                ..Default::default()
            },
            children,
        })
    }
}
pub fn maximized_insets(frame_px: i32, padded_border_px: i32) -> i32 {
    frame_px + padded_border_px
}
pub fn caption_hit(
    at: Point,
    width: f32,
    height: f32,
    scale: f32,
    maximized: bool,
    strip: &TabStrip,
) -> CaptionHit {
    let t = ShellTokens::from_theme(&Theme::new(ThemeKind::Light));
    if !maximized {
        let e = t.edge * scale;
        let l = at.x.0 < e;
        let rr = at.x.0 >= width - e;
        let top = at.y.0 < e;
        let bottom = at.y.0 >= height - e;
        match (l, rr, top, bottom) {
            (true, _, true, _) => return CaptionHit::ResizeTopLeft,
            (_, true, true, _) => return CaptionHit::ResizeTopRight,
            (true, _, _, true) => return CaptionHit::ResizeBottomLeft,
            (_, true, _, true) => return CaptionHit::ResizeBottomRight,
            (true, _, _, _) => return CaptionHit::ResizeLeft,
            (_, true, _, _) => return CaptionHit::ResizeRight,
            (_, _, true, _) => return CaptionHit::ResizeTop,
            (_, _, _, true) => return CaptionHit::ResizeBottom,
            _ => {}
        }
    }
    if at.y.0
        >= (if maximized {
            t.max_row_height
        } else {
            t.row_height
        }) * scale
    {
        return CaptionHit::None;
    }
    let x = at.x.0 / scale;
    let w = width / scale;
    if x >= w - t.caption_width {
        CaptionHit::Close
    } else if x >= w - 2.0 * t.caption_width {
        CaptionHit::Maximize
    } else if x >= w - 3.0 * t.caption_width {
        CaptionHit::Minimize
    } else {
        strip.hit_test(p(x, at.y.0 / scale))
    }
}
