//! What the Settings window paints: three tabs of controls and the footer. It needs no
//! window, so every rule here runs in tests with real event order.
use crate::format::integer_range;
use crate::store::{SettingChange, SettingsStore};
use crate::table::{HEAVY_STRIKE, ROW_HEIGHT, ShortcutTable, TEXT_TOKEN};
use qe_api::{
    Dip, DisplayList, DrawCommand, EncodingChoice, Handled, InputEvent, InputKind, Point, Rect,
    SettingKey, TextMeasurer, TextMetrics, Theme, View, ViewContext, Widget, WidgetId, WidgetRole,
    WidgetStates,
};
use qe_ui::{
    Button, Checkbox, ComboBox, Control, DialogFrame, Label, LabelKind, TabBar, TextField, UI_FONT,
    UiTokens, wrap_lines,
};
use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;

pub const SETTINGS_WIDTH: f32 = 440.0;
/// New Document and Open and Save share one height, as on the Mac.
pub const TEXT_TABS_HEIGHT: f32 = 440.0;
/// The tabs, the Mac's 464-high Keyboard panel, and the footer.
pub const KEYBOARD_HEIGHT: f32 = 556.0;
/// The footer that keeps Restore All Defaults whole.
pub const FOOTER_HEIGHT: f32 = 44.0;
const TABS_BOTTOM: f32 = 40.0;
const KEYBOARD_WIDTH: f32 = 394.0;
const KEYBOARD_X: f32 = (SETTINGS_WIDTH - KEYBOARD_WIDTH) / 2.0;
/// The slot beside Restore All Defaults. Text that fits here keeps the footer small.
const STATUS_SLOT_WIDTH: f32 = 246.0;
const STATUS_SLOT_HEIGHT: f32 = 38.0;
/// Longer text gets its own lines under the button, the window's width.
const STATUS_WIDE: f32 = 412.0;
const STATUS_WIDE_PADDING: f32 = 6.0;
const FONT_NAME_WIDTH: f32 = 214.0;
const FONT_NAME_HEIGHT: f32 = 28.0;
/// Two lines, which end above the Options heading.
const FONT_NAME_WRAPPED_HEIGHT: f32 = 34.0;
const FONT_NAME_Y: f32 = 216.0;

const ENTER: u16 = 0x0d;
pub const TAB_NEW_DOCUMENT: usize = 0;
pub const TAB_OPEN_AND_SAVE: usize = 1;
pub const TAB_KEYBOARD: usize = 2;
pub const CUSTOMIZE_ENCODINGS: &str = "Customize Encodings List\u{2026}";
pub const WINDOWS_KEYS_NOTE: &str = "Windows keeps Alt+F4, Alt+Tab, F10 and the Windows key.";

/// Work the panel cannot do itself, for the window to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelRequest {
    /// The Change button: show the Windows font dialog.
    ChooseFont,
    /// The last row of an encoding list: show the Customize Encodings panel.
    CustomizeEncodings,
}

/// A control that shows on one tab, or on all of them with `page: None`.
struct Paged {
    page: Option<usize>,
    current: Rc<Cell<usize>>,
    inner: Box<dyn Control>,
    /// The Mac's tooltip, for a screen reader.
    help: Option<&'static str>,
}
impl Paged {
    fn shown(&self) -> bool {
        self.page.is_none_or(|page| page == self.current.get())
    }
}
impl View for Paged {
    fn handle(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        // A hidden control still hears that it lost the keyboard.
        if self.shown() || matches!(event.kind, InputKind::Blur | InputKind::Focus) {
            self.inner.handle(event, ctx)
        } else {
            Handled::No
        }
    }
    fn draw(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        if self.shown() {
            self.inner.draw(list, theme, measure);
        }
    }
    fn layout(&mut self, bounds: Rect, measure: &dyn TextMeasurer) {
        self.inner.layout(bounds, measure);
    }
    fn widgets(&self) -> Option<Widget> {
        if !self.shown() {
            return None;
        }
        let mut node = self.inner.widgets()?;
        // A message line with nothing to say is not there for a screen reader.
        if node.role == WidgetRole::Label && node.name.is_empty() {
            return None;
        }
        if let Some(help) = self.help {
            node.description = Some(help.to_owned());
        }
        Some(node)
    }
}
impl Control for Paged {
    fn focusable(&self) -> bool {
        self.shown() && self.inner.focusable()
    }
    fn uses_key(&self, virtual_key: u16) -> bool {
        self.shown() && self.inner.uses_key(virtual_key)
    }
    fn take_activated(&mut self) -> bool {
        self.inner.take_activated()
    }
    fn cancel_press(&mut self, ctx: &mut ViewContext) {
        self.inner.cancel_press(ctx);
    }
    fn mark_default(&mut self) -> bool {
        self.inner.mark_default()
    }
    fn switch_tab(&mut self, back: bool, ctx: &mut ViewContext) -> bool {
        self.shown() && self.inner.switch_tab(back, ctx)
    }
    fn overlay_bounds(&self) -> Option<Rect> {
        if self.shown() {
            self.inner.overlay_bounds()
        } else {
            None
        }
    }
    fn set_popup_area(&mut self, area: Rect) {
        self.inner.set_popup_area(area);
    }
    fn draw_base(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        if self.shown() {
            self.inner.draw_base(list, theme, measure);
        }
    }
    fn draw_overlay(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        if self.shown() {
            self.inner.draw_overlay(list, theme, measure);
        }
    }
    fn hit_bounds(&self) -> Option<Rect> {
        if self.shown() {
            self.inner.hit_bounds()
        } else {
            None
        }
    }
}

