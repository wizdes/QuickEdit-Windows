//! The top row as plain data: tabs, the split button and the window buttons. It holds
//! no window handle, so that every rule here can run in a test with no desktop.
use crate::ShellError;
use qe_api::*;
use std::cell::Cell;

/// Names one tab for as long as it lives. A position can change while an owner shows a
/// "save changes?" question, and an id cannot, so a late answer cannot close another tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub title: String,
    pub dirty: bool,
    id: TabId,
}
impl Tab {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            dirty: false,
            id: TabId(0),
        }
    }
    /// Set when the tab joins a strip.
    pub fn id(&self) -> TabId {
        self.id
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ShellEvent {
    NewTab,
    /// The user asked to close this tab. The tab stays until the owner calls `close`.
    CloseTabRequested(TabId),
    /// The user asked to close the window. It stays until the owner destroys it.
    CloseWindowRequested,
    SelectTab(TabId),
    ReorderTab {
        tab: TabId,
        to: usize,
    },
    OpenCaretList(Rect),
    TearOffTab {
        tab: TabId,
        at: Point,
    },
    /// A window button was clicked. `ShellWindow` acts on this itself, so that an owner
    /// of the window never sees it.
    CaptionCommand(CaptionHit),
    /// Keyboard focus left the strip for the text area.
    FocusContent,
    /// Input that the top row did not take, for the text area.
    ContentInput(InputEvent),
    /// The text area held the mouse, and another window took it. A selection drag ends.
    ContentCaptureLost,
    /// A timer from `ShellHandle::set_timer` or `ViewRequest::RequestTimer` fired.
    Timer(u32),
    /// Another thread called `ShellWaker::wake`. Several calls can arrive as one event.
    Wake,
    /// A view asked to run a command, with `ViewRequest::Run`.
    RunCommand(CommandId),
    /// The window was destroyed. The owner quits the message loop after the last one.
    WindowDestroyed,
    Problem(ShellError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionHit {
    None,
    Drag,
    Tab(usize),
    TabClose(usize),
    NewTab,
    Caret,
    ScrollLeft,
    ScrollRight,
    Minimize,
    Maximize,
    Close,
    /// The thin band over the row that resizes the window from its top edge. The left,
    /// right and bottom edges stay in the invisible frame outside the client area.
    ResizeTop,
}

/// One value says how the window is framed. Drawing, hit testing and the window's
/// messages all read this value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameMode {
    #[default]
    Restored,
    Maximized,
    FullScreen,
}

/// The system colors that high contrast uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContrastColors {
    pub window: Color,
    pub window_text: Color,
    pub highlight: Color,
    pub highlight_text: Color,
    pub gray_text: Color,
}

/// Values that `Theme` does not have yet are here, and the milestone report lists them.
/// Colors and sizes come from Terminal's `MinMaxCloseControl.xaml` and WinUI's
/// `TabView_themeresources.xaml`.
#[derive(Clone, Copy)]
pub struct ShellTokens {
    pub row_height: f32,
    pub max_row_height: f32,
    pub tab_height: f32,
    pub radius: f32,
    pub button_radius: f32,
    pub max_width: f32,
    pub min_width: f32,
    pub split_height: f32,
    pub split_part: f32,
    pub separator_width: f32,
    pub scroll_width: f32,
    pub caption_width: f32,
    pub top_band: f32,
    pub icon: f32,
    pub caption_icon: f32,
    /// The font size that gives the caret WinUI's ink of about 7.7 x 4.2.
    pub caret_icon: f32,
    pub caret_box: f32,
    pub caret_right_padding: f32,
    /// E96E's ink sits above the middle of its em box. This moves it down to the center
    /// at 200%, and is too small to move the baseline's whole pixel at 100%.
    pub caret_nudge: f32,
    /// Its ink also sits right of the middle. Terminal's center is at x = 13 of the part.
    pub caret_nudge_x: f32,
    pub focus_width: f32,
    pub drag_threshold: f32,
    pub text_inset: f32,
    pub close_width: f32,
    pub close_box: f32,
    pub text_font_size: f32,
    pub min_window_width: f32,
    pub min_window_height: f32,
    pub new_window_columns: f32,
    pub new_window_rows: f32,
    pub row: Color,
    pub active: Color,
    pub text: Color,
    pub dim: Color,
    pub button_text: Color,
    pub button_text_pressed: Color,
    pub caret_text: Color,
    pub caret_text_pressed: Color,
    pub caption_dim: Color,
    pub hover: Color,
    pub pressed: Color,
    pub separator: Color,
    pub close_hover: Color,
    pub close_pressed: Color,
    pub close_text: Color,
    pub close_text_pressed: Color,
    pub focus: Color,
    pub text_font: &'static str,
    /// The font token for measuring text: a family and a size in points.
    pub text_token: &'static str,
    /// Measures a 12-dip icon. 12 dips is 9 points.
    pub icon_token: &'static str,
    /// Measures a 10-dip icon. 10 dips is 7.5 points.
    pub caption_icon_token: &'static str,
    /// Measures the caret. 9 dips is 6.75 points.
    pub caret_token: &'static str,
}
const fn alpha(c: Color, a: u8) -> Color {
    Color { a, ..c }
}
impl ShellTokens {
    pub fn from_theme(theme: &Theme) -> Self {
        let dark = theme.kind == ThemeKind::Dark;
        Self {
            // The spec says 40 and 32. `Theme::top_row_height` holds 32.
            row_height: 40.0,
            max_row_height: 32.0,
            tab_height: theme.tab_height.0,
            radius: 8.0,
            button_radius: 4.0,
            max_width: 240.0,
            min_width: 80.0,
            split_height: 24.0,
            split_part: 31.0,
            separator_width: 1.0,
            scroll_width: 20.0,
            caption_width: theme.caption_button_width.0,
            top_band: 8.0,
            icon: 12.0,
            caption_icon: 10.0,
            caret_icon: 10.0,
            caret_box: 12.0,
            caret_right_padding: 12.0,
            caret_nudge: 0.3,
            caret_nudge_x: -0.55,
            focus_width: 2.0,
            drag_threshold: 12.0,
            text_inset: 16.0,
            close_width: 32.0,
            close_box: 24.0,
            text_font_size: 13.0,
            min_window_width: 500.0,
            min_window_height: 160.0,
            new_window_columns: 90.0,
            new_window_rows: 30.0,
            row: theme.tab_background,
            active: theme.text_background,
            text: theme.text,
            dim: theme.dim_text,
            button_text: if dark {
                theme.text
            } else {
                alpha(theme.text, 0xe4)
            },
            button_text_pressed: if dark {
                alpha(theme.text, 0xc5)
            } else {
                alpha(theme.text, 0x9e)
            },
            // `SplitButtonForegroundSecondary`: Terminal does not override it.
            caret_text: if dark {
                alpha(theme.text, 0xc5)
            } else {
                alpha(theme.text, 0x9e)
            },
            caret_text_pressed: if dark {
                alpha(theme.text, 0x87)
            } else {
                alpha(theme.text, 0x72)
            },
            caption_dim: if dark {
                alpha(theme.text, 0x5d)
            } else {
                alpha(theme.text, 0x5c)
            },
            hover: if dark {
                alpha(theme.text, 0x0f)
            } else {
                alpha(theme.text, 0x09)
            },
            pressed: if dark {
                alpha(theme.text, 0x0a)
            } else {
                alpha(theme.text, 0x06)
            },
            // `ControlStrokeColorDefault`, through `SplitButtonBorderBrushDivider`.
            separator: if dark {
                alpha(theme.text, 0x12)
            } else {
                alpha(theme.text, 0x0f)
            },
            close_hover: Color::rgb(0xc4, 0x2b, 0x1c),
            close_pressed: alpha(Color::rgb(0xc4, 0x2b, 0x1c), 230),
            close_text: Color::rgb(255, 255, 255),
            close_text_pressed: alpha(Color::rgb(255, 255, 255), 178),
            focus: if dark {
                Color::rgb(134, 187, 255)
            } else {
                Color::rgb(0, 95, 184)
            },
            text_font: "Segoe UI",
            text_token: "Segoe UI 9.75",
            icon_token: "Segoe Fluent Icons 9",
            caption_icon_token: "Segoe Fluent Icons 7.5",
            caret_token: "Segoe Fluent Icons 6.75",
        }
    }
    /// Replaces each color with a system color. There is no hardcoded red or white.
    fn with_contrast(mut self, c: ContrastColors) -> Self {
        self.row = c.window;
        self.active = c.highlight;
        self.text = c.window_text;
        self.dim = c.window_text;
        self.button_text = c.window_text;
        self.button_text_pressed = c.highlight_text;
        self.caret_text = c.window_text;
        self.caret_text_pressed = c.highlight_text;
        self.caption_dim = c.gray_text;
        self.hover = c.highlight;
        self.pressed = c.highlight;
        self.separator = c.window_text;
        self.close_hover = c.highlight;
        self.close_pressed = c.highlight;
        self.close_text = c.highlight_text;
        self.close_text_pressed = c.highlight_text;
        self.focus = c.window_text;
        self
    }
    pub fn split_width(self) -> f32 {
        self.split_part * 2.0 + self.separator_width
    }
}

/// The widget id that means "the text area", in a `ViewRequest::SetFocus`.
pub const CONTENT_WIDGET: WidgetId = WidgetId(u64::MAX);
const TAB_IDS: u64 = 1 << 32;
const TAB_CLOSE_IDS: u64 = 2 << 32;

const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0d;
const VK_MENU: u16 = 0x12;
const VK_ESCAPE: u16 = 0x1b;
const VK_SPACE: u16 = 0x20;
const VK_LEFT: u16 = 0x25;
const VK_RIGHT: u16 = 0x27;
const VK_LMENU: u16 = 0xa4;
const VK_RMENU: u16 = 0xa5;

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
fn light_tokens() -> ShellTokens {
    ShellTokens::from_theme(&Theme::new(ThemeKind::Light))
}
/// Where a run starts so that its box of `metrics` sits at the center of `b`.
fn centered(b: Rect, metrics: TextMetrics) -> Point {
    p(
        b.x.0 + (b.width.0 - metrics.width.0) / 2.0,
        b.y.0 + (b.height.0 - metrics.height.0) / 2.0 + metrics.baseline.0,
    )
}
struct IconStyle {
    size: f32,
    token: &'static str,
    color: Color,
    scale: f32,
}
fn icon(list: &mut DisplayList, b: Rect, code: u32, style: IconStyle, m: &dyn TextMeasurer) {
    let Some(ch) = char::from_u32(code) else {
        return;
    };
    let metrics = m.measure(ch.encode_utf8(&mut [0; 4]), style.token);
    // WinUI's layout rounding starts a glyph on a whole pixel. A half pixel here widens
    // the ClearType ink by one more column.
    let at = centered(b, metrics);
    let at = p((at.x.0 * style.scale).round() / style.scale, at.y.0);
    list.push(DrawCommand::Icon {
        at,
        icon_id: format!("{code:04X}"),
        font_size: Dip(style.size),
        color: style.color,
    });
}
/// True when a cut before `c` would split what the eye sees as one character. This is a
/// short form of Unicode's grapheme rules, because this crate's lock entry has no
/// `unicode-segmentation`. It covers combining marks, variation selectors, skin tones,
/// joined emoji, flags, keycaps and tag sequences. It does not cover Indic conjuncts or
/// Hangul jamo, where a cut still gives valid text.
fn joins_previous(previous: char, c: char, regional_run: usize) -> bool {
    let code = c as u32;
    let extends = matches!(code,
        0x0300..=0x036f
        | 0x0483..=0x0489
        | 0x0591..=0x05bd
        | 0x0610..=0x061a
        | 0x064b..=0x065f
        | 0x1ab0..=0x1aff
        | 0x1dc0..=0x1dff
        | 0x200c..=0x200d
        | 0x20d0..=0x20ff
        | 0xfe00..=0xfe0f
        | 0xfe20..=0xfe2f
        | 0x1f3fb..=0x1f3ff
        | 0xe0020..=0xe007f
        | 0xe0100..=0xe01ef);
    let flag = matches!(code, 0x1f1e6..=0x1f1ff) && regional_run % 2 == 1;
    extends || flag || previous == '\u{200d}'
}
/// The longest start of `title` that fits with an ellipsis, cut between whole characters.
fn ellipsis(title: &str, max: f32, measure: &dyn TextMeasurer, font: &str) -> String {
    if measure.measure(title, font).width.0 <= max {
        return title.into();
    }
    let mut best = 0;
    let mut previous = '\0';
    let mut regional_run = 0;
    for (at, c) in title.char_indices() {
        if at > 0 && !joins_previous(previous, c, regional_run) {
            if measure.measure(&format!("{}…", &title[..at]), font).width.0 > max {
                break;
            }
            best = at;
        }
        regional_run = if matches!(c as u32, 0x1f1e6..=0x1f1ff) {
            regional_run + 1
        } else {
            0
        };
        previous = c;
    }
    format!("{}…", &title[..best])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Look {
    Rest,
    Hover,
    Pressed,
}
#[derive(Clone, Copy)]
struct TabDrag {
    index: usize,
    start: Point,
    moved: bool,
}

pub struct TabStrip {
    tabs: Vec<Tab>,
    active: usize,
    bounds: Rect,
    tab_width: f32,
    /// Always one rectangle per tab, whether or not `layout` has run.
    tab_rects: Vec<Rect>,
    split: Rect,
    overflow: bool,
    visible_width: f32,
    scroll: usize,
    wheel: f32,
    hover: CaptionHit,
    pressed: CaptionHit,
    focused: Option<CaptionHit>,
    window_active: bool,
    mode: FrameMode,
    contrast: Option<ContrastColors>,
    drag: Option<TabDrag>,
    capturing: bool,
    alt_armed: bool,
    reveal_on_layout: bool,
    release_capture: bool,
    next_id: u64,
    scale: Cell<f32>,
    events: Vec<ShellEvent>,
}
impl TabStrip {
    pub fn new(mut tabs: Vec<Tab>) -> Self {
        if tabs.is_empty() {
            tabs.push(Tab::new("Untitled"));
        }
        let mut out = Self {
            tabs,
            active: 0,
            bounds: Rect::default(),
            tab_width: light_tokens().max_width,
            tab_rects: Vec::new(),
            split: Rect::default(),
            overflow: false,
            visible_width: 0.0,
            scroll: 0,
            wheel: 0.0,
            hover: CaptionHit::None,
            pressed: CaptionHit::None,
            focused: None,
            window_active: true,
            mode: FrameMode::Restored,
            contrast: None,
            drag: None,
            capturing: false,
            alt_armed: false,
            reveal_on_layout: true,
            release_capture: false,
            next_id: 0,
            scale: Cell::new(1.0),
            events: Vec::new(),
        };
        for i in 0..out.tabs.len() {
            out.tabs[i].id = out.new_id();
        }
        out.arrange();
        out
    }
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }
    pub fn tabs_mut(&mut self) -> &mut [Tab] {
        &mut self.tabs
    }
    fn new_id(&mut self) -> TabId {
        self.next_id += 1;
        TabId(self.next_id)
    }
    pub fn add_tab(&mut self, mut tab: Tab) -> TabId {
        tab.id = self.new_id();
        let id = tab.id;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.clear_pointer();
        self.reveal(self.active);
        id
    }
    /// The position of a tab now, or nothing if it has closed.
    pub fn index_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }
    pub fn tab_id(&self, index: usize) -> Option<TabId> {
        self.tabs.get(index).map(Tab::id)
    }
    /// Device pixels per dip, so that icons can start on a whole pixel.
    pub fn set_scale(&self, scale: f32) {
        self.scale.set(scale);
    }
    /// True once if a change to the tabs ended a press that held the mouse. The window
    /// then releases capture, because no button-up will reach the press.
    pub fn take_capture_release(&mut self) -> bool {
        std::mem::take(&mut self.release_capture)
    }
    /// A key table used this key, so that Alt was not alone.
    pub fn disarm_alt(&mut self) {
        self.alt_armed = false;
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
    pub fn focused(&self) -> Option<CaptionHit> {
        self.focused
    }
    pub fn set_window_active(&mut self, value: bool) {
        self.window_active = value;
    }
    pub fn frame_mode(&self) -> FrameMode {
        self.mode
    }
    pub fn set_frame_mode(&mut self, mode: FrameMode) {
        if self.mode != mode {
            self.mode = mode;
            self.arrange();
        }
    }
    pub fn set_maximized(&mut self, value: bool) {
        self.set_frame_mode(if value {
            FrameMode::Maximized
        } else {
            FrameMode::Restored
        });
    }
    /// The height of the row for the current frame mode.
    pub fn row_height(&self) -> f32 {
        let t = light_tokens();
        if self.mode == FrameMode::Restored {
            t.row_height
        } else {
            t.max_row_height
        }
    }
    /// The whole row in dips.
    pub fn row_rect(&self) -> Rect {
        r(
            self.bounds.x.0,
            self.bounds.y.0,
            self.bounds.width.0,
            self.row_height(),
        )
    }
    pub fn set_high_contrast(&mut self, value: Option<ContrastColors>) {
        self.contrast = value;
    }
    pub fn set_focused(&mut self, value: Option<CaptionHit>) {
        self.focused = value;
        if let Some(CaptionHit::Tab(i) | CaptionHit::TabClose(i)) = value {
            self.reveal(i);
        }
        self.fix_focus();
    }
    /// Moves keyboard focus into the strip, on the active tab. Keys go to the strip
    /// only while it has focus.
    pub fn focus_strip(&mut self) {
        self.set_focused(Some(CaptionHit::Tab(self.active)));
    }
    pub fn take_events(&mut self) -> Vec<ShellEvent> {
        std::mem::take(&mut self.events)
    }
    pub fn select(&mut self, id: TabId) {
        if let Some(index) = self.index_of(id) {
            self.select_index(index);
        }
    }
    fn select_index(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
            self.reveal(index);
            self.events.push(ShellEvent::SelectTab(self.tabs[index].id));
        }
    }
    /// Moves a tab to this position. A tab that has closed does nothing.
    pub fn reorder(&mut self, id: TabId, to: usize) {
        if let Some(from) = self.index_of(id) {
            self.reorder_index(from, to);
        }
    }
    fn reorder_index(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let tab = self.tabs.remove(from);
        let id = tab.id;
        self.tabs.insert(to, tab);
        let moved = |i: usize| {
            if i == from {
                to
            } else if from < i && to >= i {
                i - 1
            } else if from > i && to <= i {
                i + 1
            } else {
                i
            }
        };
        self.active = moved(self.active);
        self.focused = match self.focused {
            Some(CaptionHit::Tab(i)) => Some(CaptionHit::Tab(moved(i))),
            Some(CaptionHit::TabClose(i)) => Some(CaptionHit::TabClose(moved(i))),
            other => other,
        };
        self.hover = CaptionHit::None;
        self.reveal(to);
        self.events.push(ShellEvent::ReorderTab { tab: id, to });
    }
    /// Removes a tab. The owner calls this after it answers `CloseTabRequested`. An id
    /// that has already closed does nothing. The same document stays active unless the
    /// active tab itself goes, and then `SelectTab` names the new one. The last tab
    /// turns into a new blank tab.
    pub fn close(&mut self, id: TabId) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        self.clear_pointer();
        if self.tabs.len() == 1 {
            self.tabs[0] = Tab::new("Untitled");
            self.tabs[0].id = self.new_id();
            self.active = 0;
            self.arrange();
            return;
        }
        self.tabs.remove(index);
        let last = self.tabs.len() - 1;
        if index < self.active {
            self.active -= 1;
        } else if index == self.active {
            self.active = self.active.min(last);
            self.events
                .push(ShellEvent::SelectTab(self.tabs[self.active].id));
        }
        let after = |i: usize| {
            if i > index {
                i - 1
            } else if i == index {
                i.min(last)
            } else {
                i
            }
        };
        self.focused = match self.focused {
            Some(CaptionHit::Tab(i)) => Some(CaptionHit::Tab(after(i))),
            Some(CaptionHit::TabClose(i)) if i == index => Some(CaptionHit::Tab(after(i))),
            Some(CaptionHit::TabClose(i)) => Some(CaptionHit::TabClose(after(i))),
            other => other,
        };
        self.arrange();
        self.reveal(self.active);
    }
    /// The one hit test. Clicks, hover, `WM_NCHITTEST` and `WM_NCMOUSEMOVE` all use it.
    pub fn hit_test(&self, at: Point) -> CaptionHit {
        let t = light_tokens();
        // Terminal's main window answers HTTOP above its drag bar, which starts below the
        // top border (`NonClientIslandWindow.cpp`, `_ResizeDragBarWindow` and
        // `_OnNcHitTest`), so the border's row resizes even over the window buttons.
        let row = self.row_rect();
        if self.mode == FrameMode::Restored
            && at.y.0 >= 0.0
            && at.y.0 < row.y.0
            && at.x.0 >= row.x.0
            && at.x.0 < row.x.0 + row.width.0
        {
            return CaptionHit::ResizeTop;
        }
        if !inside(at, row) {
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
        // The band is measured from the window's top, as Terminal's is.
        if self.mode == FrameMode::Restored && at.y.0 < t.top_band {
            return CaptionHit::ResizeTop;
        }
        if self.overflow {
            for hit in [CaptionHit::ScrollLeft, CaptionHit::ScrollRight] {
                if inside(at, self.part_rect(hit)) {
                    return hit;
                }
            }
        }
        for hit in [CaptionHit::NewTab, CaptionHit::Caret] {
            if inside(at, self.part_rect(hit)) {
                return hit;
            }
        }
        if x >= self.clip_start()
            && x < self.clip_end()
            && let Some(first) = self.tab_rects.first()
        {
            // All tabs have one width, so the position gives the tab with no scan. A move
            // costs the same with 3 tabs or 10,000. Rounding can land one tab off.
            let guess = ((x - first.x.0) / self.tab_width).floor() as isize;
            for i in [guess, guess - 1, guess + 1] {
                let Ok(i) = usize::try_from(i) else {
                    continue;
                };
                if let Some(b) = self.tab_rects.get(i)
                    && inside(at, *b)
                {
                    return if x >= b.x.0 + b.width.0 - t.close_width {
                        CaptionHit::TabClose(i)
                    } else {
                        CaptionHit::Tab(i)
                    };
                }
            }
        }
        CaptionHit::Drag
    }
    /// Ends a press, drag and hover. `Blur` uses this: another window has the input
    /// now, and nothing may keep looking pressed.
    pub fn cancel_pointer(&mut self, ctx: &mut ViewContext) {
        if self.capturing {
            ctx.capture_mouse(false);
            self.capturing = false;
        }
        if self.pressed != CaptionHit::None {
            ctx.invalidate(self.part_rect(self.pressed));
        }
        if self.hover != CaptionHit::None {
            ctx.invalidate(self.part_rect(self.hover));
        }
        self.clear_pointer();
    }
    /// The pointer left the window. A button held by capture keeps its press.
    pub fn pointer_left(&mut self, ctx: &mut ViewContext) {
        if self.hover != CaptionHit::None && !self.capturing {
            ctx.invalidate(self.part_rect(self.hover));
            self.hover = CaptionHit::None;
        }
    }
    /// `WM_CAPTURECHANGED`: capture already ended, so this asks for no release.
    pub fn capture_lost(&mut self, ctx: &mut ViewContext) {
        self.capturing = false;
        self.cancel_pointer(ctx);
    }
    fn clear_pointer(&mut self) {
        if self.capturing {
            self.release_capture = true;
        }
        self.hover = CaptionHit::None;
        self.pressed = CaptionHit::None;
        self.drag = None;
        self.capturing = false;
    }
    fn clip_start(&self) -> f32 {
        self.bounds.x.0
            + if self.overflow {
                light_tokens().scroll_width
            } else {
                0.0
            }
    }
    fn clip_end(&self) -> f32 {
        self.split.x.0
            - if self.overflow {
                light_tokens().scroll_width
            } else {
                0.0
            }
    }
    fn caption_x(&self, slot: usize, t: ShellTokens) -> f32 {
        self.bounds.x.0 + self.bounds.width.0 - (3 - slot) as f32 * t.caption_width
    }
    /// The rectangle that one target paints in. Targets with no paint have no area.
    fn part_rect(&self, hit: CaptionHit) -> Rect {
        let t = light_tokens();
        let row = self.row_height();
        match hit {
            CaptionHit::Tab(i) | CaptionHit::TabClose(i) => {
                self.tab_rects.get(i).copied().unwrap_or_default()
            }
            // WinUI's `PrimaryBackgroundGrid` spans the divider's column too.
            CaptionHit::NewTab => r(
                self.split.x.0,
                self.split.y.0,
                t.split_part + t.separator_width,
                t.split_height,
            ),
            CaptionHit::Caret => r(
                self.split.x.0 + t.split_part + t.separator_width,
                self.split.y.0,
                t.split_part,
                t.split_height,
            ),
            CaptionHit::ScrollLeft if self.overflow => r(
                self.bounds.x.0,
                self.split.y.0,
                t.scroll_width,
                t.split_height,
            ),
            CaptionHit::ScrollRight if self.overflow => r(
                self.split.x.0 - t.scroll_width,
                self.split.y.0,
                t.scroll_width,
                t.split_height,
            ),
            CaptionHit::Minimize => r(self.caption_x(0, t), self.bounds.y.0, t.caption_width, row),
            CaptionHit::Maximize => r(self.caption_x(1, t), self.bounds.y.0, t.caption_width, row),
            CaptionHit::Close => r(self.caption_x(2, t), self.bounds.y.0, t.caption_width, row),
            _ => Rect::default(),
        }
    }
    /// The hover box of a tab's X.
    fn close_box(&self, index: usize, t: ShellTokens) -> Rect {
        let b = self.tab_rects[index];
        r(
            b.x.0 + b.width.0 - t.close_width + (t.close_width - t.close_box) / 2.0,
            b.y.0 + (b.height.0 - t.close_box) / 2.0,
            t.close_box,
            t.close_box,
        )
    }
    fn look(&self, hit: CaptionHit) -> Look {
        if self.pressed == hit && self.hover == hit {
            Look::Pressed
        } else if self.hover == hit && self.pressed == CaptionHit::None {
            Look::Hover
        } else {
            Look::Rest
        }
    }
    fn max_scroll(&self) -> usize {
        if !self.overflow || self.tab_width <= 0.0 {
            return 0;
        }
        let extra = self.tabs.len() as f32 * self.tab_width - self.visible_width;
        (extra / self.tab_width).ceil().max(0.0) as usize
    }
    /// Places every tab. This needs no measurer, so `add_tab` and `close` can call it
    /// at any time, and `tab_rects` always matches `tabs`.
    fn arrange(&mut self) {
        let t = light_tokens();
        let bounds = self.bounds;
        let available = (bounds.width.0 - 3.0 * t.caption_width - t.split_width()).max(0.0);
        self.tab_width = (available / self.tabs.len() as f32).clamp(t.min_width, t.max_width);
        self.overflow = self.tabs.len() as f32 * t.min_width > available;
        let left = bounds.x.0 + if self.overflow { t.scroll_width } else { 0.0 };
        self.visible_width = if self.overflow {
            (available - 2.0 * t.scroll_width).max(t.min_width)
        } else {
            available
        };
        self.scroll = self.scroll.min(self.max_scroll());
        let tab_top = bounds.y.0 + self.row_height() - t.tab_height;
        self.tab_rects = (0..self.tabs.len())
            .map(|i| {
                r(
                    left + (i as f32 - self.scroll as f32) * self.tab_width,
                    tab_top,
                    self.tab_width,
                    t.tab_height,
                )
            })
            .collect();
        self.split = r(
            if self.overflow {
                left + self.visible_width + t.scroll_width
            } else {
                left + self.tabs.len() as f32 * self.tab_width
            },
            tab_top + (t.tab_height - t.split_height) / 2.0,
            t.split_width(),
            t.split_height,
        );
        self.fix_focus();
    }
    /// Scrolls so that this whole tab shows.
    fn reveal(&mut self, index: usize) {
        self.arrange();
        if self.bounds.width.0 <= 0.0 {
            self.reveal_on_layout = true;
            return;
        }
        if index < self.scroll {
            self.scroll = index;
        } else if self.overflow && self.tab_width > 0.0 {
            let end = (index + 1) as f32 * self.tab_width;
            let first = ((end - self.visible_width) / self.tab_width)
                .ceil()
                .max(0.0) as usize;
            self.scroll = self.scroll.max(first);
        }
        self.arrange();
    }
    fn scroll_by(&mut self, delta: isize) {
        let next = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll());
        if next != self.scroll {
            self.scroll = next;
            self.arrange();
        }
    }
    /// Focus never points at something that is gone.
    fn fix_focus(&mut self) {
        let count = self.tabs.len();
        self.focused = match self.focused {
            Some(CaptionHit::Tab(i) | CaptionHit::TabClose(i)) if i >= count => {
                Some(CaptionHit::Tab(count - 1))
            }
            Some(CaptionHit::ScrollLeft | CaptionHit::ScrollRight) if !self.overflow => {
                Some(CaptionHit::NewTab)
            }
            other => other,
        };
    }
    /// Everything Tab stops at, in the order it shows on screen.
    fn focus_stops(&self, with_close: bool) -> Vec<CaptionHit> {
        let mut out = Vec::with_capacity(self.tabs.len() * 2 + 4);
        if self.overflow {
            out.push(CaptionHit::ScrollLeft);
        }
        for i in 0..self.tabs.len() {
            out.push(CaptionHit::Tab(i));
            if with_close {
                out.push(CaptionHit::TabClose(i));
            }
        }
        if self.overflow {
            out.push(CaptionHit::ScrollRight);
        }
        out.push(CaptionHit::NewTab);
        out.push(CaptionHit::Caret);
        out
    }
    fn move_focus_to(&mut self, next: CaptionHit, ctx: &mut ViewContext) {
        self.focused = Some(next);
        if let CaptionHit::Tab(i) | CaptionHit::TabClose(i) = next {
            self.reveal(i);
        }
        ctx.set_focus(Self::widget_id(next));
        ctx.invalidate(self.row_rect());
    }
    fn leave_focus(&mut self, ctx: &mut ViewContext) {
        self.focused = None;
        self.events.push(ShellEvent::FocusContent);
        ctx.set_focus(CONTENT_WIDGET);
        ctx.invalidate(self.row_rect());
    }
    fn focused_key(
        &mut self,
        key: u16,
        modifiers: Modifiers,
        repeat: bool,
        ctx: &mut ViewContext,
    ) -> Handled {
        let Some(current) = self.focused else {
            return Handled::No;
        };
        match key {
            VK_TAB => {
                let stops = self.focus_stops(true);
                let at = stops.iter().position(|hit| *hit == current).unwrap_or(0);
                let next = if modifiers.shift {
                    at.checked_sub(1).map(|i| stops[i])
                } else {
                    stops.get(at + 1).copied()
                };
                match next {
                    Some(hit) => self.move_focus_to(hit, ctx),
                    None => self.leave_focus(ctx),
                }
                Handled::Yes
            }
            VK_LEFT | VK_RIGHT => {
                let stops = self.focus_stops(false);
                let on = match current {
                    CaptionHit::TabClose(i) => CaptionHit::Tab(i),
                    other => other,
                };
                let at = stops.iter().position(|hit| *hit == on).unwrap_or(0);
                let count = stops.len();
                let next = if key == VK_LEFT {
                    (at + count - 1) % count
                } else {
                    (at + 1) % count
                };
                self.move_focus_to(stops[next], ctx);
                Handled::Yes
            }
            VK_ESCAPE => {
                self.leave_focus(ctx);
                Handled::Yes
            }
            VK_RETURN | VK_SPACE => {
                if !repeat {
                    self.activate(current);
                    ctx.invalidate(self.row_rect());
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn activate(&mut self, hit: CaptionHit) {
        match hit {
            CaptionHit::NewTab => self.events.push(ShellEvent::NewTab),
            CaptionHit::Caret => self.events.push(ShellEvent::OpenCaretList(self.split)),
            CaptionHit::Tab(i) => self.select_index(i),
            CaptionHit::TabClose(i) => {
                if let Some(id) = self.tab_id(i) {
                    self.events.push(ShellEvent::CloseTabRequested(id));
                }
            }
            CaptionHit::ScrollLeft => self.scroll_by(-1),
            CaptionHit::ScrollRight => self.scroll_by(1),
            CaptionHit::Minimize | CaptionHit::Maximize | CaptionHit::Close => {
                self.events.push(ShellEvent::CaptionCommand(hit));
            }
            CaptionHit::None | CaptionHit::Drag | CaptionHit::ResizeTop => {}
        }
    }
    fn set_hover(&mut self, hit: CaptionHit, ctx: &mut ViewContext) {
        if hit != self.hover {
            ctx.invalidate(self.part_rect(self.hover));
            ctx.invalidate(self.part_rect(hit));
            self.hover = hit;
        }
    }
    fn mouse_move(&mut self, at: Point, ctx: &mut ViewContext) -> Handled {
        let t = light_tokens();
        let hit = self.hit_test(at);
        self.set_hover(hit, ctx);
        if let Some(drag) = self.drag {
            if (at.y.0 - drag.start.y.0).abs() > self.row_height() {
                if let Some(tab) = self.tab_id(drag.index) {
                    self.events.push(ShellEvent::TearOffTab { tab, at });
                }
                self.cancel_pointer(ctx);
                return Handled::Yes;
            }
            // Past the visible tabs, each move steps one place and scrolls with it, so
            // that a dragged tab can reach a tab that is scrolled off.
            let at_edge = if !self.overflow {
                None
            } else if at.x.0 < self.clip_start() {
                drag.index.checked_sub(1)
            } else if at.x.0 >= self.clip_end() {
                Some(drag.index + 1).filter(|next| *next < self.tabs.len())
            } else {
                None
            };
            let target = at_edge.or(match hit {
                CaptionHit::Tab(to) | CaptionHit::TabClose(to)
                    if to != drag.index && (at.x.0 - drag.start.x.0).abs() > t.drag_threshold =>
                {
                    Some(to)
                }
                _ => None,
            });
            if let Some(to) = target {
                self.reorder_index(drag.index, to);
                self.drag = Some(TabDrag {
                    index: to,
                    start: at,
                    moved: true,
                });
                self.pressed = CaptionHit::Tab(to);
                self.hover = CaptionHit::Tab(to);
                ctx.invalidate(self.row_rect());
            }
        }
        if hit != CaptionHit::None || self.capturing {
            Handled::Yes
        } else {
            Handled::No
        }
    }
    fn mouse_down(&mut self, at: Point, left: bool, ctx: &mut ViewContext) -> Handled {
        let hit = self.hit_test(at);
        if hit == CaptionHit::None {
            // A click in the text area takes keyboard focus away from the strip.
            if self.focused.take().is_some() {
                self.events.push(ShellEvent::FocusContent);
                ctx.invalidate(self.row_rect());
            }
            return Handled::No;
        }
        // The rest of the row never reaches the text area, even in full screen, where
        // its empty part is client area because a full-screen window does not move.
        if !left || matches!(hit, CaptionHit::Drag | CaptionHit::ResizeTop) {
            return Handled::Yes;
        }
        self.pressed = hit;
        self.hover = hit;
        if let CaptionHit::Tab(index) = hit {
            self.drag = Some(TabDrag {
                index,
                start: at,
                moved: false,
            });
        }
        self.capturing = true;
        ctx.capture_mouse(true);
        ctx.invalidate(self.part_rect(hit));
        Handled::Yes
    }
    fn mouse_up(&mut self, at: Point, ctx: &mut ViewContext) -> Handled {
        let pressed = self.pressed;
        let hit = self.hit_test(at);
        if pressed == CaptionHit::None {
            return if hit == CaptionHit::None {
                Handled::No
            } else {
                Handled::Yes
            };
        }
        let moved = self.drag.is_some_and(|drag| drag.moved);
        ctx.invalidate(self.part_rect(pressed));
        self.drag = None;
        self.pressed = CaptionHit::None;
        self.hover = hit;
        if self.capturing {
            self.capturing = false;
            ctx.capture_mouse(false);
        }
        ctx.invalidate(self.part_rect(hit));
        if hit == pressed && !moved {
            self.activate(pressed);
            if !matches!(
                pressed,
                CaptionHit::Minimize | CaptionHit::Maximize | CaptionHit::Close
            ) {
                ctx.invalidate(self.row_rect());
            }
        }
        Handled::Yes
    }
}
impl View for TabStrip {
    fn layout(&mut self, bounds: Rect, _measure: &dyn TextMeasurer) {
        let new_width = bounds.width.0 != self.bounds.width.0;
        self.bounds = bounds;
        self.arrange();
        if self.reveal_on_layout || new_width {
            self.reveal_on_layout = false;
            self.reveal(self.active);
        }
    }
    fn handle(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        match event.kind {
            InputKind::KeyDown {
                virtual_key: VK_MENU | VK_LMENU | VK_RMENU,
                modifiers,
                repeat,
            } => {
                // The first press arms. A repeat changes nothing: another key may have
                // already disarmed this press.
                if !repeat {
                    self.alt_armed = !modifiers.ctrl && !modifiers.shift && !modifiers.win;
                }
                Handled::No
            }
            InputKind::KeyUp {
                virtual_key: VK_MENU | VK_LMENU | VK_RMENU,
                modifiers,
            } => {
                let open = self.alt_armed && !modifiers.ctrl && !modifiers.shift && !modifiers.win;
                self.alt_armed = false;
                if open {
                    self.events.push(ShellEvent::OpenCaretList(self.split));
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            InputKind::KeyDown {
                virtual_key,
                modifiers,
                repeat,
            } => {
                self.alt_armed = false;
                // Chords belong to the system and the command table.
                if modifiers.alt || modifiers.ctrl || modifiers.win {
                    return Handled::No;
                }
                self.focused_key(virtual_key, modifiers, repeat, ctx)
            }
            InputKind::MouseMove { at } => self.mouse_move(at, ctx),
            InputKind::MouseDown { at, button, .. } => {
                self.alt_armed = false;
                self.mouse_down(at, button == MouseButton::Left, ctx)
            }
            InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                ..
            } => self.mouse_up(at, ctx),
            InputKind::MouseUp { at, .. } => {
                if inside(at, self.row_rect()) {
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            InputKind::Wheel {
                at,
                delta_x,
                delta_y,
                ..
            } => {
                if !inside(at, self.row_rect()) {
                    return Handled::No;
                }
                // One notch is 1.0. Small steps from a fine wheel add up to one tab.
                self.wheel += delta_x - delta_y;
                let steps = self.wheel.trunc();
                self.wheel -= steps;
                if self.overflow && steps != 0.0 {
                    self.scroll_by(steps as isize);
                    // A different tab is under the pointer now.
                    self.hover = self.hit_test(at);
                    ctx.invalidate(self.row_rect());
                }
                Handled::Yes
            }
            InputKind::Blur => {
                self.window_active = false;
                self.alt_armed = false;
                self.cancel_pointer(ctx);
                ctx.invalidate(self.row_rect());
                Handled::Yes
            }
            InputKind::Focus => {
                self.window_active = true;
                ctx.invalidate(self.row_rect());
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
    fn draw(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        let mut t = ShellTokens::from_theme(theme);
        if let Some(contrast) = self.contrast {
            t = t.with_contrast(contrast);
        }
        list.push(DrawCommand::FillRect {
            rect: self.row_rect(),
            color: t.row,
        });
        self.draw_tabs(list, t, measure);
        if self.overflow {
            for (hit, code) in [
                (CaptionHit::ScrollLeft, 0xe76b),
                (CaptionHit::ScrollRight, 0xe76c),
            ] {
                self.draw_button(list, hit, code, t, measure);
            }
        }
        self.draw_split(list, t, measure);
        // A fill snaps to whole pixels. A line 1 wide on a whole x would blur across two.
        list.push(DrawCommand::FillRect {
            rect: r(
                self.split.x.0 + t.split_part,
                self.split.y.0,
                t.separator_width,
                t.split_height,
            ),
            color: t.separator,
        });
        self.draw_caption(list, t, measure);
    }
    fn widgets(&self) -> Option<Widget> {
        let t = light_tokens();
        let button = |hit: CaptionHit, name: &str, bounds: Rect| Widget {
            id: Self::widget_id(hit),
            bounds,
            role: WidgetRole::Button,
            name: name.into(),
            value: None,
            states: WidgetStates {
                enabled: true,
                focused: self.focused == Some(hit),
                ..Default::default()
            },
            children: Vec::new(),
        };
        let mut children = Vec::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            let b = self.tab_rects[i];
            children.push(Widget {
                id: Self::widget_id(CaptionHit::Tab(i)),
                bounds: b,
                role: WidgetRole::Tab,
                name: tab.title.clone(),
                value: None,
                states: WidgetStates {
                    enabled: true,
                    selected: i == self.active,
                    focused: self.focused == Some(CaptionHit::Tab(i)),
                    ..Default::default()
                },
                children: vec![button(
                    CaptionHit::TabClose(i),
                    "Close tab",
                    r(
                        b.x.0 + b.width.0 - t.close_width,
                        b.y.0,
                        t.close_width,
                        b.height.0,
                    ),
                )],
            });
        }
        for (hit, name) in [
            (CaptionHit::NewTab, "New tab"),
            (CaptionHit::Caret, "Tab list"),
            (CaptionHit::Minimize, "Minimize"),
            (CaptionHit::Maximize, "Maximize or restore"),
            (CaptionHit::Close, "Close window"),
        ] {
            children.push(button(hit, name, self.part_rect(hit)));
        }
        if self.overflow {
            for (hit, name) in [
                (CaptionHit::ScrollLeft, "Scroll tabs left"),
                (CaptionHit::ScrollRight, "Scroll tabs right"),
            ] {
                children.push(button(hit, name, self.part_rect(hit)));
            }
        }
        Some(Widget {
            id: WidgetId(0),
            bounds: self.row_rect(),
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
impl TabStrip {
    /// Ids in separate ranges, so that no count of tabs makes two widgets share one.
    pub fn widget_id(hit: CaptionHit) -> WidgetId {
        WidgetId(match hit {
            CaptionHit::Tab(i) => TAB_IDS + i as u64,
            CaptionHit::TabClose(i) => TAB_CLOSE_IDS + i as u64,
            CaptionHit::NewTab => 1,
            CaptionHit::Caret => 2,
            CaptionHit::Minimize => 3,
            CaptionHit::Maximize => 4,
            CaptionHit::Close => 5,
            CaptionHit::ScrollLeft => 6,
            CaptionHit::ScrollRight => 7,
            CaptionHit::None | CaptionHit::Drag | CaptionHit::ResizeTop => 0,
        })
    }
    fn draw_tabs(&self, list: &mut DisplayList, t: ShellTokens, measure: &dyn TextMeasurer) {
        let (start, end) = (self.clip_start(), self.clip_end());
        let row = self.row_rect();
        list.push(DrawCommand::PushClip(r(
            start,
            row.y.0,
            (end - start).max(0.0),
            row.height.0,
        )));
        let contrast = self.contrast.is_some();
        for (i, tab) in self.tabs.iter().enumerate().skip(self.scroll) {
            let b = self.tab_rects[i];
            if b.x.0 >= end {
                break;
            }
            if b.x.0 + b.width.0 <= start {
                continue;
            }
            let active = i == self.active;
            let close_hit = CaptionHit::TabClose(i);
            let hovering = self.hover == CaptionHit::Tab(i) || self.hover == close_hit;
            let look = if self.look(CaptionHit::Tab(i)) == Look::Pressed {
                Look::Pressed
            } else if hovering && self.pressed == CaptionHit::None {
                Look::Hover
            } else {
                Look::Rest
            };
            // The box runs past the row's bottom, and the clip cuts it there. So only
            // the top corners are round, and an active tab joins the text area.
            let shape = r(b.x.0, b.y.0, b.width.0, b.height.0 + t.radius);
            if active {
                list.push(DrawCommand::RoundedRect {
                    rect: shape,
                    radius: Dip(t.radius),
                    color: t.active,
                });
            } else if contrast && look != Look::Rest {
                list.push(DrawCommand::StrokeRect {
                    rect: shape,
                    radius: Dip(t.radius),
                    width: Dip(1.0),
                    color: t.hover,
                });
            } else if look != Look::Rest {
                list.push(DrawCommand::RoundedRect {
                    rect: shape,
                    radius: Dip(t.radius),
                    color: if look == Look::Pressed {
                        t.pressed
                    } else {
                        t.hover
                    },
                });
            }
            let on_highlight = contrast && active;
            let ink = if on_highlight {
                t.button_text_pressed
            } else if active && self.window_active {
                t.text
            } else {
                t.dim
            };
            let button_ink = if on_highlight {
                t.button_text_pressed
            } else if self.window_active {
                t.button_text
            } else {
                t.dim
            };
            let text = ellipsis(
                &tab.title,
                (b.width.0 - t.text_inset - t.close_width).max(0.0),
                measure,
                t.text_token,
            );
            let metrics = measure.measure(&text, t.text_token);
            list.push(DrawCommand::TextRun {
                at: p(
                    b.x.0 + t.text_inset,
                    b.y.0 + (b.height.0 - metrics.height.0) / 2.0 + metrics.baseline.0,
                ),
                text,
                font: t.text_font.into(),
                font_size: Dip(t.text_font_size),
                color: ink,
            });
            let close = self.close_box(i, t);
            match self.look(close_hit) {
                Look::Rest => {}
                close_look if contrast => list.push(DrawCommand::StrokeRect {
                    rect: close,
                    radius: Dip(t.button_radius),
                    width: Dip(if close_look == Look::Pressed {
                        2.0
                    } else {
                        1.0
                    }),
                    color: if on_highlight { ink } else { t.hover },
                }),
                close_look => list.push(DrawCommand::RoundedRect {
                    rect: close,
                    radius: Dip(t.button_radius),
                    color: if close_look == Look::Pressed {
                        t.pressed
                    } else {
                        t.hover
                    },
                }),
            }
            if tab.dirty && !hovering {
                let metrics = measure.measure("•", t.text_token);
                list.push(DrawCommand::TextRun {
                    at: centered(close, metrics),
                    text: "•".into(),
                    font: t.text_font.into(),
                    font_size: Dip(t.text_font_size),
                    color: button_ink,
                });
            } else {
                icon(
                    list,
                    close,
                    0xe8bb,
                    IconStyle {
                        size: t.caption_icon,
                        token: t.caption_icon_token,
                        color: button_ink,
                        scale: self.scale.get(),
                    },
                    measure,
                );
            }
            if self.focused == Some(CaptionHit::Tab(i)) {
                // On the highlight fill, the ring sits inside so that it shows.
                let inset = if on_highlight { 3.0 } else { 0.0 };
                list.push(DrawCommand::StrokeRect {
                    rect: r(
                        shape.x.0 + inset,
                        shape.y.0 + inset,
                        shape.width.0 - 2.0 * inset,
                        shape.height.0 - t.radius - inset,
                    ),
                    radius: Dip(t.radius - inset),
                    width: Dip(t.focus_width),
                    color: if on_highlight { ink } else { t.focus },
                });
            }
            if self.focused == Some(close_hit) {
                list.push(DrawCommand::StrokeRect {
                    rect: close,
                    radius: Dip(t.button_radius),
                    width: Dip(t.focus_width),
                    color: if on_highlight { ink } else { t.focus },
                });
            }
        }
        list.push(DrawCommand::PopClip);
    }
    fn draw_button(
        &self,
        list: &mut DisplayList,
        hit: CaptionHit,
        code: u32,
        t: ShellTokens,
        measure: &dyn TextMeasurer,
    ) {
        let b = self.part_rect(hit);
        let look = self.look(hit);
        if look != Look::Rest {
            list.push(DrawCommand::RoundedRect {
                rect: b,
                radius: Dip(t.button_radius),
                color: if look == Look::Pressed {
                    t.pressed
                } else {
                    t.hover
                },
            });
        }
        let contrast = self.contrast.is_some();
        let color = if look == Look::Pressed || (contrast && look == Look::Hover) {
            t.button_text_pressed
        } else if self.window_active {
            t.button_text
        } else {
            t.dim
        };
        icon(
            list,
            b,
            code,
            IconStyle {
                size: t.icon,
                token: t.icon_token,
                color,
                scale: self.scale.get(),
            },
            measure,
        );
        if self.focused == Some(hit) {
            list.push(DrawCommand::StrokeRect {
                rect: b,
                radius: Dip(t.button_radius),
                width: Dip(t.focus_width),
                color: t.focus,
            });
        }
    }
    /// WinUI's `SplitButton.xaml` and Terminal's `TabRowControl.xaml`. The fills are plain
    /// rectangles: only the outer corners round, from the 4-radius root grid's clip. The
    /// `+` fill spans the divider's column. The caret's 12 x 12 box is right aligned with
    /// padding 0,0,12,0, so that its center is at x = 13 of the 31-wide part.
    fn draw_split(&self, list: &mut DisplayList, t: ShellTokens, measure: &dyn TextMeasurer) {
        let contrast = self.contrast.is_some();
        let s = self.split;
        let r_btn = t.button_radius;
        let caret_x = s.x.0 + t.split_part + t.separator_width;
        for (hit, clip, shape) in [
            (
                CaptionHit::NewTab,
                self.part_rect(CaptionHit::NewTab),
                r(
                    s.x.0,
                    s.y.0,
                    t.split_part + t.separator_width + r_btn,
                    t.split_height,
                ),
            ),
            (
                CaptionHit::Caret,
                self.part_rect(CaptionHit::Caret),
                r(caret_x - r_btn, s.y.0, t.split_part + r_btn, t.split_height),
            ),
        ] {
            let look = self.look(hit);
            if look != Look::Rest {
                list.push(DrawCommand::PushClip(clip));
                list.push(DrawCommand::RoundedRect {
                    rect: shape,
                    radius: Dip(r_btn),
                    color: if look == Look::Pressed {
                        t.pressed
                    } else {
                        t.hover
                    },
                });
                list.push(DrawCommand::PopClip);
            }
        }
        let plus_look = self.look(CaptionHit::NewTab);
        icon(
            list,
            r(s.x.0, s.y.0, t.split_part, t.split_height),
            0xe710,
            IconStyle {
                size: t.icon,
                token: t.icon_token,
                color: if plus_look == Look::Pressed || (contrast && plus_look == Look::Hover) {
                    t.button_text_pressed
                } else if self.window_active {
                    t.button_text
                } else {
                    t.dim
                },
                scale: self.scale.get(),
            },
            measure,
        );
        let caret_look = self.look(CaptionHit::Caret);
        icon(
            list,
            r(
                caret_x + t.split_part - t.caret_right_padding - t.caret_box + t.caret_nudge_x,
                s.y.0 + (t.split_height - t.caret_box) / 2.0 + t.caret_nudge,
                t.caret_box,
                t.caret_box,
            ),
            0xe96e,
            IconStyle {
                size: t.caret_icon,
                token: t.caret_token,
                color: match caret_look {
                    Look::Pressed => t.caret_text_pressed,
                    Look::Hover if contrast => t.caret_text_pressed,
                    Look::Hover => t.button_text,
                    Look::Rest if self.window_active => t.caret_text,
                    Look::Rest => t.dim,
                },
                scale: self.scale.get(),
            },
            measure,
        );
        for hit in [CaptionHit::NewTab, CaptionHit::Caret] {
            if self.focused == Some(hit) {
                list.push(DrawCommand::StrokeRect {
                    rect: self.part_rect(hit),
                    radius: Dip(r_btn),
                    width: Dip(t.focus_width),
                    color: t.focus,
                });
            }
        }
    }
    fn draw_caption(&self, list: &mut DisplayList, t: ShellTokens, measure: &dyn TextMeasurer) {
        let restore = self.mode != FrameMode::Restored;
        let contrast = self.contrast.is_some();
        // `MinMaxCloseControl.xaml`: high contrast has its own heavier glyphs.
        let (minimize, maximize, restore_glyph, close) = if contrast {
            (0xef2d, 0xef2e, 0xef2f, 0xef2c)
        } else {
            (0xe921, 0xe922, 0xe923, 0xe8bb)
        };
        for (hit, code) in [
            (CaptionHit::Minimize, minimize),
            (
                CaptionHit::Maximize,
                if restore { restore_glyph } else { maximize },
            ),
            (CaptionHit::Close, close),
        ] {
            let b = self.part_rect(hit);
            let look = self.look(hit);
            let is_close = hit == CaptionHit::Close;
            let fill = match (look, is_close) {
                (Look::Rest, _) => None,
                (Look::Hover, true) => Some(t.close_hover),
                (Look::Pressed, true) => Some(t.close_pressed),
                (Look::Hover, false) => Some(t.hover),
                (Look::Pressed, false) => Some(t.pressed),
            };
            if let Some(color) = fill {
                list.push(DrawCommand::FillRect { rect: b, color });
            }
            let color = match look {
                Look::Hover if is_close => t.close_text,
                Look::Pressed if is_close => t.close_text_pressed,
                Look::Hover | Look::Pressed if contrast => t.close_text,
                _ if self.window_active => t.text,
                _ => t.caption_dim,
            };
            icon(
                list,
                b,
                code,
                IconStyle {
                    size: t.caption_icon,
                    token: t.caption_icon_token,
                    color,
                    scale: self.scale.get(),
                },
                measure,
            );
        }
    }
}
pub fn maximized_insets(frame_px: i32, padded_border_px: i32) -> i32 {
    frame_px + padded_border_px
}
/// `hit_test` for a point in device pixels of the client area.
pub fn caption_hit(at_px: Point, scale: f32, strip: &TabStrip) -> CaptionHit {
    strip.hit_test(p(at_px.x.0 / scale, at_px.y.0 / scale))
}
