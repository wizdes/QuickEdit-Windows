//! Questions and errors, one at a time, in qe-ui's painted alert. Nothing here waits: the alert
//! disables its owner, the normal message loop runs it, and its answer joins the app's queue.
use crate::app::{App, Incoming, Shared, push, queue_note};
use qe_a11y::{UiThreadOnly, UiaHost, UiaSlot, WidgetAction};
use qe_api::Widget;
use qe_api::{
    Dip, DisplayList, DrawCommand, InputEvent, InputKind, Modifiers, MouseButton, PixelRect, Point,
    Rect, Renderer, Theme, ThemeKind, View, ViewContext, WidgetId,
};
use qe_docs::{Answer, DocId, QuestionId};
use qe_render::BitmapRenderer;
use qe_ui::{AlertModel, DialogFrame, DialogResult, DialogWindow, TextField, UiTokens};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use windows::Win32::Foundation::{HWND, LRESULT};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const ALERT_ID: u64 = 30_000;
const ALERT_WIDTH: f32 = 440.0;

#[derive(Clone, Debug)]
pub enum AlertPurpose {
    Note,
    Question {
        id: QuestionId,
        choices: Vec<Answer>,
    },
    SaveFailed {
        doc: DocId,
        offer_save_as: bool,
    },
    Rename(DocId),
    Leftover {
        path: PathBuf,
        leftover: PathBuf,
    },
    /// Show Spelling's question about one word.
    SpellWord {
        doc: DocId,
        suggestion: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct AlertRequest {
    pub title: String,
    pub message: String,
    pub buttons: Vec<String>,
    pub purpose: AlertPurpose,
    /// A text box with this text, for a name.
    pub field: Option<String>,
    pub owner: Option<HWND>,
}

/// How an alert ended.
#[derive(Clone, Debug)]
pub struct AlertDone {
    pub purpose: AlertPurpose,
    /// `None` means the window's own close button, or Esc.
    pub button: Option<usize>,
    pub field: Option<String>,
}

struct Open {
    frame: DialogFrame,
    window: DialogWindow,
    renderer: BitmapRenderer,
    theme: Theme,
    scale: f32,
    measure: std::sync::Arc<qe_layout::DirectWriteLayout>,
    purpose: AlertPurpose,
    has_field: bool,
    title: String,
    message: String,
    buttons: Vec<String>,
}

#[derive(Default)]
pub struct Alerts {
    current: RefCell<Option<Rc<RefCell<Open>>>>,
    waiting: RefCell<VecDeque<AlertRequest>>,
}

fn modifiers() -> Modifiers {
    // SAFETY: plain queries about this thread's key state.
    let down = |key: u16| unsafe { GetKeyState(i32::from(key)) < 0 };
    Modifiers {
        ctrl: down(VK_CONTROL.0),
        alt: down(VK_MENU.0),
        shift: down(VK_SHIFT.0),
        win: false,
    }
}

fn dips(scale: f32, l: isize) -> Point {
    Point {
        x: Dip(f32::from((l & 0xffff) as i16) / scale),
        y: Dip(f32::from(((l >> 16) & 0xffff) as i16) / scale),
    }
}

impl Open {
    fn paint(&mut self) {
        let mut list = DisplayList::default();
        list.push(DrawCommand::FillRect {
            rect: self.frame.bounds,
            color: UiTokens::from_theme(&self.theme).background,
        });
        self.frame
            .draw(&mut list, &self.theme, self.measure.as_ref());
        self.renderer.invalidate_all();
        let painted = self
            .renderer
            .begin_frame()
            .and_then(|()| self.renderer.draw(&list))
            .and_then(|()| self.renderer.end_frame());
        if let Err(e) = painted {
            eprintln!("Painting an alert failed: {e}");
        }
        // SAFETY: a live window of this thread.
        let _ = unsafe { InvalidateRect(Some(self.window.hwnd()), None, false) };
    }
    fn field_text(&self) -> Option<String> {
        if self.has_field {
            let last = self.frame.children.len().checked_sub(1)?;
            self.frame
                .child::<TextField>(last)
                .map(|field| field.text.clone())
        } else {
            None
        }
    }
}

impl Alerts {
    pub fn request(&self, shared: &Rc<Shared>, request: AlertRequest) {
        self.waiting.borrow_mut().push_back(request);
        self.next(shared);
    }
    /// Opens the next one, if none is open.
    fn next(&self, shared: &Rc<Shared>) {
        if self.current.borrow().is_some() {
            return;
        }
        let Some(request) = self.waiting.borrow_mut().pop_front() else {
            return;
        };
        let purpose = request.purpose.clone();
        if let Err(problem) = self.open(shared, request) {
            queue_note(shared, format!("A question could not show: {problem}"));
            // Without its window, a question still needs its safe answer.
            push(
                shared,
                Incoming::Alert(AlertDone {
                    purpose,
                    button: None,
                    field: None,
                }),
            );
            self.next(shared);
        }
    }
    fn open(&self, shared: &Rc<Shared>, request: AlertRequest) -> Result<(), String> {
        let scale = request.owner.map_or(1.0, |owner| {
            // SAFETY: a plain query. A dead handle gives 0.
            let dpi = unsafe { GetDpiForWindow(owner) };
            if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
        });
        let lines = (request.message.chars().count() / 52
            + 1
            + request.message.matches('\n').count()) as f32;
        let height = 88.0 + lines * 20.0 + if request.field.is_some() { 36.0 } else { 0.0 };
        let bounds = Rect {
            x: Dip(0.0),
            y: Dip(0.0),
            width: Dip(ALERT_WIDTH),
            height: Dip(height),
        };
        let model = AlertModel::new(
            request.title.clone(),
            request.message.clone(),
            request.buttons.clone(),
        );
        let mut frame = DialogFrame::from_alert(WidgetId(ALERT_ID), &model, bounds);
        let window = DialogWindow::create_with_owner(
            &request.title,
            (ALERT_WIDTH * scale).round() as i32,
            (height * scale).round() as i32,
            request.owner.is_some(),
            false,
            request.owner,
        )
        .map_err(|e| e.to_string())?;
        let hwnd = window.hwnd();
        let (width, height_px) = window.client_size().map_err(|e| e.to_string())?;
        let mut renderer =
            BitmapRenderer::new(width as u32, height_px as u32).map_err(|e| e.message)?;
        renderer
            .set_dpi((scale * 96.0).round() as u32)
            .map_err(|e| e.message)?;
        let theme = shared_theme(shared);
        let has_field = request.field.is_some();
        if let Some(text) = &request.field {
            let mut field = TextField::new(WidgetId(ALERT_ID + 50), "Name");
            if shared.channel {
                field = field.with_clipboard(qe_ui::MemoryClipboard::shared());
            }
            field.text.clone_from(text);
            field.bounds = Rect {
                x: Dip(16.0),
                y: Dip(44.0),
                width: Dip(ALERT_WIDTH - 32.0),
                height: Dip(26.0),
            };
            // It goes last, so that each button keeps its number. It takes the first focus.
            let mut ctx = ViewContext::default();
            let old = frame.focus_index;
            frame.children[old].handle(
                &InputEvent {
                    time_us: 0,
                    kind: InputKind::Blur,
                },
                &mut ctx,
            );
            frame.add(Box::new(field));
            frame.focus_index = frame.children.len() - 1;
            let at = frame.focus_index;
            frame.children[at].handle(
                &InputEvent {
                    time_us: 0,
                    kind: InputKind::Focus,
                },
                &mut ctx,
            );
        }
        let open = Rc::new(RefCell::new(Open {
            frame,
            window,
            renderer,
            theme,
            scale,
            measure: shared.measure.clone(),
            purpose: request.purpose,
            has_field,
            title: request.title,
            message: request.message,
            buttons: request.buttons,
        }));
        open.borrow_mut().paint();
        let painter = open.clone();
        let window_ref = open.borrow();
        window_ref
            .window
            .set_paint_handler(Box::new(move |dc, rect| {
                if let Ok(mut open) = painter.try_borrow_mut() {
                    let done = <BitmapRenderer as Renderer>::present_rect(
                        &mut open.renderer,
                        dc.0,
                        PixelRect {
                            left: rect.left,
                            top: rect.top,
                            right: rect.right,
                            bottom: rect.bottom,
                        },
                    );
                    if let Err(e) = done {
                        eprintln!("Showing an alert failed: {e}");
                    }
                }
            }));
        let messages = Rc::downgrade(&open);
        let weak = Rc::downgrade(shared);
        let slot = Rc::new(UiaSlot::new());
        let for_host = (Rc::downgrade(&open), Rc::downgrade(shared));
        window_ref
            .window
            .set_message_handler(Box::new(move |hwnd, message, w, l| {
                if message == WM_DESTROY
                    && let Err(e) = slot.shutdown()
                {
                    eprintln!("A screen reader's link to a question would not end: {e}");
                }
                let host = for_host.clone();
                if let Some(answer) = slot.on_window_message(hwnd, message, w, l, move || {
                    Arc::new(AlertUia {
                        ui: UiThreadOnly::new(host),
                    })
                }) {
                    return Some(answer);
                }
                let w = w.0;
                let l = l.0;
                let open = messages.upgrade()?;
                let shared = weak.upgrade()?;
                alert_message(&open, &shared, message, w, l)
            }));
        drop(window_ref);
        if !shared.hidden {
            // SAFETY: plain calls on a window of this thread.
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(hwnd));
            }
        }
        *self.current.borrow_mut() = Some(open);
        Ok(())
    }
    /// The window's own close button closed the alert.
    pub fn closed_by_window(&self, shared: &Rc<Shared>) {
        self.finish(shared, None);
    }
    /// Ends the open alert with this answer, and goes on to the next.
    pub fn finish(&self, shared: &Rc<Shared>, button: Option<usize>) {
        let Some(open) = self.current.borrow_mut().take() else {
            return;
        };
        let done = {
            let Ok(state) = open.try_borrow() else {
                return;
            };
            AlertDone {
                purpose: state.purpose.clone(),
                button,
                field: state.field_text(),
            }
        };
        // The window goes when the last reference does. A message on its way finds it gone.
        if let Ok(state) = open.try_borrow()
            && state.window.is_alive()
        {
            // SAFETY: a window of this thread. Failure means it has gone already.
            let _ = unsafe { DestroyWindow(state.window.hwnd()) };
        }
        push(shared, Incoming::Alert(done));
        self.next(shared);
    }
    /// This question has gone from the store, as when its document closed. It needs no answer.
    pub fn drop_question(&self, shared: &Rc<Shared>, gone: QuestionId) {
        let is_it = |purpose: &AlertPurpose| matches!(purpose, AlertPurpose::Question { id, .. } if *id == gone);
        self.waiting
            .borrow_mut()
            .retain(|request| !is_it(&request.purpose));
        let open = self.current.borrow().clone();
        let Some(open) = open else {
            return;
        };
        let Ok(state) = open.try_borrow() else {
            return;
        };
        if is_it(&state.purpose) {
            if state.window.is_alive() {
                // SAFETY: a window of this thread. Failure means it has gone already.
                let _ = unsafe { DestroyWindow(state.window.hwnd()) };
            }
            drop(state);
            *self.current.borrow_mut() = None;
            self.next(shared);
        }
    }
    /// What is open now, for the test channel: title, message and buttons.
    pub fn describe(&self) -> Option<(String, String, Vec<String>)> {
        let current = self.current.borrow();
        let open = current.as_ref()?.try_borrow().ok()?;
        Some((
            open.title.clone(),
            open.message.clone(),
            open.buttons.clone(),
        ))
    }
    /// The open alert's window, for the test channel's real key messages.
    pub fn window(&self) -> Option<HWND> {
        let current = self.current.borrow();
        Some(current.as_ref()?.try_borrow().ok()?.window.hwnd())
    }
    /// Sets the text box of the open alert, for the test channel.
    pub fn set_field(&self, text: &str) -> bool {
        let current = self.current.borrow();
        let Some(open) = current.as_ref() else {
            return false;
        };
        let Ok(mut open) = open.try_borrow_mut() else {
            return false;
        };
        let last = open.frame.children.len().saturating_sub(1);
        match open.frame.child_mut::<TextField>(last) {
            Some(field) => {
                field.text = text.to_owned();
                true
            }
            None => false,
        }
    }
}

