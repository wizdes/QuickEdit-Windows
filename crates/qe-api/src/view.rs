use crate::{
    CommandId, DisplayList, InputEvent, Point, Rect, TextMeasurer, Theme, Widget, WidgetId,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    Yes,
    No,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseCursor {
    Arrow,
    IBeam,
    Hand,
    SizeHorizontal,
    SizeVertical,
    Wait,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ViewRequest {
    Run(CommandId),
    Invalidate(Rect),
    SetCursor(MouseCursor),
    CaptureMouse(bool),
    RequestTimer(u32),
    SetFocus(WidgetId),
    /// Asks the host to end the input method's composition with no text. Esc asks this.
    CancelComposition,
    /// Asks the host to open the context menu at this point in window coordinates.
    ContextMenu(Point),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewContext {
    pub requests: Vec<ViewRequest>,
}

impl ViewContext {
    pub fn run(&mut self, command: CommandId) {
        self.requests.push(ViewRequest::Run(command));
    }
    pub fn invalidate(&mut self, rect: Rect) {
        self.requests.push(ViewRequest::Invalidate(rect));
    }
    pub fn set_cursor(&mut self, cursor: MouseCursor) {
        self.requests.push(ViewRequest::SetCursor(cursor));
    }
    pub fn capture_mouse(&mut self, capture: bool) {
        self.requests.push(ViewRequest::CaptureMouse(capture));
    }
    pub fn request_timer(&mut self, milliseconds: u32) {
        self.requests.push(ViewRequest::RequestTimer(milliseconds));
    }
    pub fn set_focus(&mut self, widget: WidgetId) {
        self.requests.push(ViewRequest::SetFocus(widget));
    }
    pub fn cancel_composition(&mut self) {
        self.requests.push(ViewRequest::CancelComposition);
    }
    pub fn context_menu(&mut self, at: Point) {
        self.requests.push(ViewRequest::ContextMenu(at));
    }
}

/// A view is called on the UI thread. The app applies requests after `handle`.
pub trait View {
    fn handle(&mut self, event: &InputEvent, ctx: &mut ViewContext) -> Handled;
    fn draw(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer);
    fn layout(&mut self, bounds: Rect, measure: &dyn TextMeasurer);
    fn widgets(&self) -> Option<Widget>;
}
