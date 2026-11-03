//! What the app keeps for one document beside the store's view: its gutter and map, its
//! spelling and the state of its Smart menu rows. A background tab holds this and runs nothing.
use crate::app::App;
use qe_api::{ByteOffset, ViewContext};
use qe_docs::DocId;
use qe_editor::{Clipboard, ClipboardFilter, ClipboardPreparer, EditorView};
use qe_spell::SpellService;
use qe_tools::{Substitutions, smart_delete_range, smart_paste, substitute_typed};
use qe_ui::SharedClipboard;
use qe_views::{Paper, ViewFrame, system_paper};
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK, SPI_GETWHEELSCROLLLINES,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

pub struct DocUi {
    pub frame: ViewFrame,
    /// The Smart rows, for this document alone, as on the Mac.
    pub subs: Rc<Cell<Substitutions>>,
    pub bytes: Rc<Cell<u64>>,
    pub prepared: bool,
    pub spell: Option<SpellService>,
    pub spell_on: bool,
    pub correct_on: bool,
    /// A typed break that may end a misspelled word, waiting for the checker's answer.
    pub correct_at: Option<ByteOffset>,
    /// The Show Spelling question goes on after each answer.
    pub spelling_walk: bool,
    /// The check waits for a layout, which a later wake brings.
    pub spell_waiting: bool,
    pub found: Option<qe_spell::FixMenu>,
    pub seen_revision: u64,
    pub seen_place: (Range<ByteOffset>, f64),
    /// The revision the frame last looked at, so that one edit costs one look.
    pub synced_revision: u64,
    /// Messages on show for this document. Each stays until the person dismisses it.
    pub notices: Vec<String>,
    /// The view's own warning, as last shown.
    pub seen_warning: Option<String>,
    pub bar: crate::notices::BarLayout,
}

/// The real clipboard, with Copy's slow part on a worker.
struct SystemClipboard(qe_ui::SystemClipboard);
impl Clipboard for SystemClipboard {
    fn read(&mut self) -> Result<Option<String>, String> {
        qe_ui::ClipboardAccess::read_text(&self.0)
    }
    fn write(&mut self, value: &str) -> Result<(), String> {
        qe_ui::ClipboardAccess::write_text(&self.0, value)
    }
    fn preparer(&self) -> Option<ClipboardPreparer> {
        Some(Arc::new(|text, cancel| {
            let prepared = qe_ui::prepare_clipboard_text_unless(text, cancel)?;
            Ok(Box::new(prepared) as Box<dyn std::any::Any + Send>)
        }))
    }
    fn write_prepared(&mut self, prepared: Box<dyn std::any::Any + Send>) -> Result<(), String> {
        let prepared = prepared
            .downcast::<qe_ui::PreparedClipboardText>()
            .map_err(|_| "the copied text was prepared for another clipboard".to_string())?;
        self.0.write_prepared(*prepared)
    }
}
/// The test channel's clipboard. No test touches the system's.
struct SharedMemoryClipboard(SharedClipboard);
impl Clipboard for SharedMemoryClipboard {
    fn read(&mut self) -> Result<Option<String>, String> {
        self.0.read_text()
    }
    fn write(&mut self, value: &str) -> Result<(), String> {
        self.0.write_text(value)
    }
}

struct SmartClipboard {
    subs: Rc<Cell<Substitutions>>,
    bytes: Rc<Cell<u64>>,
}
impl SmartClipboard {
    fn options(&self) -> Substitutions {
        self.subs.get().for_document(self.bytes.get())
    }
}
impl ClipboardFilter for SmartClipboard {
    fn cut_range(
        &mut self,
        selected: Range<ByteOffset>,
        before: Option<char>,
        after: Option<char>,
        was_word: bool,
    ) -> Range<ByteOffset> {
        smart_delete_range(selected, before, after, self.options(), was_word)
    }
    fn paste_text(
        &mut self,
        text: &str,
        before: Option<char>,
        after: Option<char>,
        was_word: bool,
    ) -> String {
        smart_paste(text, before, after, self.options(), was_word)
    }
}

/// The paper setting. A saved size wins, and otherwise the user's region decides.
pub fn paper_from_settings(settings: &qe_settings::SettingsStore) -> Paper {
    match settings.get_text(qe_api::SettingKey::PaperSize).as_deref() {
        Some("Letter") => Paper::LETTER,
        Some("A4") => Paper::A4,
        Some("Legal") => Paper::LEGAL,
        _ => system_paper(),
    }
}