/// Lets a screen reader read a question and press its buttons.
struct AlertUia {
    ui: UiThreadOnly<(Weak<RefCell<Open>>, Weak<Shared>)>,
}
impl UiaHost for AlertUia {
    fn widgets(&self) -> Option<Widget> {
        let open = self.ui.get()?.0.upgrade()?;
        let open = open.try_borrow().ok()?;
        open.frame.widgets()
    }
    fn action(&self, id: WidgetId, action: WidgetAction) -> windows::core::Result<()> {
        let Some((open, shared)) = self.ui.get() else {
            return Ok(());
        };
        let (Some(open), Some(shared)) = (open.upgrade(), shared.upgrade()) else {
            return Ok(());
        };
        match action {
            WidgetAction::Invoke | WidgetAction::Select => {
                // The label is `ALERT_ID + 1`, and the buttons follow it.
                let count = open.try_borrow().map_or(0, |open| open.buttons.len()) as u64;
                let first = ALERT_ID + 2;
                if id.0 >= first && id.0 < first + count {
                    shared.alerts.finish(&shared, Some((id.0 - first) as usize));
                }
            }
            WidgetAction::SetValue(text) => {
                shared.alerts.set_field(&text);
            }
            _ => {}
        }
        Ok(())
    }
}

fn shared_theme(shared: &Rc<Shared>) -> Theme {
    shared
        .state
        .try_borrow()
        .ok()
        .and_then(|app| {
            let window = app.store.last_used_window()?;
            app.windows.get(&window)?.handle.theme().ok()
        })
        .unwrap_or_else(|| Theme::new(ThemeKind::Light))
}

