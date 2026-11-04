//! The top row as plain data: tabs, the + button and the window buttons. It holds
//! no window handle, so that every rule here can run in a test with no desktop.
use crate::ShellError;
use qe_api::motion::{
    BUTTON_FADE_IN_MS, BUTTON_FADE_OUT_MS, CLOSE_FADE_MS, Clock, LIST_QUIET_MS, OPEN_FADE_MS,
    SLIDE_CURVE, SLIDE_MS, part_of,
};
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
    /// A menu bar label opened. The owner hosts its popup at this rectangle.
    OpenMenuGroup(usize, Rect),
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
    /// The pointer left the window's client area or its frame. Hover in the text area ends.
    ContentPointerLeft,
    /// A timer from `ShellHandle::set_timer` or `ViewRequest::RequestTimer` fired.
    Timer(u32),
    /// Another thread called `ShellWaker::wake`. Several calls can arrive as one event.
    Wake,
    /// A view asked to end the input method's composition.
    CancelComposition,
    /// A view asked for the context menu at this point.
    ContextMenu(qe_api::Point),
    /// A view asked to run a command, with `ViewRequest::Run`.
    RunCommand(CommandId),
    /// The window was destroyed. The owner quits the message loop after the last one.
    WindowDestroyed,
    Problem(ShellError),
    /// A painted control changed in a way the top row's owner cannot see: a menu's selected
    /// row, say. An owner with a screen reader's link reports it.
    WidgetsChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionHit {
    None,
    Drag,
    Tab(usize),
    TabClose(usize),
    NewTab,
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
    pub plus_height: f32,
    pub plus_width: f32,
    pub scroll_width: f32,
    pub caption_width: f32,
    pub top_band: f32,
    pub icon: f32,
    pub caption_icon: f32,
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
    pub caption_dim: Color,
    pub hover: Color,
    pub pressed: Color,
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
            plus_height: 24.0,
            plus_width: 31.0,
            scroll_width: 20.0,
            caption_width: theme.caption_button_width.0,
            top_band: 8.0,
            icon: 12.0,
            caption_icon: 10.0,
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
            active: theme.tab_selected,
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
        self.caption_dim = c.gray_text;
        self.hover = c.highlight;
        self.pressed = c.highlight;
        self.close_hover = c.highlight;
        self.close_pressed = c.highlight;
        self.close_text = c.highlight_text;
        self.close_text_pressed = c.highlight_text;
        self.focus = c.window_text;
        self
    }
}

/// The widget id that means "the text area", in a `ViewRequest::SetFocus`.
pub const CONTENT_WIDGET: WidgetId = WidgetId(u64::MAX);
const TAB_IDS: u64 = 1 << 32;
const TAB_CLOSE_IDS: u64 = 2 << 32;

const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0d;
const VK_ESCAPE: u16 = 0x1b;
const VK_SPACE: u16 = 0x20;
const VK_LEFT: u16 = 0x25;
const VK_RIGHT: u16 = 0x27;
/// A release this near a press made during a slide counts as the same click, even though
/// the tabs have reached their places by then.
const PRESS_SLOP: f32 = 4.0;

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

/// Where one thing is drawn while it slides. Its end is always its final place, so that a
/// change of place during the slide needs no new work and makes no jump.
#[derive(Clone, Copy)]
struct Slide {
    from_x: f32,
    from_width: f32,
    start: f64,
}
impl Slide {
    fn place(self, end: Rect, now: f64) -> Rect {
        let part = SLIDE_CURVE.at(part_of(now, self.start, SLIDE_MS)) as f32;
        Rect {
            x: Dip(self.from_x + (end.x.0 - self.from_x) * part),
            width: Dip(self.from_width + (end.width.0 - self.from_width) * part),
            ..end
        }
    }
    fn done(self, now: f64) -> bool {
        now - self.start >= SLIDE_MS
    }
}
/// What is left of a closed tab: pixels only. No hit test and no screen reader sees it.
struct Ghost {
    rect: Rect,
    title: String,
    dirty: bool,
    active: bool,
    start: f64,
}
/// A window button's hover fill: 0 is none, 1 is all of it.
#[derive(Clone, Copy, Default)]
struct ButtonFade {
    lit: bool,
    moving: bool,
    from: f32,
    start: f64,
}
impl ButtonFade {
    fn value(self, now: f64) -> f32 {
        if !self.moving {
            return if self.lit { 1.0 } else { 0.0 };
        }
        if self.lit {
            (self.from + ((now - self.start) / BUTTON_FADE_IN_MS) as f32).min(1.0)
        } else {
            (self.from - ((now - self.start) / BUTTON_FADE_OUT_MS) as f32).max(0.0)
        }
    }
    fn done(self, now: f64) -> bool {
        let value = self.value(now);
        if self.lit { value >= 1.0 } else { value <= 0.0 }
    }
}
/// Where the row's parts were drawn just before a change.
struct DrawnBefore {
    tabs: Vec<(TabId, f32, f32)>,
    plus: (f32, f32),
}
/// Everything that moves in the row. Only drawing reads it. Hit tests, the order of the tabs,
/// focus and the screen reader's tree all come from the final places.
struct StripMotion {
    /// `motion_off`: every change shows its end state at once. A new strip starts here, and
    /// a window turns motion on when it may.
    off: bool,
    clock: Clock,
    /// The time of the frame being drawn. A paint between ticks draws the same frame.
    now: f64,
    /// Only tabs in or near the view slide, so that this grows with the row, not the list.
    slides: Vec<(TabId, Slide)>,
    plus: Option<Slide>,
    /// At most `MAX_GHOSTS`, the newest, however many tabs close at once.
    ghosts: Vec<Ghost>,
    fade_in: Option<(TabId, f64)>,
    buttons: [ButtonFade; 3],
    last_list_change: Option<f64>,
    quiet_ms: f64,
    /// What the last frame drew that moves, so that the next one also repaints it.
    drawn: Option<Rect>,
}
/// How many closed tabs may fade at once. A person sees no more than a few, and a frame's cost
/// must not follow the number of tabs that closed.
const MAX_GHOSTS: usize = 8;
const CAPTION_HITS: [CaptionHit; 3] = [
    CaptionHit::Minimize,
    CaptionHit::Maximize,
    CaptionHit::Close,
];
fn union_rect(a: Option<Rect>, b: Rect) -> Rect {
    match a {
        None => b,
        Some(a) => {
            let (left, top) = (a.x.0.min(b.x.0), a.y.0.min(b.y.0));
            let right = (a.x.0 + a.width.0).max(b.x.0 + b.width.0);
            let bottom = (a.y.0 + a.height.0).max(b.y.0 + b.height.0);
            r(left, top, right - left, bottom - top)
        }
    }
}
fn mix(a: Color, b: Color, part: f32) -> Color {
    let channel =
        |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * part).round() as u8;
    Color {
        r: channel(a.r, b.r),
        g: channel(a.g, b.g),
        b: channel(a.b, b.b),
        a: channel(a.a, b.a),
    }
}

