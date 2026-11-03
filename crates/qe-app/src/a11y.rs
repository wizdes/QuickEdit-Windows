//! What a screen reader sees: the painted tree, plus one document object for the view on show.
//! Every call here runs from a sent message, so each one uses `try_borrow`.
use crate::app::{Incoming, Shared, pump, push};
use qe_a11y::{UiThreadOnly, UiaHost, WidgetAction};
use qe_api::{Dip, Rect, ViewContext, Widget, WidgetId, WidgetRole, WidgetStates};
use qe_docs::WindowId;
use qe_editor::EditorView;
use std::rc::Weak;
use std::sync::Arc;

const DOCUMENT_WIDGET: u64 = 19_000;

struct AppUiaHost {
    shared: UiThreadOnly<Weak<Shared>>,
    window: WindowId,
}

pub fn make_host(shared: Weak<Shared>, window: WindowId) -> Arc<dyn UiaHost> {
    Arc::new(AppUiaHost {
        shared: UiThreadOnly::new(shared),
        window,
    })
}

fn find(widget: &Widget, id: WidgetId) -> Option<&Widget> {
    if widget.id == id {
        return Some(widget);
    }
    widget.children.iter().find_map(|child| find(child, id))
}

impl UiaHost for AppUiaHost {
    fn widgets(&self) -> Option<Widget> {
        let shared = self.shared.get()?.upgrade()?;
        let app = shared.state.try_borrow().ok()?;
        app.widget_tree(self.window)
    }
    fn action(&self, id: WidgetId, action: WidgetAction) -> windows::core::Result<()> {
        let Some(shared) = self.shared.get().and_then(Weak::upgrade) else {
            return Ok(());
        };
        push(&shared, Incoming::Action(self.window, id, action));
        Ok(())
    }
    fn document_widget(&self) -> Option<Widget> {
        let shared = self.shared.get()?.upgrade()?;
        let app = shared.state.try_borrow().ok()?;
        let win = app.windows.get(&self.window)?;
        let doc = app.store.active_tab(self.window)?;
        let focused = win.tools.focus() == qe_tools::ToolsFocus::Editor;
        let bounds = Rect {
            x: Dip(win.content.x.0),
            y: Dip(win.content.y.0),
            width: Dip(win.content.width.0),
            height: Dip(win.content.height.0),
        };
        Some(Widget {
            id: WidgetId(DOCUMENT_WIDGET),
            bounds,
            role: WidgetRole::TextField,
            name: app.store.display_name(doc),
            value: None,
            states: WidgetStates {
                enabled: true,
                focused,
                ..Default::default()
            },
            selection: None,
            range_value: None,
            description: None,
            char_boxes: Vec::new(),
            access_key: None,
            children: Vec::new(),
        })
    }
    fn view_id(&self) -> Option<u64> {
        let shared = self.shared.get()?.upgrade()?;
        let app = shared.state.try_borrow().ok()?;
        Some(app.store.active_tab(self.window)?.0)
    }
    fn with_editor(&self, body: &mut dyn FnMut(&mut EditorView, &mut ViewContext)) -> bool {
        let Some(shared) = self.shared.get().and_then(Weak::upgrade) else {
            return false;
        };
        let Ok(mut app) = shared.state.try_borrow_mut() else {
            return false;
        };
        let Some(doc) = app.store.active_tab(self.window) else {
            return false;
        };
        let Some(view) = app.store.view_mut(doc) else {
            return false;
        };
        let mut ctx = ViewContext::default();
        body(view, &mut ctx);
        app.apply(self.window, ctx.requests);
        app.after_view_call(self.window, doc);
        drop(app);
        pump(&shared);
        true
    }
    fn font(&self) -> Option<(String, f64)> {
        let shared = self.shared.get()?.upgrade()?;
        let font = shared.settings.font();
        Some((font.family, f64::from(font.points)))
    }
    fn dpi(&self) -> Option<u32> {
        let shared = self.shared.get()?.upgrade()?;
        let app = shared.state.try_borrow().ok()?;
        Some(app.windows.get(&self.window)?.dpi.get())
    }
    fn is_checkable(&self, id: WidgetId) -> bool {
        self.widgets()
            .as_ref()
            .and_then(|tree| find(tree, id).map(|widget| widget.states.checkable))
            .unwrap_or(false)
    }
    fn action_failed(&self, id: WidgetId, action: &WidgetAction, error: &windows::core::Error) {
        if let Some(shared) = self.shared.get().and_then(Weak::upgrade) {
            push(
                &shared,
                Incoming::Note(format!(
                    "A screen reader's {action:?} on control {} failed: {error}",
                    id.0
                )),
            );
        }
    }
}