fn alert_message(
    open: &Rc<RefCell<Open>>,
    shared: &Rc<Shared>,
    message: u32,
    w: usize,
    l: isize,
) -> Option<LRESULT> {
    let mut state = open.try_borrow_mut().ok()?;
    let at = |l| dips(state.scale, l);
    let kind = match message {
        WM_KEYDOWN => InputKind::KeyDown {
            virtual_key: w as u16,
            modifiers: modifiers(),
            repeat: l & (1 << 30) != 0,
        },
        WM_KEYUP => InputKind::KeyUp {
            virtual_key: w as u16,
            modifiers: modifiers(),
        },
        WM_CHAR => InputKind::Char(char::from_u32(w as u32)?),
        WM_MOUSEMOVE => InputKind::MouseMove { at: at(l) },
        WM_LBUTTONDOWN => InputKind::MouseDown {
            at: at(l),
            button: MouseButton::Left,
            modifiers: modifiers(),
        },
        WM_LBUTTONUP => InputKind::MouseUp {
            at: at(l),
            button: MouseButton::Left,
            modifiers: modifiers(),
        },
        _ => return None,
    };
    let mut ctx = ViewContext::default();
    let event = InputEvent { time_us: 0, kind };
    state.frame.handle(&event, &mut ctx);
    let result = state.frame.result();
    let repaint = !ctx.requests.is_empty();
    if repaint && result.is_none() {
        state.paint();
    }
    drop(state);
    if let Some(result) = result {
        let button = match result {
            DialogResult::Button(index) => Some(index),
            DialogResult::Dismissed => None,
        };
        shared.alerts.finish(shared, button);
    }
    Some(LRESULT(0))
}