/// A section title in small semibold type, as on the Mac and in the mockup.
struct Header {
    id: WidgetId,
    text: String,
    bounds: Rect,
}
impl View for Header {
    fn handle(&mut self, _: &InputEvent, _: &mut ViewContext) -> Handled {
        Handled::No
    }
    fn draw(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        let t = UiTokens::from_theme(theme);
        let metrics = measure.measure(&self.text, TEXT_TOKEN);
        let y = Dip(self.bounds.y.0
            + (self.bounds.height.0 - metrics.height.0) / 2.0
            + metrics.baseline.0);
        for shift in [0.0, HEAVY_STRIKE] {
            list.push(DrawCommand::TextRun {
                at: Point {
                    x: Dip(self.bounds.x.0 + shift),
                    y,
                },
                text: self.text.clone(),
                font: UI_FONT.into(),
                font_size: Dip(12.0),
                color: t.text,
            });
        }
    }
    fn layout(&mut self, bounds: Rect, _: &dyn TextMeasurer) {
        self.bounds = bounds;
    }
    fn widgets(&self) -> Option<Widget> {
        Some(Widget {
            char_boxes: Vec::new(),
            access_key: None,
            id: self.id,
            bounds: self.bounds,
            role: WidgetRole::Label,
            name: self.text.clone(),
            value: None,
            states: WidgetStates {
                enabled: true,
                section: true,
                ..Default::default()
            },
            selection: None,
            range_value: None,
            description: None,
            children: vec![],
        })
    }
}
impl Control for Header {
    fn focusable(&self) -> bool {
        false
    }
}

/// A thin line across the window. It comes first in the list, so an open list covers it.
struct Rule {
    y: Rc<Cell<f32>>,
}
impl View for Rule {
    fn handle(&mut self, _: &InputEvent, _: &mut ViewContext) -> Handled {
        Handled::No
    }
    fn draw(&self, list: &mut DisplayList, theme: &Theme, _: &dyn TextMeasurer) {
        let y = self.y.get() + 0.5;
        list.push(DrawCommand::Line {
            from: Point {
                x: Dip(0.0),
                y: Dip(y),
            },
            to: Point {
                x: Dip(SETTINGS_WIDTH),
                y: Dip(y),
            },
            width: Dip(1.0),
            color: theme.border,
        });
    }
    fn layout(&mut self, _: Rect, _: &dyn TextMeasurer) {}
    fn widgets(&self) -> Option<Widget> {
        None
    }
}
impl Control for Rule {
    fn focusable(&self) -> bool {
        false
    }
    fn hit_bounds(&self) -> Option<Rect> {
        None
    }
}

fn only_digits(text: &str) -> bool {
    text.bytes().all(|byte| byte.is_ascii_digit())
}
fn r(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x: Dip(x),
        y: Dip(y),
        width: Dip(width),
        height: Dip(height),
    }
}

const SPELLING_HELP: &str =
    "Choose Edit > Spelling to change this option for individual documents.";
const SUBSTITUTIONS_HELP: &str =
    "Choose Edit > Substitutions to change this option for individual documents.";

/// Adds controls in tab order, with the place of each.
struct Builder {
    frame: DialogFrame,
    current: Rc<Cell<usize>>,
    next_id: u64,
    places: Vec<(usize, Rect)>,
    checks: Vec<(usize, SettingKey)>,
}
impl Builder {
    fn id(&mut self) -> WidgetId {
        self.next_id += 1;
        WidgetId(self.next_id)
    }
    fn add_with_help(
        &mut self,
        page: Option<usize>,
        inner: Box<dyn Control>,
        at: Option<Rect>,
        help: Option<&'static str>,
    ) -> usize {
        self.frame.add(Box::new(Paged {
            page,
            current: self.current.clone(),
            inner,
            help,
        }));
        let index = self.frame.children.len() - 1;
        if let Some(at) = at {
            self.places.push((index, at));
        }
        index
    }
    fn add(&mut self, page: Option<usize>, inner: Box<dyn Control>, at: Option<Rect>) -> usize {
        self.add_with_help(page, inner, at, None)
    }
    fn header(&mut self, page: Option<usize>, text: &str, y: f32) {
        let header = Header {
            id: self.id(),
            text: text.into(),
            bounds: Rect::default(),
        };
        self.add(page, Box::new(header), Some(r(20.0, y, 400.0, 18.0)));
    }
    fn label(&mut self, page: Option<usize>, text: &str, kind: LabelKind, at: Rect) -> usize {
        let made = Label::new(self.id(), text, kind);
        self.add(page, Box::new(made), Some(at))
    }
    fn wrapped(
        &mut self,
        page: Option<usize>,
        text: &str,
        kind: LabelKind,
        at: Option<Rect>,
    ) -> usize {
        let mut made = Label::new(self.id(), text, kind);
        made.wrap = true;
        self.add(page, Box::new(made), at)
    }
    fn button(&mut self, page: Option<usize>, text: &str, at: Option<Rect>) -> usize {
        let made = Button::new(self.id(), text);
        self.add(page, Box::new(made), at)
    }
    fn field(&mut self, page: Option<usize>, name: &str, at: Rect) -> usize {
        let mut made = TextField::new(self.id(), name);
        made.validator = Some(only_digits);
        self.add(page, Box::new(made), Some(at))
    }
    fn combo(&mut self, page: Option<usize>, name: &str, at: Rect) -> usize {
        let made = ComboBox::new(self.id(), name, vec![]);
        self.add(page, Box::new(made), Some(at))
    }
    fn check(
        &mut self,
        page: Option<usize>,
        key: SettingKey,
        text: &str,
        help: &'static str,
        at: Rect,
    ) {
        let made = Checkbox::new(self.id(), text);
        let index = self.add_with_help(page, Box::new(made), Some(at), Some(help));
        self.checks.push((index, key));
    }
}