pub struct TabStrip {
    tabs: Vec<Tab>,
    active: usize,
    bounds: Rect,
    tab_width: f32,
    /// The tab width while the pointer stays in the row after a close by mouse. The next tab
    /// then comes under the pointer, so that the next click hits its X. WinUI's rule.
    held_width: Option<f32>,
    /// While set, the row keeps this overflow state and its scroll place, whatever the list
    /// says. A close by mouse sets it when the next tab would not come under the pointer.
    held_row: Option<bool>,
    /// Where the mouse was last seen on this row.
    pointer: Option<Point>,
    pointer_in_row: bool,
    /// The X or a middle click asked to close this tab. A close of any other kind has no hold.
    pointer_close: Option<TabId>,
    /// A press during a slide that took what was drawn over the final place: where, and what.
    drawn_press: Option<(Point, CaptionHit)>,
    motion: StripMotion,
    /// Always one rectangle per tab, whether or not `layout` has run.
    tab_rects: Vec<Rect>,
    plus: Rect,
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
    /// The tab a middle press began on. It has the mouse, as a left press does, until the
    /// release, which closes this tab if the pointer is still on it.
    middle_press: Option<TabId>,
    capturing: bool,
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
            held_width: None,
            held_row: None,
            pointer: None,
            pointer_in_row: false,
            pointer_close: None,
            drawn_press: None,
            motion: StripMotion {
                off: true,
                clock: Clock::real(),
                now: 0.0,
                slides: Vec::new(),
                plus: None,
                ghosts: Vec::new(),
                fade_in: None,
                buttons: [ButtonFade::default(); 3],
                last_list_change: None,
                quiet_ms: LIST_QUIET_MS,
                drawn: None,
            },
            tab_rects: Vec::new(),
            plus: Rect::default(),
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
            middle_press: None,
            capturing: false,
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
        let before = self.drawn_before();
        self.clear_hold();
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.clear_pointer();
        self.reveal(self.active);
        self.after_list_change(before, None, Some(id));
        self.sync_button_fades();
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
    /// True while a press that began on this row holds the mouse.
    pub fn holds_mouse(&self) -> bool {
        self.capturing
    }
    pub fn take_capture_release(&mut self) -> bool {
        std::mem::take(&mut self.release_capture)
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
    pub fn plus_rect(&self) -> Rect {
        self.plus
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
            self.clear_hold();
            self.finish_motion();
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
        let by_pointer = self.pointer_close.take() == Some(id) && self.pointer_in_row;
        let before = self.drawn_before();
        let ghost = before.is_some().then(|| self.ghost_of(index));
        let old_list_width = self.tab_width * self.tabs.len() as f32;
        self.clear_pointer();
        if self.tabs.len() == 1 {
            self.tabs[0] = Tab::new("Untitled");
            self.tabs[0].id = self.new_id();
            self.active = 0;
            self.clear_hold();
            self.arrange();
            self.after_list_change(before, None, None);
            self.sync_button_fades();
            return;
        }
        let was_rightmost = index + 1 == self.tabs.len();
        let was_active = index == self.active;
        // A selected tab that the person scrolled away from has no claim on the row.
        let selected_matters = was_active || self.tab_shows_whole(self.active);
        let next = self.tab_id(index + 1);
        let row_before = (self.overflow, self.scroll, self.tab_width);
        let on_x = self
            .pointer
            .is_some_and(|at| matches!(self.hit_test(at), CaptionHit::TabClose(_)));
        self.tabs.remove(index);
        self.held_row = None;
        self.held_width = if by_pointer {
            self.width_to_hold(was_rightmost, old_list_width)
        } else {
            None
        };
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
        // A close by mouse does not move the row to show a tab that is scrolled off.
        if !by_pointer || was_active {
            self.reveal(self.active);
        }
        if by_pointer && row_before.0 {
            let aimed = next.or_else(|| self.tab_id(self.tabs.len() - 1));
            self.keep_next_under_pointer(aimed, on_x, row_before, selected_matters);
        }
        self.after_list_change(before, ghost, None);
        if by_pointer && let Some(at) = self.pointer {
            // The next tab is under the pointer now, so its X lights up with no move.
            self.hover = self.hit_test(at);
        }
        self.sync_button_fades();
    }
    /// Ends a hold of the widths and of the row. True if there was one.
    fn clear_hold(&mut self) -> bool {
        let row = self.held_row.take().is_some();
        self.held_width.take().is_some() || row
    }
    /// With the scroll arrows in play, a close can move every tab: the arrows go, or the scroll
    /// place falls back. If the tab the person would aim at next is then not under the pointer,
    /// the row stays as it was until the pointer leaves. No click closes an unseen target.
    fn keep_next_under_pointer(
        &mut self,
        aimed: Option<TabId>,
        on_x: bool,
        row: (bool, usize, f32),
        selected_matters: bool,
    ) {
        // The same part of the next tab: its X after a click on an X.
        let under = self.pointer.and_then(|at| match self.hit_test(at) {
            CaptionHit::TabClose(i) if on_x => self.tab_id(i),
            CaptionHit::Tab(i) if !on_x => self.tab_id(i),
            _ => None,
        });
        if under == aimed {
            return;
        }
        let normal = (self.scroll, self.held_width);
        let (overflow, scroll, width) = row;
        self.held_row = Some(overflow);
        self.held_width = Some(width);
        self.scroll = scroll;
        self.arrange();
        // The row is not kept if that shows no tabs or hides the selected tab.
        let selected_shows = !selected_matters || self.tab_shows_whole(self.active);
        if selected_shows && self.scroll < self.tabs.len() {
            return;
        }
        self.held_row = None;
        (self.scroll, self.held_width) = normal;
        self.arrange();
        if selected_matters {
            self.reveal(self.active);
        }
    }
    /// The width that the tabs keep after a close by mouse, or nothing if they may fill.
    /// Closing the rightmost tab keeps the list's right end in place, up to the maximum.
    fn width_to_hold(&self, was_rightmost: bool, old_list_width: f32) -> Option<f32> {
        let t = light_tokens();
        let count = self.tabs.len() as f32;
        let available = (self.bounds.width.0 - 3.0 * t.caption_width - t.plus_width).max(0.0);
        if count * t.min_width > available {
            return None;
        }
        let fill = (available / count).clamp(t.min_width, t.max_width);
        let width = if was_rightmost {
            (old_list_width / count).min(t.max_width)
        } else {
            self.tab_width
        };
        (width < fill).then_some(width)
    }
    /// Ends the hold, and the widths jump, as WinUI's do. No motion follows.
    fn release_hold(&mut self, ctx: &mut ViewContext) {
        if self.clear_hold() {
            self.motion.slides.clear();
            self.motion.plus = None;
            self.arrange();
            ctx.invalidate(self.row_rect());
        }
    }
    /// True where the pointer holds the widths: the part of the row left of the window buttons.
    pub fn in_hold_zone(&self, at: Point) -> bool {
        inside(at, self.row_rect()) && at.x.0 < self.caption_x(0, light_tokens())
    }
    /// Where each tab is drawn now, for a test.
    #[doc(hidden)]
    pub fn drawn_tab_places(&self) -> Vec<Rect> {
        (0..self.tabs.len()).map(|i| self.drawn_rect(i)).collect()
    }
    /// Where each closed tab fades, and its title, for a test.
    #[doc(hidden)]
    pub fn ghost_places(&self) -> Vec<(Rect, String)> {
        let ghosts = &self.motion.ghosts;
        ghosts.iter().map(|g| (g.rect, g.title.clone())).collect()
    }
    /// What the pointer lights now, for a test.
    #[doc(hidden)]
    pub fn hover_hit(&self) -> CaptionHit {
        self.hover
    }
    /// True if the whole of this tab shows between the arrows.
    #[doc(hidden)]
    pub fn tab_shows_whole(&self, index: usize) -> bool {
        self.tab_rects
            .get(index)
            .is_some_and(|b| b.x.0 >= self.clip_start() && b.x.0 + b.width.0 <= self.clip_end())
    }
    /// How wide a tab's X box is, for a test.
    #[doc(hidden)]
    pub fn close_box_width(&self) -> f32 {
        light_tokens().close_width
    }
    /// The owner answered a close by mouse with no close, or other input came first. A later
    /// close of that tab is not the mouse's.
    pub fn forget_pointer_close(&mut self) {
        self.pointer_close = None;
    }
    /// True while a close by mouse holds the tab widths.
    pub fn width_is_held(&self) -> bool {
        self.held_width.is_some()
    }
    /// Sets `motion_off`. Turning it on shows every end state at once. True if a motion ended.
    pub fn set_motion_off(&mut self, off: bool) -> bool {
        self.motion.off = off;
        off && self.finish_motion()
    }
    pub fn motion_off(&self) -> bool {
        self.motion.off
    }
    /// The clock that motion reads. A test sets the time with `Clock::manual`.
    pub fn set_motion_clock(&mut self, clock: Clock) {
        self.motion.clock = clock;
    }
    /// How long the tab list must rest before its next change animates. A bench that needs
    /// constant motion sets 0.
    #[doc(hidden)]
    pub fn set_motion_quiet_ms(&mut self, ms: f64) {
        self.motion.quiet_ms = ms;
    }
    /// True while anything in the row moves. This costs the same with any number of tabs.
    pub fn is_moving(&self) -> bool {
        let m = &self.motion;
        !m.slides.is_empty()
            || m.plus.is_some()
            || !m.ghosts.is_empty()
            || m.fade_in.is_some()
            || m.buttons.iter().any(|b| b.moving)
    }
    /// Shows every end state at once. True if anything was moving.
    pub fn finish_motion(&mut self) -> bool {
        let moving = self.is_moving();
        let m = &mut self.motion;
        m.slides.clear();
        m.plus = None;
        m.ghosts.clear();
        m.fade_in = None;
        m.drawn = None;
        for button in &mut m.buttons {
            button.moving = false;
        }
        moving
    }
    /// One animation frame at this time: ends what is done and says what to repaint. The
    /// frame that shows the end state repaints too, and then nothing moves.
    pub fn step_motion(&mut self, now: f64) -> Vec<Rect> {
        self.motion.now = now;
        let mut out = Vec::with_capacity(4);
        if let Some(area) = self.moving_area() {
            out.push(union_rect(self.motion.drawn, area));
        } else if let Some(drawn) = self.motion.drawn {
            out.push(drawn);
        }
        for (slot, hit) in CAPTION_HITS.into_iter().enumerate() {
            if self.motion.buttons[slot].moving {
                out.push(self.part_rect(hit));
            }
        }
        let m = &mut self.motion;
        m.slides.retain(|(_, slide)| !slide.done(now));
        if m.plus.is_some_and(|slide| slide.done(now)) {
            m.plus = None;
        }
        m.ghosts.retain(|ghost| now - ghost.start < CLOSE_FADE_MS);
        if m.fade_in
            .is_some_and(|(_, start)| now - start >= OPEN_FADE_MS)
        {
            m.fade_in = None;
        }
        for button in &mut m.buttons {
            if button.moving && button.done(now) {
                button.moving = false;
            }
        }
        self.motion.drawn = self.moving_area();
        out
    }
    /// The smallest rectangle around all that slides or fades in the tab list.
    fn moving_area(&self) -> Option<Rect> {
        let m = &self.motion;
        let now = m.now;
        let mut area = None;
        for (id, slide) in &m.slides {
            if let Some(i) = self.index_of(*id) {
                area = Some(union_rect(area, slide.place(self.tab_rects[i], now)));
            }
        }
        if let Some(slide) = m.plus {
            area = Some(union_rect(area, slide.place(self.plus, now)));
        }
        for ghost in &m.ghosts {
            area = Some(union_rect(area, ghost.rect));
        }
        if let Some((id, _)) = m.fade_in
            && let Some(i) = self.index_of(id)
        {
            area = Some(union_rect(area, self.tab_rects[i]));
        }
        area
    }
    /// Where a tab is drawn now.
    fn drawn_rect(&self, index: usize) -> Rect {
        let end = self.tab_rects[index];
        if self.motion.slides.is_empty() {
            return end;
        }
        let id = self.tabs[index].id;
        match self.motion.slides.iter().find(|(known, _)| *known == id) {
            Some((_, slide)) => slide.place(end, self.motion.now),
            None => end,
        }
    }
    /// Where the tabs stop being drawn. Tabs on their way in from the right show until the
    /// + arrives, and a new tab shows whole at once.
    fn drawn_clip_end(&self) -> f32 {
        let end = self.clip_end();
        match self.motion.plus {
            Some(_) if !self.overflow => end.max(self.drawn_plus().x.0),
            _ => end,
        }
    }
    fn drawn_plus(&self) -> Rect {
        match self.motion.plus {
            Some(slide) => slide.place(self.plus, self.motion.now),
            None => self.plus,
        }
    }
    /// The tabs in or near the row's view now, by index. The cost follows the view, not the
    /// number of tabs.
    fn shown_range(&self) -> std::ops::Range<usize> {
        let first = self.scroll.min(self.tabs.len());
        let count = if self.tab_width > 0.0 {
            ((self.clip_end() - self.clip_start()) / self.tab_width)
                .ceil()
                .max(0.0) as usize
                + 2
        } else {
            self.tabs.len()
        };
        first.saturating_sub(1)..(first + count).min(self.tabs.len())
    }
    fn drawn_before(&self) -> Option<DrawnBefore> {
        if self.motion.off {
            return None;
        }
        let (start, end) = (self.clip_start(), self.clip_end());
        let mut tabs = Vec::new();
        for i in self.shown_range() {
            let b = self.drawn_rect(i);
            if b.x.0 < end && b.x.0 + b.width.0 > start {
                tabs.push((self.tabs[i].id, b.x.0, b.width.0));
            }
        }
        let plus = self.drawn_plus();
        Some(DrawnBefore {
            tabs,
            plus: (plus.x.0, plus.width.0),
        })
    }
    fn ghost_of(&self, index: usize) -> Ghost {
        let tab = &self.tabs[index];
        Ghost {
            rect: self.drawn_rect(index),
            title: tab.title.clone(),
            dirty: tab.dirty,
            active: index == self.active,
            start: 0.0,
        }
    }
    /// Every slide starts from where its tab is drawn now and ends at its final place, so
    /// that a second change during a slide makes no jump.
    fn start_slides(&mut self, before: &DrawnBefore, except: Option<TabId>, now: f64) {
        self.motion.slides.clear();
        let mut after_last = None;
        let (start, end) = (self.clip_start(), self.clip_end());
        for i in self.shown_range() {
            let b = self.tab_rects[i];
            let id = self.tabs[i].id;
            if Some(id) == except {
                after_last = Some(b.x.0 + b.width.0);
                continue;
            }
            let known = before.tabs.iter().find(|(tab, ..)| *tab == id);
            // A tab that was out of view comes in behind its neighbour.
            let (from_x, from_width) = match (known, after_last) {
                (Some((_, x, width)), _) => (*x, *width),
                (None, Some(x)) => (x, b.width.0),
                (None, None) => (b.x.0, b.width.0),
            };
            after_last = Some(from_x + from_width);
            let moved = from_x != b.x.0 || from_width != b.width.0;
            let reaches_view = from_x.min(b.x.0) < end && from_x.max(b.x.0) + b.width.0 > start;
            if moved && reaches_view {
                self.motion.slides.push((
                    id,
                    Slide {
                        from_x,
                        from_width,
                        start: now,
                    },
                ));
            }
        }
        self.motion.plus = (before.plus != (self.plus.x.0, self.plus.width.0)).then_some(Slide {
            from_x: before.plus.0,
            from_width: before.plus.1,
            start: now,
        });
        self.motion.now = now;
    }
    /// A tab came or went. It animates if the list has rested, as WinUI's does. A change
    /// during motion goes on from what is drawn. Any other quick change just jumps.
    fn after_list_change(
        &mut self,
        before: Option<DrawnBefore>,
        ghost: Option<Ghost>,
        new_tab: Option<TabId>,
    ) {
        let Some(before) = before else {
            return;
        };
        let now = self.motion.clock.now_ms();
        let quiet = self
            .motion
            .last_list_change
            .is_none_or(|last| now - last >= self.motion.quiet_ms);
        let in_flight = !self.motion.slides.is_empty()
            || self.motion.plus.is_some()
            || !self.motion.ghosts.is_empty()
            || self.motion.fade_in.is_some();
        self.motion.last_list_change = Some(now);
        if !quiet && !in_flight {
            return;
        }
        self.start_slides(&before, new_tab, now);
        if let Some(mut ghost) = ghost {
            ghost.start = now;
            if self.motion.ghosts.len() >= MAX_GHOSTS {
                self.motion.ghosts.remove(0);
            }
            self.motion.ghosts.push(ghost);
        }
        if let Some(id) = new_tab {
            self.motion.fade_in = Some((id, now));
        }
        self.motion.drawn = self.moving_area();
    }
    /// Starts a window button's fade if what the pointer does to it changed. The key path
    /// never comes here.
    fn sync_button_fades(&mut self) {
        for (slot, hit) in CAPTION_HITS.into_iter().enumerate() {
            let look = self.look(hit);
            let want = look != Look::Rest;
            let button = self.motion.buttons[slot];
            if self.motion.off || look == Look::Pressed {
                self.motion.buttons[slot] = ButtonFade {
                    lit: want,
                    ..Default::default()
                };
            } else if want != button.lit {
                let now = self.motion.clock.now_ms();
                self.motion.now = now;
                self.motion.buttons[slot] = ButtonFade {
                    lit: want,
                    moving: true,
                    from: button.value(now),
                    start: now,
                };
            }
        }
    }
    /// What a click at this place means. Hit tests are final from the first moment, but a
    /// person clicks what is drawn. So during a slide, a tab or the + drawn here takes the
    /// click, even if another one's final place is here. While a close by mouse holds the
    /// widths, the final answer wins on a tab or a gap, so that a repeated click reaches the
    /// next X. With no hold, the X must be seen to be hit: a tab's drawn box says which part
    /// was hit, and a gap never closes a tab. The window buttons never move.
    pub fn drawn_hit_test(&self, at: Point) -> CaptionHit {
        let hit = self.hit_test(at);
        let m = &self.motion;
        if m.slides.is_empty() && m.plus.is_none() {
            return hit;
        }
        if !matches!(
            hit,
            CaptionHit::Tab(_) | CaptionHit::TabClose(_) | CaptionHit::NewTab | CaptionHit::Drag
        ) {
            return hit;
        }
        if inside(at, self.drawn_plus()) {
            return CaptionHit::NewTab;
        }
        let held = self.held_width.is_some();
        // No X is drawn in a gap, so that a press there can at most select.
        let gap = match hit {
            CaptionHit::TabClose(i) if !held => CaptionHit::Tab(i),
            other => other,
        };
        if at.x.0 < self.clip_start() || at.x.0 >= self.drawn_clip_end() {
            return gap;
        }
        let t = light_tokens();
        for i in self.shown_range() {
            let b = self.drawn_rect(i);
            if inside(at, b) {
                if let CaptionHit::Tab(same) | CaptionHit::TabClose(same) = hit
                    && same == i
                    && held
                {
                    return hit;
                }
                return if at.x.0 >= b.x.0 + b.width.0 - t.close_width {
                    CaptionHit::TabClose(i)
                } else {
                    CaptionHit::Tab(i)
                };
            }
        }
        gap
    }
    /// What a release at this place means. One near a press that took a drawn place belongs
    /// to that press, wherever the tabs have got to by now.
    fn release_hit(&mut self, at: Point) -> CaptionHit {
        match self.drawn_press.take() {
            Some((from, target))
                if (at.x.0 - from.x.0).abs() <= PRESS_SLOP
                    && (at.y.0 - from.y.0).abs() <= PRESS_SLOP =>
            {
                target
            }
            _ => self.drawn_hit_test(at),
        }
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
        if inside(at, self.plus) {
            return CaptionHit::NewTab;
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
        self.end_hold_if_pointer_is_off(ctx);
        self.sync_button_fades();
    }
    /// A press kept the hold while it had the mouse. Now that it has ended, a pointer outside
    /// the row has no other notice to end the hold with.
    fn end_hold_if_pointer_is_off(&mut self, ctx: &mut ViewContext) {
        if !self.capturing && !self.pointer_in_row {
            self.release_hold(ctx);
        }
    }
    /// Where the pointer is after a time when the row heard no moves.
    pub fn pointer_checked(&mut self, in_hold_zone: bool, ctx: &mut ViewContext) {
        self.pointer_in_row = in_hold_zone;
        self.end_hold_if_pointer_is_off(ctx);
    }
    /// The pointer left the window, or this row. A button held by capture keeps its press.
    pub fn pointer_left(&mut self, ctx: &mut ViewContext) {
        self.pointer_moved_off(ctx);
        if !self.capturing {
            self.pointer_in_row = false;
            self.release_hold(ctx);
        }
    }
    /// The pointer left a tab for the empty part of the row. Windows counts that as leaving
    /// the client area, but the tab widths stay held.
    pub fn pointer_moved_off(&mut self, ctx: &mut ViewContext) {
        if self.hover != CaptionHit::None && !self.capturing {
            ctx.invalidate(self.part_rect(self.hover));
            self.hover = CaptionHit::None;
        }
        self.sync_button_fades();
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
        self.drawn_press = None;
        self.hover = CaptionHit::None;
        self.pressed = CaptionHit::None;
        self.drag = None;
        self.middle_press = None;
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
        self.plus.x.0
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
            CaptionHit::NewTab => self.plus,
            CaptionHit::ScrollLeft if self.overflow => r(
                self.bounds.x.0,
                self.plus.y.0,
                t.scroll_width,
                t.plus_height,
            ),
            CaptionHit::ScrollRight if self.overflow => r(
                self.plus.x.0 - t.scroll_width,
                self.plus.y.0,
                t.scroll_width,
                t.plus_height,
            ),
            CaptionHit::Minimize => r(self.caption_x(0, t), self.bounds.y.0, t.caption_width, row),
            CaptionHit::Maximize => r(self.caption_x(1, t), self.bounds.y.0, t.caption_width, row),
            CaptionHit::Close => r(self.caption_x(2, t), self.bounds.y.0, t.caption_width, row),
            _ => Rect::default(),
        }
    }
    /// The hover box of the X of a tab drawn at `b`.
    fn close_box_in(b: Rect, t: ShellTokens) -> Rect {
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
        let available = (bounds.width.0 - 3.0 * t.caption_width - t.plus_width).max(0.0);
        self.tab_width = self.held_width.unwrap_or_else(|| {
            (available / self.tabs.len() as f32).clamp(t.min_width, t.max_width)
        });
        self.overflow = self
            .held_row
            .unwrap_or(self.tabs.len() as f32 * t.min_width > available);
        let left = bounds.x.0 + if self.overflow { t.scroll_width } else { 0.0 };
        self.visible_width = if self.overflow {
            (available - 2.0 * t.scroll_width).max(t.min_width)
        } else {
            available
        };
        if self.held_row.is_none() {
            self.scroll = self.scroll.min(self.max_scroll());
        }
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
        self.plus = r(
            if self.overflow {
                left + self.visible_width + t.scroll_width
            } else {
                left + self.tabs.len() as f32 * self.tab_width
            },
            tab_top + (t.tab_height - t.plus_height) / 2.0,
            t.plus_width,
            t.plus_height,
        );
        self.fix_focus();
    }
    /// Scrolls so that this whole tab shows.
    fn reveal(&mut self, index: usize) {
        // A held row stays while what is asked for shows. Otherwise, showing it comes first.
        if self.held_row.is_some() {
            if self.tab_shows_whole(index) {
                return;
            }
            self.clear_hold();
        }
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
        // The person takes over: a kept row ends first, so that the scroll limit is true again.
        if self.held_row.is_some() {
            self.clear_hold();
            self.arrange();
        }
        let next = self
            .scroll
            .saturating_add_signed(delta)
            .min(self.max_scroll());
        if next != self.scroll {
            self.scroll = next;
            // Scrolling has no motion, and a slide would cross it.
            self.motion.slides.clear();
            self.motion.plus = None;
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
        let mut out = Vec::with_capacity(self.tabs.len() * 2 + 3);
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
        self.pointer = Some(at);
        self.pointer_in_row = self.in_hold_zone(at);
        if !self.pointer_in_row && !self.capturing {
            self.release_hold(ctx);
        }
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
                let before = self.drawn_before();
                let dragged = self.tab_id(drag.index);
                self.reorder_index(drag.index, to);
                if let Some(before) = before {
                    // The other tabs slide. The one in hand goes where the hand is.
                    let now = self.motion.clock.now_ms();
                    self.start_slides(&before, dragged, now);
                    self.motion.drawn = self.moving_area();
                }
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
    fn mouse_down(&mut self, at: Point, button: MouseButton, ctx: &mut ViewContext) -> Handled {
        self.pointer_close = None;
        // A left press holds the mouse. Its drag goes on as if this press never happened.
        if button == MouseButton::Middle && self.capturing {
            return Handled::Yes;
        }
        let left = button == MouseButton::Left;
        self.pointer = Some(at);
        self.pointer_in_row = self.in_hold_zone(at);
        let hit = self.drawn_hit_test(at);
        self.drawn_press = (hit != self.hit_test(at)).then_some((at, hit));
        // Any other press ends a middle press. A left press on a target below keeps the
        // capture: a release here would end that new press too, through `capture_lost`.
        if self.middle_press.take().is_some()
            && (!left
                || matches!(
                    hit,
                    CaptionHit::None | CaptionHit::Drag | CaptionHit::ResizeTop
                ))
        {
            self.capturing = false;
            ctx.capture_mouse(false);
        }
        if hit == CaptionHit::None {
            // A click in the text area takes keyboard focus away from the strip.
            if self.focused.take().is_some() {
                self.events.push(ShellEvent::FocusContent);
                ctx.invalidate(self.row_rect());
            }
            return Handled::No;
        }
        if button == MouseButton::Middle
            && let CaptionHit::Tab(index) | CaptionHit::TabClose(index) = hit
        {
            // Where nothing is drawn, a middle press means nothing. A hold is the exception:
            // the person is closing tab after tab at this spot.
            if self.held_width.is_none() && !inside(at, self.drawn_rect(index)) {
                self.drawn_press = None;
                return Handled::Yes;
            }
            self.middle_press = self.tab_id(index);
            self.capturing = true;
            ctx.capture_mouse(true);
            return Handled::Yes;
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
    /// The tab under the pointer closes the way its X does, if the press began on it.
    fn middle_up(&mut self, at: Point, ctx: &mut ViewContext) -> Handled {
        self.pointer = Some(at);
        self.pointer_in_row = self.in_hold_zone(at);
        let hit = self.release_hit(at);
        let Some(pressed) = self.middle_press.take() else {
            // During a left press, this release is no one else's either.
            return if self.capturing || inside(at, self.row_rect()) {
                Handled::Yes
            } else {
                Handled::No
            };
        };
        self.capturing = false;
        ctx.capture_mouse(false);
        self.end_hold_if_pointer_is_off(ctx);
        self.set_hover(hit, ctx);
        if let CaptionHit::Tab(index) | CaptionHit::TabClose(index) = hit
            && self.tab_id(index) == Some(pressed)
        {
            self.activate(CaptionHit::TabClose(index));
            self.pointer_close = Some(pressed);
        }
        Handled::Yes
    }
    fn mouse_up(&mut self, at: Point, ctx: &mut ViewContext) -> Handled {
        let pressed = self.pressed;
        self.pointer = Some(at);
        self.pointer_in_row = self.in_hold_zone(at);
        let hit = self.release_hit(at);
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
        self.end_hold_if_pointer_is_off(ctx);
        ctx.invalidate(self.part_rect(hit));
        if hit == pressed && !moved {
            self.activate(pressed);
            if let CaptionHit::TabClose(index) = pressed {
                self.pointer_close = self.tab_id(index);
            }
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
        if new_width {
            // A hold could run past the new edge. A resize shows its end state at once.
            self.clear_hold();
            self.finish_motion();
        }
        self.arrange();
        if self.reveal_on_layout || new_width {
            self.reveal_on_layout = false;
            self.reveal(self.active);
        }
    }
    fn handle(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        let handled = self.handle_event(event, ctx);
        // Keys read no clock and start no motion here.
        if !matches!(
            event.kind,
            InputKind::KeyDown { .. } | InputKind::KeyUp { .. } | InputKind::Char(_)
        ) {
            self.sync_button_fades();
        }
        handled
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
        self.draw_button(list, CaptionHit::NewTab, 0xe710, t, measure);
        self.draw_caption(list, t, measure);
    }
    fn widgets(&self) -> Option<Widget> {
        let t = light_tokens();
        let button = |hit: CaptionHit, name: &str, bounds: Rect| Widget {
            char_boxes: Vec::new(),
            access_key: None,
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
            selection: None,
            range_value: None,
            description: None,
            children: Vec::new(),
        };
        let mut children = Vec::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            let b = self.tab_rects[i];
            children.push(Widget {
                char_boxes: Vec::new(),
                access_key: None,
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
                selection: None,
                range_value: None,
                description: None,
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
            char_boxes: Vec::new(),
            access_key: None,
            id: WidgetId(0),
            bounds: self.row_rect(),
            role: WidgetRole::TabList,
            name: "Tabs".into(),
            value: None,
            states: WidgetStates {
                enabled: true,
                ..Default::default()
            },
            selection: None,
            range_value: None,
            description: None,
            children,
        })
    }
}
impl TabStrip {
    fn handle_event(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        match event.kind {
            InputKind::KeyDown {
                virtual_key,
                modifiers,
                repeat,
            } => {
                // A key now means a later close of that tab was not the mouse's.
                self.pointer_close = None;
                // Chords belong to the system and the command table.
                if modifiers.alt || modifiers.ctrl || modifiers.win {
                    return Handled::No;
                }
                self.focused_key(virtual_key, modifiers, repeat, ctx)
            }
            InputKind::MouseMove { at } => self.mouse_move(at, ctx),
            InputKind::MouseDown { at, button, .. } => self.mouse_down(at, button, ctx),
            InputKind::MouseUp {
                at,
                button: MouseButton::Left,
                ..
            } => self.mouse_up(at, ctx),
            InputKind::MouseUp {
                at,
                button: MouseButton::Middle,
                ..
            } => self.middle_up(at, ctx),
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
                self.pointer_close = None;
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
    /// Ids in separate ranges, so that no count of tabs makes two widgets share one.
    pub fn widget_id(hit: CaptionHit) -> WidgetId {
        WidgetId(match hit {
            CaptionHit::Tab(i) => TAB_IDS + i as u64,
            CaptionHit::TabClose(i) => TAB_CLOSE_IDS + i as u64,
            CaptionHit::NewTab => 1,
            CaptionHit::Minimize => 3,
            CaptionHit::Maximize => 4,
            CaptionHit::Close => 5,
            CaptionHit::ScrollLeft => 6,
            CaptionHit::ScrollRight => 7,
            CaptionHit::None | CaptionHit::Drag | CaptionHit::ResizeTop => 0,
        })
    }
    /// A closed tab on its way out, under the tabs that slide over it.
    fn draw_ghost(
        &self,
        ghost: &Ghost,
        list: &mut DisplayList,
        t: ShellTokens,
        measure: &dyn TextMeasurer,
    ) {
        let left = 1.0 - part_of(self.motion.now, ghost.start, CLOSE_FADE_MS) as f32;
        let b = ghost.rect;
        list.push(DrawCommand::PushOpacity(left));
        if ghost.active {
            list.push(DrawCommand::RoundedRect {
                rect: r(b.x.0, b.y.0, b.width.0, b.height.0 + t.radius),
                radius: Dip(t.radius),
                color: t.active,
            });
        }
        let text = ellipsis(
            &ghost.title,
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
            color: if ghost.active && self.window_active {
                t.text
            } else {
                t.dim
            },
        });
        let close = Self::close_box_in(b, t);
        let ink = if self.window_active {
            t.button_text
        } else {
            t.dim
        };
        if ghost.dirty {
            let metrics = measure.measure("•", t.text_token);
            list.push(DrawCommand::TextRun {
                at: centered(close, metrics),
                text: "•".into(),
                font: t.text_font.into(),
                font_size: Dip(t.text_font_size),
                color: ink,
            });
        } else {
            icon(
                list,
                close,
                0xe8bb,
                IconStyle {
                    size: t.caption_icon,
                    token: t.caption_icon_token,
                    color: ink,
                    scale: self.scale.get(),
                },
                measure,
            );
        }
        list.push(DrawCommand::PopOpacity);
    }
    /// How much of a new tab's fill shows: its text and its X show at once.
    fn fill_color(&self, id: TabId, color: Color) -> Color {
        match self.motion.fade_in {
            Some((new, start)) if new == id => {
                let part = part_of(self.motion.now, start, OPEN_FADE_MS) as f32;
                Color {
                    a: (f32::from(color.a) * part).round() as u8,
                    ..color
                }
            }
            _ => color,
        }
    }
    fn draw_tabs(&self, list: &mut DisplayList, t: ShellTokens, measure: &dyn TextMeasurer) {
        let (start, end) = (self.clip_start(), self.drawn_clip_end());
        let row = self.row_rect();
        list.push(DrawCommand::PushClip(r(
            start,
            row.y.0,
            (end - start).max(0.0),
            row.height.0,
        )));
        let contrast = self.contrast.is_some();
        for ghost in &self.motion.ghosts {
            self.draw_ghost(ghost, list, t, measure);
        }
        let sliding = !self.motion.slides.is_empty();
        let range = if sliding {
            self.shown_range()
        } else {
            self.scroll.min(self.tabs.len())..self.tabs.len()
        };
        for i in range {
            let tab = &self.tabs[i];
            let b = self.drawn_rect(i);
            if !sliding && b.x.0 >= end {
                break;
            }
            if b.x.0 >= end || b.x.0 + b.width.0 <= start {
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
            // the top corners are round, and an active tab joins the menu bar.
            let shape = r(b.x.0, b.y.0, b.width.0, b.height.0 + t.radius);
            if active {
                list.push(DrawCommand::RoundedRect {
                    rect: shape,
                    radius: Dip(t.radius),
                    color: self.fill_color(tab.id, t.active),
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
            let close = Self::close_box_in(b, t);
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
            // A new tab's X waits for the + to pass, or the two would cross on screen.
            let plus_on_x = self.motion.fade_in.is_some_and(|(new, _)| new == tab.id)
                && self.drawn_plus().x.0 < close.x.0 + close.width.0;
            if !plus_on_x && tab.dirty && !hovering {
                let metrics = measure.measure("•", t.text_token);
                list.push(DrawCommand::TextRun {
                    at: centered(close, metrics),
                    text: "•".into(),
                    font: t.text_font.into(),
                    font_size: Dip(t.text_font_size),
                    color: button_ink,
                });
            } else if !plus_on_x {
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
        let b = if hit == CaptionHit::NewTab {
            self.drawn_plus()
        } else {
            self.part_rect(hit)
        };
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
    fn draw_caption(&self, list: &mut DisplayList, t: ShellTokens, measure: &dyn TextMeasurer) {
        let restore = self.mode != FrameMode::Restored;
        let contrast = self.contrast.is_some();
        // `MinMaxCloseControl.xaml`: high contrast has its own heavier glyphs.
        let (minimize, maximize, restore_glyph, close) = if contrast {
            (0xef2d, 0xef2e, 0xef2f, 0xef2c)
        } else {
            (0xe921, 0xe922, 0xe923, 0xe8bb)
        };
        for (slot, (hit, code)) in [
            (CaptionHit::Minimize, minimize),
            (
                CaptionHit::Maximize,
                if restore { restore_glyph } else { maximize },
            ),
            (CaptionHit::Close, close),
        ]
        .into_iter()
        .enumerate()
        {
            let b = self.part_rect(hit);
            let look = self.look(hit);
            let is_close = hit == CaptionHit::Close;
            // A fill on its way in or out has the hover look at part of its strength. The
            // icon's color goes with it, or white would sit on a fill too weak to show it.
            let button = self.motion.buttons[slot];
            let part = if button.moving {
                button.value(self.motion.now)
            } else if look == Look::Rest {
                0.0
            } else {
                1.0
            };
            let hover_fill = if is_close { t.close_hover } else { t.hover };
            let fill = match (look, is_close) {
                (Look::Pressed, true) => Some(t.close_pressed),
                (Look::Pressed, false) => Some(t.pressed),
                (Look::Hover, _) if part >= 1.0 => Some(hover_fill),
                _ if part > 0.0 => Some(Color {
                    a: (f32::from(hover_fill.a) * part).round() as u8,
                    ..hover_fill
                }),
                _ => None,
            };
            if let Some(color) = fill {
                list.push(DrawCommand::FillRect { rect: b, color });
            }
            let rest_ink = if self.window_active {
                t.text
            } else {
                t.caption_dim
            };
            let color = match look {
                Look::Pressed if is_close => t.close_text_pressed,
                Look::Hover | Look::Pressed if contrast => t.close_text,
                Look::Hover if is_close && part >= 1.0 => t.close_text,
                _ if is_close && part > 0.0 => mix(rest_ink, t.close_text, part),
                _ => rest_ink,
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