impl App {
    pub fn alert_done(&mut self, done: AlertDone) {
        let AlertDone {
            purpose,
            button,
            field,
        } = done;
        match purpose {
            AlertPurpose::Note => {}
            AlertPurpose::Question { id, choices } => {
                // No button means the safe way out. Every question has Cancel, or its first
                // choice is the one that keeps the text.
                let answer = button
                    .and_then(|index| choices.get(index).copied())
                    .or_else(|| choices.iter().copied().find(|a| *a == Answer::Cancel))
                    .or_else(|| choices.first().copied());
                if let Some(answer) = answer
                    && let Err(e) = self.store.answer(id, answer)
                {
                    // Nothing happened, and the question stays open. It shows again.
                    self.shown_questions.remove(&id);
                    self.error_alert("That answer could not be carried out", &e.message);
                }
            }
            AlertPurpose::SaveFailed { doc, offer_save_as } => {
                if offer_save_as && button == Some(0) {
                    self.store.request_save_as(doc);
                }
            }
            AlertPurpose::Rename(doc) => {
                if button == Some(0)
                    && let Some(name) = field
                    && let Err(e) = self.store.rename(doc, &name)
                {
                    self.error_alert("The file could not be renamed", &e.message);
                }
            }
            AlertPurpose::Leftover { path, leftover } => match button {
                Some(0) => self.store.recover_leftover(leftover, path),
                Some(1) => self.store.discard_leftover(leftover, path),
                _ => {}
            },
            AlertPurpose::SpellWord { doc, suggestion } => {
                self.spell_answer(doc, button, suggestion);
            }
        }
    }
}