/// What each row of one encoding list means.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EncodingRows(Vec<Option<EncodingChoice>>);

/// The painted Settings content.
pub struct SettingsPanel {
    frame: DialogFrame,
    store: SettingsStore,
    current: Rc<Cell<usize>>,
    footer_y: Rc<Cell<f32>>,
    bounds: Rect,
    /// Controls with a fixed place, from the panel's top left corner.
    places: Vec<(usize, Rect)>,
    tabs: usize,
    checks: Vec<(usize, SettingKey)>,
    width: usize,
    height: usize,
    change_font: usize,
    font_name: usize,
    field_message: usize,
    open_combo: usize,
    save_combo: usize,
    open_rows: EncodingRows,
    save_rows: EncodingRows,
    table: usize,
    key_message: usize,
    windows_note: usize,
    restore_keys: usize,
    restore_all: usize,
    status: usize,
    /// Why a typed size was refused, and which field it was. It stays until that field
    /// is typed in again or the tab changes.
    size_error: Option<(usize, String)>,
    request: Option<PanelRequest>,
    announcement: Option<String>,
    /// A problem the window met, such as a font dialog that could not open. The next
    /// action on a control clears it.
    window_problem: Option<String>,
    /// A problem with painting or sizing the window now. The window sets and clears it.
    surface_problem: Option<String>,
    /// How the current text fits, from the last measure.
    fitted: Fitted,
}
/// What the measurer said about the text that changes the panel's shape.
#[derive(Default)]
struct Fitted {
    status: String,
    font: String,
    font_shown: String,
    font_wraps: bool,
    /// Height added under the footer's row. Zero when the text fits beside the button.
    status_lines_height: f32,
}
const MEASURE_THEME: Theme = Theme::new(qe_api::ThemeKind::Light);
fn lines_height(lines: &[String], measure: &dyn TextMeasurer, font: &str) -> f32 {
    lines
        .iter()
        .map(|line| measure.measure(line, font).height.0)
        .sum()
}
/// The font text in at most two lines. A longer name gives way, and the size stays.
fn fit_font_name(text: &str, measure: &dyn TextMeasurer, font: &str) -> String {
    let fits = |text: &str| wrap_lines(text, FONT_NAME_WIDTH, measure, font).len() <= 2;
    if fits(text) {
        return text.to_owned();
    }
    let (family, size) = text
        .rsplit_once(" pt")
        .and_then(|(head, _)| head.rsplit_once(' '))
        .unwrap_or((text, ""));
    let mut kept = family.to_owned();
    while kept.pop().is_some() {
        let candidate = format!("{}\u{2026} {size} pt", kept.trim_end());
        if fits(&candidate) {
            return candidate;
        }
    }
    format!("\u{2026} {size} pt")
}
fn inner<T: Control>(frame: &DialogFrame, index: usize) -> Option<&T> {
    let paged = frame.child::<Paged>(index)?;
    let any: &dyn Any = paged.inner.as_ref();
    any.downcast_ref()
}
fn inner_mut<T: Control>(frame: &mut DialogFrame, index: usize) -> Option<&mut T> {
    let paged = frame.child_mut::<Paged>(index)?;
    let any: &mut dyn Any = paged.inner.as_mut();
    any.downcast_mut()
}
impl SettingsPanel {
    /// `id` leaves the 256 ids above it to this panel.
    pub fn new(id: WidgetId, store: SettingsStore) -> Self {
        let current = Rc::new(Cell::new(TAB_NEW_DOCUMENT));
        let footer_y = Rc::new(Cell::new(TEXT_TABS_HEIGHT - FOOTER_HEIGHT));
        let mut b = Builder {
            frame: DialogFrame::new(id, "Settings"),
            current: current.clone(),
            next_id: id.0,
            places: Vec::new(),
            checks: Vec::new(),
        };
        let (doc, save, keys) = (
            Some(TAB_NEW_DOCUMENT),
            Some(TAB_OPEN_AND_SAVE),
            Some(TAB_KEYBOARD),
        );
        b.add(
            None,
            Box::new(Rule {
                y: Rc::new(Cell::new(TABS_BOTTOM)),
            }),
            None,
        );
        b.add(
            None,
            Box::new(Rule {
                y: footer_y.clone(),
            }),
            None,
        );
        let tab_names = vec![
            "New Document".to_owned(),
            "Open and Save".to_owned(),
            "Keyboard".to_owned(),
        ];
        let tab_id = b.id();
        let tabs = b.add(
            None,
            Box::new(TabBar::new(tab_id, "Settings tabs", tab_names)),
            Some(r(12.0, 6.0, 336.0, 30.0)),
        );
        // Tab order is child order, so each tab's controls go in reading order.
        // New Document: Format.
        b.header(doc, "Format", 48.0);
        b.label(
            doc,
            "Use the Format menu to choose settings for an open document.",
            LabelKind::Hint,
            r(20.0, 66.0, 400.0, 18.0),
        );
        b.check(
            doc,
            SettingKey::ShowPageBreaks,
            "Wrap to page",
            "Select to make text wrap to document margins rather than window boundaries.",
            r(20.0, 86.0, 400.0, 22.0),
        );
        // Window Size.
        b.header(doc, "Window Size", 114.0);
        b.label(doc, "Width:", LabelKind::Plain, r(20.0, 134.0, 50.0, 26.0));
        let width = b.field(
            doc,
            "Window width in characters",
            r(72.0, 134.0, 56.0, 26.0),
        );
        b.label(
            doc,
            "characters",
            LabelKind::Plain,
            r(134.0, 134.0, 70.0, 26.0),
        );
        b.label(doc, "Height:", LabelKind::Plain, r(20.0, 164.0, 50.0, 26.0));
        let height = b.field(doc, "Window height in lines", r(72.0, 164.0, 56.0, 26.0));
        b.label(doc, "lines", LabelKind::Plain, r(134.0, 164.0, 70.0, 26.0));
        let field_message = b.wrapped(
            doc,
            "",
            LabelKind::Warning,
            Some(r(210.0, 134.0, 214.0, 56.0)),
        );
        // Font.
        b.header(doc, "Font", 196.0);
        b.label(
            doc,
            "Plain text font:",
            LabelKind::Plain,
            r(20.0, 216.0, 96.0, 28.0),
        );
        let change_font = b.button(doc, "Change\u{2026}", Some(r(118.0, 216.0, 84.0, 28.0)));
        let font_name = b.label(doc, "", LabelKind::Plain, r(210.0, 216.0, 214.0, 28.0));
        // Options: spelling on the left, substitutions on the right.
        b.header(doc, "Options", 250.0);
        b.check(
            doc,
            SettingKey::CheckSpellingWhileTyping,
            "Check spelling as you type",
            SPELLING_HELP,
            r(20.0, 270.0, 212.0, 22.0),
        );
        b.check(
            doc,
            SettingKey::CorrectSpellingAutomatically,
            "Correct spelling automatically",
            SPELLING_HELP,
            r(20.0, 292.0, 212.0, 22.0),
        );
        b.check(
            doc,
            SettingKey::SmartCopyPaste,
            "Smart copy/paste",
            SUBSTITUTIONS_HELP,
            r(236.0, 270.0, 190.0, 22.0),
        );
        b.check(
            doc,
            SettingKey::SmartQuotes,
            "Smart quotes",
            SUBSTITUTIONS_HELP,
            r(236.0, 292.0, 190.0, 22.0),
        );
        b.check(
            doc,
            SettingKey::SmartDashes,
            "Smart dashes",
            SUBSTITUTIONS_HELP,
            r(236.0, 314.0, 190.0, 22.0),
        );
        // View.
        b.header(doc, "View", 342.0);
        b.check(
            doc,
            SettingKey::ShowLineNumbers,
            "Show line numbers",
            "Select to show line numbers on the left of every window.",
            r(20.0, 362.0, 212.0, 22.0),
        );
        // Open and Save.
        b.header(save, "When Saving a File:", 48.0);
        b.check(
            save,
            SettingKey::AddTxtExtension,
            "Add \".txt\" extension to plain text files",
            "Specify the default setting for the save option which adds \".txt\" to a file name with no known extension.",
            r(20.0, 68.0, 400.0, 22.0),
        );
        b.header(save, "Plain Text File Encoding", 112.0);
        b.label(
            save,
            "Opening files:",
            LabelKind::Plain,
            r(20.0, 134.0, 96.0, 30.0),
        );
        let open_combo = b.combo(
            save,
            "Encoding for opening files",
            r(118.0, 134.0, 302.0, 30.0),
        );
        b.label(
            save,
            "Saving files:",
            LabelKind::Plain,
            r(20.0, 170.0, 96.0, 30.0),
        );
        let save_combo = b.combo(
            save,
            "Encoding for saving files",
            r(118.0, 170.0, 302.0, 30.0),
        );
        // Keyboard: 394 wide, the Mac's panel, in the middle of the window.
        b.label(
            keys,
            "Click a shortcut, then press the new keys.",
            LabelKind::Plain,
            r(KEYBOARD_X, 46.0, KEYBOARD_WIDTH, 20.0),
        );
        let table_id = b.id();
        let table = b.add(
            keys,
            Box::new(ShortcutTable::new(table_id, store.clone())),
            None,
        );
        let key_message = b.wrapped(keys, "", LabelKind::Warning, None);
        let windows_note = b.wrapped(keys, WINDOWS_KEYS_NOTE, LabelKind::Hint, None);
        let restore_keys = b.button(keys, "Restore Keyboard Defaults", None);
        // The footer.
        let restore_all = b.button(None, "Restore All Defaults", None);
        let status = b.wrapped(None, "", LabelKind::Warning, None);
        let Builder {
            frame,
            places,
            checks,
            ..
        } = b;
        let mut panel = Self {
            frame,
            store,
            current,
            footer_y,
            bounds: r(0.0, 0.0, SETTINGS_WIDTH, TEXT_TABS_HEIGHT),
            places,
            tabs,
            checks,
            width,
            height,
            change_font,
            font_name,
            field_message,
            open_combo,
            save_combo,
            open_rows: EncodingRows::default(),
            save_rows: EncodingRows::default(),
            table,
            key_message,
            windows_note,
            restore_keys,
            restore_all,
            status,
            size_error: None,
            request: None,
            announcement: None,
            window_problem: None,
            surface_problem: None,
            fitted: Fitted::default(),
        };
        panel.arrange();
        panel.refresh();
        // The Width field has the keyboard first, as on the Mac.
        panel.focus_child(panel.width, 0, &mut ViewContext::default());
        panel.select_field(panel.width);
        panel
    }
    /// The height this panel wants for its tab, in dips.
    pub fn wanted_height(&self) -> f32 {
        let tab = if self.current.get() == TAB_KEYBOARD {
            KEYBOARD_HEIGHT
        } else {
            TEXT_TABS_HEIGHT
        };
        tab + self.fitted.status_lines_height
    }
    /// Measures the text that can change the panel's shape: the footer's message and the
    /// font's name. `layout` does this too. Call it before `wanted_height` when a window
    /// must size itself first. The work happens only when that text changes.
    pub fn fit_to_text(&mut self, measure: &dyn TextMeasurer) {
        let status = self.status_text();
        let font_text = self.store.font().display_text();
        if self.fitted.status != status
            || self.fitted.font != font_text
            || self.fitted.font_shown.is_empty()
        {
            let font = UiTokens::from_theme(&MEASURE_THEME).measure_font;
            let in_slot = wrap_lines(&status, STATUS_SLOT_WIDTH, measure, font);
            let status_lines_height = if lines_height(&in_slot, measure, font) <= STATUS_SLOT_HEIGHT
            {
                0.0
            } else {
                let wide = wrap_lines(&status, STATUS_WIDE, measure, font);
                lines_height(&wide, measure, font) + STATUS_WIDE_PADDING
            };
            let font_wraps = measure.measure(&font_text, font).width.0 > FONT_NAME_WIDTH;
            let font_shown = if font_wraps {
                fit_font_name(&font_text, measure, font)
            } else {
                font_text.clone()
            };
            self.fitted = Fitted {
                status,
                font: font_text,
                font_shown,
                font_wraps,
                status_lines_height,
            };
        }
        let (shown, wraps) = (self.fitted.font_shown.clone(), self.fitted.font_wraps);
        if let Some(label) = inner_mut::<Label>(&mut self.frame, self.font_name) {
            label.text = shown;
            label.wrap = wraps;
        }
    }
    pub fn current_tab(&self) -> usize {
        self.current.get()
    }
    pub fn closed(&self) -> bool {
        self.frame.closed
    }
    /// Work for the window to do. Reading clears it.
    pub fn take_request(&mut self) -> Option<PanelRequest> {
        self.request.take()
    }
    /// What a screen reader should say now. Reading clears it.
    pub fn take_announcement(&mut self) -> Option<String> {
        self.announcement
            .take()
            .or_else(|| self.table_mut().and_then(ShortcutTable::take_announcement))
    }
    /// Shows a problem the window met, such as a font dialog that could not open.
    pub fn show_problem(&mut self, problem: impl Into<String>) {
        let problem = problem.into();
        self.window_problem = (!problem.is_empty()).then_some(problem);
        self.update_status();
    }
    /// What is wrong with painting or sizing now, or `None` after a good paint. True when
    /// that changes the footer, so the window paints once more.
    pub(crate) fn set_surface_problem(&mut self, problem: Option<String>) -> bool {
        if self.surface_problem == problem {
            return false;
        }
        self.surface_problem = problem;
        self.update_status();
        true
    }
    /// A press on the title bar or the frame: a click anywhere ends a recording, as on the
    /// Mac. True when one ended.
    pub(crate) fn end_recording(&mut self) -> bool {
        let mut ctx = ViewContext::default();
        let ended = self.table_mut().is_some_and(|table| {
            let was = table.is_recording();
            table.end_recording(&mut ctx);
            was
        });
        self.update_messages();
        ended
    }
    pub fn table(&self) -> Option<&ShortcutTable> {
        inner::<ShortcutTable>(&self.frame, self.table)
    }
    fn table_mut(&mut self) -> Option<&mut ShortcutTable> {
        inner_mut::<ShortcutTable>(&mut self.frame, self.table)
    }
    /// The name of the control with the keyboard, as the widget tree has it.
    pub fn focused_name(&self) -> Option<String> {
        let child = self.frame.children.get(self.frame.focus_index)?;
        child.widgets().map(|w| w.name)
    }
    fn label_text(&self, index: usize) -> String {
        inner::<Label>(&self.frame, index)
            .map(|label| label.text.clone())
            .unwrap_or_default()
    }
    /// The line beside Restore All Defaults.
    pub fn status_text(&self) -> String {
        self.label_text(self.status)
    }
    /// The line beside the size fields.
    pub fn size_message(&self) -> String {
        self.label_text(self.field_message)
    }
    /// The line under the keyboard table.
    pub fn key_message(&self) -> String {
        self.label_text(self.key_message)
    }
    /// The font shown beside the Change button.
    pub fn font_text(&self) -> String {
        self.label_text(self.font_name)
    }
    /// Disables a control by its label, for the picture of a disabled control. No
    /// control here is disabled in normal use, as on the Mac.
    pub fn disable_for_picture(&mut self, name: &str) {
        for index in 0..self.frame.children.len() {
            if let Some(button) = inner_mut::<Button>(&mut self.frame, index)
                && button.label == name
            {
                button.enabled = false;
            }
            if let Some(check) = inner_mut::<Checkbox>(&mut self.frame, index)
                && check.label == name
            {
                check.enabled = false;
            }
        }
    }
    /// Puts the store's values in every control. A size field being typed in keeps its
    /// text, as on the Mac.
    pub fn refresh(&mut self) {
        for (index, key) in self.checks.clone() {
            let value = self.store.get_bool(key).unwrap_or(false);
            if let Some(check) = inner_mut::<Checkbox>(&mut self.frame, index) {
                check.checked = value;
            }
        }
        self.refresh_field(self.width, SettingKey::WindowWidthCharacters);
        self.refresh_field(self.height, SettingKey::WindowHeightLines);
        let font = self.store.font().display_text();
        if let Some(label) = inner_mut::<Label>(&mut self.frame, self.font_name) {
            label.text = font;
        }
        self.open_rows = self.fill_combo(self.open_combo, SettingKey::OpenEncoding);
        self.save_rows = self.fill_combo(self.save_combo, SettingKey::SaveEncoding);
        self.update_status();
        self.update_messages();
    }
    fn refresh_field(&mut self, index: usize, key: SettingKey) {
        let value = self.store.get_integer(key).unwrap_or(0).to_string();
        if let Some(field) = inner_mut::<TextField>(&mut self.frame, index)
            && !field.focused
            && field.text != value
        {
            set_field(field, &value);
        }
    }
    /// Automatic, the enabled list, and the Customize row. A saved choice that is not on
    /// the list shows as an extra row at the top, as on the Mac.
    fn fill_combo(&mut self, index: usize, key: SettingKey) -> EncodingRows {
        let enabled = self.store.enabled_encodings();
        let choice = self.store.encoding_choice(key);
        let mut items: Vec<Option<String>> = Vec::new();
        let mut rows: Vec<Option<EncodingChoice>> = Vec::new();
        if let EncodingChoice::Specific(encoding) = choice
            && !enabled.contains(&encoding)
        {
            items.push(Some(encoding.name().to_owned()));
            rows.push(Some(choice));
        }
        items.push(Some("Automatic".into()));
        rows.push(Some(EncodingChoice::Automatic));
        items.push(None);
        rows.push(None);
        for encoding in enabled {
            items.push(Some(encoding.name().to_owned()));
            rows.push(Some(EncodingChoice::Specific(encoding)));
        }
        items.push(None);
        rows.push(None);
        items.push(Some(CUSTOMIZE_ENCODINGS.to_owned()));
        rows.push(None);
        let selected = rows
            .iter()
            .position(|row| *row == Some(choice))
            .unwrap_or(0);
        if let Some(combo) = inner_mut::<ComboBox>(&mut self.frame, index)
            && !combo.open
        {
            combo.items = items;
            combo.selected = selected;
        }
        EncodingRows(rows)
    }
    /// Everything wrong now, in the footer, most recent kind first. One does not hide
    /// another, and a save that is blocked by the load problem does not say it twice.
    fn update_status(&mut self) {
        let write_error = self.store.write_error();
        let load_problem = self.store.load_problem().filter(|problem| {
            !write_error
                .as_ref()
                .is_some_and(|error| error.contains(problem.as_str()))
        });
        let text = [
            write_error,
            self.window_problem.clone(),
            self.surface_problem.clone(),
            load_problem,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
        if let Some(label) = inner_mut::<Label>(&mut self.frame, self.status) {
            label.text = text;
        }
    }
    fn update_messages(&mut self) {
        let keys = self.table().and_then(ShortcutTable::message);
        if let Some(label) = inner_mut::<Label>(&mut self.frame, self.key_message) {
            match keys {
                Some((text, warning)) => {
                    label.text = text;
                    label.kind = if warning {
                        LabelKind::Warning
                    } else {
                        LabelKind::Hint
                    };
                }
                None => label.text.clear(),
            }
        }
        let typed_junk = [self.width, self.height].into_iter().any(|index| {
            inner::<TextField>(&self.frame, index).is_some_and(|f| f.rejected_input.is_some())
        });
        let text = if typed_junk {
            "Only the digits 0 to 9 can go here.".to_owned()
        } else {
            self.size_error
                .as_ref()
                .map(|(_, text)| text.clone())
                .unwrap_or_default()
        };
        if let Some(label) = inner_mut::<Label>(&mut self.frame, self.field_message) {
            label.text = text;
        }
    }
    fn select_field(&mut self, index: usize) {
        if let Some(field) = inner_mut::<TextField>(&mut self.frame, index) {
            field.anchor = 0;
            field.caret = field.text.len();
        }
    }
    /// Takes a typed size. Junk, an empty field or a number out of range puts the last
    /// good value back, as on the Mac, and says why, which the Mac does not.
    fn commit_field(&mut self, index: usize) {
        let (key, what) = if index == self.width {
            (SettingKey::WindowWidthCharacters, "Width")
        } else if index == self.height {
            (SettingKey::WindowHeightLines, "Height")
        } else {
            return;
        };
        let good = self.store.get_integer(key).unwrap_or(0).to_string();
        let Some(field) = inner_mut::<TextField>(&mut self.frame, index) else {
            return;
        };
        field.rejected_input = None;
        if field.text == good {
            return;
        }
        let (low, high) = integer_range(key).unwrap_or((1, 10_000));
        let number = field
            .text
            .parse::<i64>()
            .ok()
            .filter(|n| (low..=high).contains(n));
        let Some(number) = number else {
            set_field(field, &good);
            let message = format!("{what} must be a whole number from {low} to {high}.");
            self.announcement = Some(message.clone());
            self.size_error = Some((index, message));
            return;
        };
        if let Err(refused) = self.store.set_integer(key, number) {
            self.size_error = Some((index, refused.0));
        }
        // "090" becomes "90".
        let shown = self.store.get_integer(key).unwrap_or(0).to_string();
        if let Some(field) = inner_mut::<TextField>(&mut self.frame, index) {
            set_field(field, &shown);
        }
    }
    /// Commits both fields. A window that closes keeps what was typed, as on the Mac.
    pub fn commit_typed(&mut self) {
        self.commit_field(self.width);
        self.commit_field(self.height);
        self.update_messages();
    }
    fn focus_child(&mut self, index: usize, time_us: u64, ctx: &mut ViewContext) {
        let old = self.frame.focus_index;
        if old != index {
            if let Some(child) = self.frame.children.get_mut(old) {
                child.handle(
                    &InputEvent {
                        time_us,
                        kind: InputKind::Blur,
                    },
                    ctx,
                );
            }
            self.frame.focus_index = index;
        }
        if let Some(child) = self.frame.children.get_mut(index) {
            child.handle(
                &InputEvent {
                    time_us,
                    kind: InputKind::Focus,
                },
                ctx,
            );
        }
        ctx.invalidate(self.bounds);
    }
    /// Shows a tab, as a click on its name does.
    pub fn select_tab(&mut self, tab: usize, ctx: &mut ViewContext) {
        if let Some(bar) = inner_mut::<TabBar>(&mut self.frame, self.tabs) {
            bar.selected = tab;
        }
        self.after_tab_change(0, ctx);
    }
    fn after_tab_change(&mut self, time_us: u64, ctx: &mut ViewContext) {
        let selected = inner::<TabBar>(&self.frame, self.tabs).map_or(0, |bar| bar.selected);
        if selected == self.current.get() {
            return;
        }
        // A recording ends when the tab changes, as on the Mac.
        if let Some(table) = self.table_mut() {
            table.end_recording(ctx);
        }
        // An old refusal does not wait on the other tab. One from this commit is new.
        self.size_error = None;
        self.commit_typed();
        self.current.set(selected);
        // Ctrl+Tab can leave the keyboard on a control that just went away.
        if !self.frame.children[self.frame.focus_index].focusable() {
            self.focus_child(self.tabs, time_us, ctx);
        }
        self.arrange();
        ctx.invalidate(self.bounds);
    }
    fn is_field(&self, index: usize) -> bool {
        index == self.width || index == self.height
    }
    fn field_text(&self, index: usize) -> String {
        inner::<TextField>(&self.frame, index)
            .map(|field| field.text.clone())
            .unwrap_or_default()
    }
    fn customize_chosen(&self, index: usize) -> bool {
        inner::<ComboBox>(&self.frame, index).is_some_and(|combo| {
            combo
                .items
                .get(combo.selected)
                .and_then(Option::as_deref)
                .is_some_and(|text| text == CUSTOMIZE_ENCODINGS)
        })
    }
    fn combo_changed(&mut self, index: usize, key: SettingKey, list_was_open: bool) {
        if self.customize_chosen(index) {
            // This row is an action, so a closed list's arrow keys do not run it. They
            // leave the value as it was. Only a row chosen from the open list runs it.
            if list_was_open {
                self.request = Some(PanelRequest::CustomizeEncodings);
            }
            self.refresh();
            return;
        }
        let rows = if index == self.open_combo {
            &self.open_rows
        } else {
            &self.save_rows
        };
        let selected = inner::<ComboBox>(&self.frame, index).map_or(0, |combo| combo.selected);
        if let Some(Some(choice)) = rows.0.get(selected).copied()
            && let Err(refused) = self.store.set_encoding_choice(key, choice)
        {
            self.show_problem(refused.0);
        }
        self.refresh();
    }
    fn act_on(
        &mut self,
        child: usize,
        event: &InputEvent,
        list_was_open: bool,
        ctx: &mut ViewContext,
    ) {
        // The user went on, so a problem from the last action is over.
        self.window_problem = None;
        if child == self.tabs {
            self.after_tab_change(event.time_us, ctx);
        } else if let Some((_, key)) = self.checks.iter().find(|(index, _)| *index == child) {
            let key = *key;
            let value = inner::<Checkbox>(&self.frame, child).is_some_and(|check| check.checked);
            if let Err(refused) = self.store.set_bool(key, value) {
                self.show_problem(refused.0);
            }
        } else if child == self.change_font {
            self.commit_typed();
            self.request = Some(PanelRequest::ChooseFont);
        } else if child == self.open_combo {
            self.combo_changed(child, SettingKey::OpenEncoding, list_was_open);
        } else if child == self.save_combo {
            self.combo_changed(child, SettingKey::SaveEncoding, list_was_open);
        } else if child == self.restore_keys {
            if let Some(table) = self.table_mut() {
                table.after_restore(ctx);
            }
            self.store.restore_keyboard_defaults();
            self.announcement = Some("Every keyboard shortcut is back to its default.".into());
        } else if child == self.restore_all {
            if let Some(table) = self.table_mut() {
                table.after_restore(ctx);
            }
            self.size_error = None;
            self.store.restore_all();
            // A half-typed size must not land after the reset.
            for (index, key) in [
                (self.width, SettingKey::WindowWidthCharacters),
                (self.height, SettingKey::WindowHeightLines),
            ] {
                let value = self.store.get_integer(key).unwrap_or(0).to_string();
                if let Some(field) = inner_mut::<TextField>(&mut self.frame, index) {
                    set_field(field, &value);
                }
            }
            self.refresh();
            self.announcement = Some("Every setting is back to its default.".into());
        }
        ctx.invalidate(self.bounds);
    }
    /// A store event: another window changed a setting, or a write ended.
    pub fn store_changed(&mut self, _change: &SettingChange) {
        self.refresh();
    }
    /// Puts each control in place for the panel's bounds. The keyboard table takes what
    /// the window has left, so a short screen shortens it and not the footer.
    fn arrange(&mut self) {
        let (x0, y0) = (self.bounds.x.0, self.bounds.y.0);
        let foot = y0 + self.bounds.height.0 - FOOTER_HEIGHT - self.fitted.status_lines_height;
        self.footer_y.set(foot);
        for (index, at) in self.places.clone() {
            self.place(index, r(x0 + at.x.0, y0 + at.y.0, at.width.0, at.height.0));
        }
        let kx = x0 + KEYBOARD_X;
        let button_y = foot - 34.0;
        let message_y = button_y - 38.0;
        let table_y = y0 + 68.0;
        // A whole number of rows, so that no blank part of a row shows at the bottom.
        let room = (message_y - 4.0 - table_y).max(ROW_HEIGHT * 3.0);
        let table_height = (room / ROW_HEIGHT).floor() * ROW_HEIGHT + 2.0;
        self.place(self.table, r(kx, table_y, KEYBOARD_WIDTH, table_height));
        self.place(self.key_message, r(kx, message_y, KEYBOARD_WIDTH, 36.0));
        self.place(self.windows_note, r(kx, button_y, 196.0, 34.0));
        self.place(
            self.restore_keys,
            r(kx + KEYBOARD_WIDTH - 190.0, button_y, 190.0, 30.0),
        );
        // Restore All Defaults sits at the lower left, as on the Mac.
        self.place(self.restore_all, r(x0 + 14.0, foot + 7.0, 164.0, 30.0));
        let extra = self.fitted.status_lines_height;
        let status_place = if extra > 0.0 {
            r(x0 + 14.0, foot + FOOTER_HEIGHT, STATUS_WIDE, extra)
        } else {
            r(
                x0 + 186.0,
                foot + 3.0,
                STATUS_SLOT_WIDTH,
                STATUS_SLOT_HEIGHT,
            )
        };
        self.place(self.status, status_place);
        let font_height = if self.fitted.font_wraps {
            FONT_NAME_WRAPPED_HEIGHT
        } else {
            FONT_NAME_HEIGHT
        };
        self.place(
            self.font_name,
            r(x0 + 210.0, y0 + FONT_NAME_Y, FONT_NAME_WIDTH, font_height),
        );
        let area = self.bounds;
        for child in &mut self.frame.children {
            child.set_popup_area(area);
        }
        self.frame.bounds = area;
    }
    fn place(&mut self, index: usize, at: Rect) {
        if let Some(child) = self.frame.child_mut::<Paged>(index) {
            child.inner.layout(at, &NoMeasure);
        }
    }
}
/// `arrange` places controls with this. None of them measures in `layout`.
struct NoMeasure;
impl TextMeasurer for NoMeasure {
    fn measure(&self, _: &str, _: &str) -> TextMetrics {
        TextMetrics {
            width: Dip(0.0),
            height: Dip(0.0),
            baseline: Dip(0.0),
        }
    }
}
fn set_field(field: &mut TextField, text: &str) {
    field.text = text.to_owned();
    field.caret = text.len();
    field.anchor = text.len();
    field.rejected_input = None;
}
impl View for SettingsPanel {
    fn handle(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled {
        // A click anywhere ends a recording and still reaches its target, as on the Mac.
        if matches!(event.kind, InputKind::MouseDown { .. })
            && let Some(table) = self.table_mut()
        {
            table.end_recording(ctx);
        }
        let before = self.frame.focus_index;
        if let InputKind::KeyDown {
            virtual_key: ENTER, ..
        } = event.kind
            && self.is_field(before)
        {
            self.commit_typed();
            self.select_field(before);
            ctx.invalidate(self.bounds);
            return Handled::Yes;
        }
        let list_was_open = [self.open_combo, self.save_combo]
            .into_iter()
            .any(|index| inner::<ComboBox>(&self.frame, index).is_some_and(|combo| combo.open));
        let typed_in = self.is_field(before).then(|| self.field_text(before));
        let handled = self.frame.handle(event, ctx);
        // Typing in the field that was refused answers that message.
        if typed_in.is_some_and(|was| was != self.field_text(before))
            && self
                .size_error
                .as_ref()
                .is_some_and(|(owner, _)| *owner == before)
        {
            self.size_error = None;
        }
        let after = self.frame.focus_index;
        if after != before {
            self.commit_field(before);
            self.select_field(after);
        }
        if matches!(event.kind, InputKind::Blur) {
            self.commit_typed();
        }
        if let Some(child) = self.frame.take_last_activated() {
            self.act_on(child, event, list_was_open, ctx);
        }
        self.update_messages();
        // A store call above may have failed while the window's state was in use, so its
        // listener could not hear of it. The footer catches up here.
        self.update_status();
        handled
    }
    fn draw(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        self.frame.draw(list, theme, measure);
    }
    fn layout(&mut self, bounds: Rect, measure: &dyn TextMeasurer) {
        self.fit_to_text(measure);
        self.bounds = bounds;
        self.frame.layout(bounds, measure);
        self.arrange();
    }
    fn widgets(&self) -> Option<Widget> {
        self.frame.widgets()
    }
}