impl App {
    /// The app's part of a document, made the first time the app hears of it.
    pub fn doc_ui(&mut self, doc: DocId) -> Option<&mut DocUi> {
        if !self.docs.contains_key(&doc) {
            self.store.view(doc)?;
            let font = self.settings.font();
            let mut frame = ViewFrame::new(
                paper_from_settings(&self.settings),
                &font.family,
                font.points,
            );
            let wake = self.wake.clone();
            frame.set_wake(Arc::new(move || wake()));
            let key = qe_api::SettingKey::CheckSpellingWhileTyping;
            let spell_on = self.settings.get_bool(key).unwrap_or(false);
            let key = qe_api::SettingKey::CorrectSpellingAutomatically;
            let correct_on = self.settings.get_bool(key).unwrap_or(false);
            let flag = |key| self.settings.get_bool(key).unwrap_or(false);
            let subs = Substitutions {
                smart_copy: flag(qe_api::SettingKey::SmartCopyPaste),
                quotes: flag(qe_api::SettingKey::SmartQuotes),
                dashes: flag(qe_api::SettingKey::SmartDashes),
            };
            self.docs.insert(
                doc,
                DocUi {
                    frame,
                    subs: Rc::new(Cell::new(subs)),
                    bytes: Rc::new(Cell::new(self.store.byte_count(doc))),
                    prepared: false,
                    spell: None,
                    spell_on,
                    correct_on,
                    correct_at: None,
                    spelling_walk: false,
                    spell_waiting: false,
                    found: None,
                    seen_revision: 0,
                    seen_place: (ByteOffset(0)..ByteOffset(0), 0.0),
                    synced_revision: 0,
                    notices: Vec::new(),
                    seen_warning: None,
                    bar: crate::notices::BarLayout::default(),
                },
            );
        }
        self.docs.get_mut(&doc)
    }

    /// Everything a view needs before it first shows. Costs nothing for a tab never shown.
    pub fn prepare_doc(&mut self, doc: DocId, ctx: &mut ViewContext) {
        if self.doc_ui(doc).is_none_or(|ui| ui.prepared) {
            return;
        }
        let font = self.settings.font();
        let key = qe_api::SettingKey::TabWidth;
        let tab_width = self.settings.get_integer(key).unwrap_or(8).max(1) as usize;
        let line_numbers = self
            .settings
            .get_bool(qe_api::SettingKey::ShowLineNumbers)
            .unwrap_or(false);
        let document_map = self
            .settings
            .get_bool(qe_api::SettingKey::ShowDocumentMap)
            .unwrap_or(true);
        let page_wrap = self.store.wrap_to_page(doc);
        let bytes = self.store.byte_count(doc);
        let clipboard = self.clipboard.clone();
        let measure = self.measure.clone();
        let scale = self
            .store
            .window_of(doc)
            .and_then(|window| self.windows.get(&window))
            .map_or(1.0, |win| 96.0 / win.dpi.get().max(1) as f32);
        let (Some(ui), Some(view)) = (self.docs.get_mut(&doc), self.store.view_mut(doc)) else {
            return;
        };
        ui.prepared = true;
        view.set_host_shows_warnings(true);
        view.set_host_runs_command_keys(true);
        match clipboard {
            Some(memory) => view.set_clipboard(Box::new(SharedMemoryClipboard(memory))),
            None => view.set_clipboard(Box::new(SystemClipboard(qe_ui::SystemClipboard))),
        }
        set_input_limits(view, scale);
        let (subs, size) = (ui.subs.clone(), ui.bytes.clone());
        view.set_typing_filter(Some(Box::new(move |typed, before| {
            let options = subs.get().for_document(size.get());
            substitute_typed(typed, before, options).map(|edit| (edit.remove_before, edit.insert))
        })));
        view.set_clipboard_filter(Some(Box::new(SmartClipboard {
            subs: ui.subs.clone(),
            bytes: ui.bytes.clone(),
        })));
        let mut problems = Vec::new();
        if let Err(e) =
            ui.frame
                .set_base_font(&font.family, font.points, view, measure.as_ref(), ctx)
        {
            problems.push(format!("The font {} could not be used: {e}", font.family));
        }
        ui.frame
            .set_tab_width(tab_width, view, measure.as_ref(), ctx);
        ui.frame
            .set_line_numbers(line_numbers, view, measure.as_ref(), ctx);
        ui.frame
            .set_document_map(document_map, view, measure.as_ref(), ctx);
        if page_wrap && let Err(e) = ui.frame.set_page_wrap(true, view, measure.as_ref(), ctx) {
            problems.push(format!("Wrap to Page could not start: {e}"));
        }
        ui.frame.set_file_bytes(Some(bytes));
        ui.seen_revision = view.revision();
        ui.seen_place = (view.selection(), view.scroll_offset());
        for problem in problems {
            self.note(problem);
        }
    }
}

/// The system's double-click and wheel settings.
fn set_input_limits(view: &mut EditorView, scale: f32) {
    // SAFETY: plain queries of system settings.
    let (time_ms, width, height) = unsafe {
        (
            GetDoubleClickTime(),
            GetSystemMetrics(SM_CXDOUBLECLK),
            GetSystemMetrics(SM_CYDOUBLECLK),
        )
    };
    view.set_double_click_limits(
        u64::from(time_ms) * 1000,
        width as f32 * scale,
        height as f32 * scale,
    );
    let mut lines = 3_u32;
    // SAFETY: the call writes one u32 to `lines`, which lives through the call.
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            Some((&raw mut lines).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    match read {
        Ok(()) => view.set_wheel_lines(lines),
        Err(e) => eprintln!("reading the wheel setting failed, so one notch is 3 rows: {e}"),
    }
}
