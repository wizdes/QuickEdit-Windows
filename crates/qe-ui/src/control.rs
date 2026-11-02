use qe_api::{
    DisplayList, InputKind, Rect, TextMeasurer, Theme, View, ViewContext, Widget, WidgetId,
    WidgetRole,
};
use std::any::Any;
use std::collections::HashSet;

pub(crate) const TAB: u16 = 0x09;
pub(crate) const BACK: u16 = 0x08;
pub(crate) const ENTER: u16 = 0x0d;
pub(crate) const ESCAPE: u16 = 0x1b;
pub(crate) const SPACE: u16 = 0x20;
pub(crate) const PAGE_UP: u16 = 0x21;
pub(crate) const PAGE_DOWN: u16 = 0x22;
pub(crate) const END: u16 = 0x23;
pub(crate) const HOME: u16 = 0x24;
pub(crate) const LEFT: u16 = 0x25;
pub(crate) const UP: u16 = 0x26;
pub(crate) const RIGHT: u16 = 0x27;
pub(crate) const DOWN: u16 = 0x28;
pub(crate) const INSERT: u16 = 0x2d;
pub(crate) const DELETE: u16 = 0x2e;
pub(crate) const F4: u16 = 0x73;
/// One wheel notch, in the units of `InputKind::Wheel`.
pub(crate) const WHEEL_NOTCH: f32 = 120.0;

/// True for a key pressed with Alt, which AltGr also reports. These chords belong to
/// the system and the host, such as Alt+F4 and Alt+Space, so a control leaves them.
pub(crate) fn alt_chord(kind: &InputKind) -> bool {
    matches!(kind, InputKind::KeyDown { modifiers, .. } if modifiers.alt)
}

/// What a `DialogFrame` needs from a child beyond `View`.
pub trait Control: View + Any {
    /// True when Tab may stop here.
    fn focusable(&self) -> bool {
        self.widgets().is_some_and(|w| {
            w.states.enabled && !matches!(w.role, WidgetRole::Label | WidgetRole::Group)
        })
    }
    /// True when this control acts on the key while it has focus, so that the
    /// frame must not take the key for itself.
    fn uses_key(&self, _virtual_key: u16) -> bool {
        false
    }
    /// True once after the user changed or activated this control. Every control
    /// reports a user's change here, by any route: key, wheel or mouse.
    fn take_activated(&mut self) -> bool {
        false
    }
    /// Ends a key press that began on this control, with no click.
    fn cancel_press(&mut self, _: &mut ViewContext) {}
    /// Makes this control the one Enter presses. False if it cannot be that.
    fn mark_default(&mut self) -> bool {
        false
    }
    /// Moves to the next or previous tab, for Ctrl+Tab. False if this has no tabs.
    fn switch_tab(&mut self, _back: bool, _: &mut ViewContext) -> bool {
        false
    }
    /// The part painted above other controls, such as an open list.
    fn overlay_bounds(&self) -> Option<Rect> {
        None
    }
    /// The area that an open list must stay inside.
    fn set_popup_area(&mut self, _area: Rect) {}
    /// Paints all but the overlay.
    fn draw_base(&self, list: &mut DisplayList, theme: &Theme, measure: &dyn TextMeasurer) {
        self.draw(list, theme, measure)
    }
    fn draw_overlay(&self, _: &mut DisplayList, _: &Theme, _: &dyn TextMeasurer) {}
    /// The area that takes mouse presses.
    fn hit_bounds(&self) -> Option<Rect> {
        self.widgets().map(|w| w.bounds)
    }
}

/// The id of a node that a control makes for itself, such as a row or a cell.
///
/// Callers choose small ids for their controls. These ids have the top bit set and
/// mix in the parent's id, so that they do not meet a caller's id or another
/// control's rows. Numbers such as `id + row` did meet: a table's second row had
/// the id of the control beside it.
pub fn child_id(parent: WidgetId, slot: u64) -> WidgetId {
    let mut x = parent.0 ^ slot.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    WidgetId(x.wrapping_add(slot) | (1 << 63))
}

/// The ids that appear more than once in a tree. An empty list means that a screen
/// reader can tell every node apart.
pub fn duplicate_ids(tree: &Widget) -> Vec<WidgetId> {
    fn walk(node: &Widget, seen: &mut HashSet<WidgetId>, out: &mut Vec<WidgetId>) {
        if !seen.insert(node.id) {
            out.push(node.id);
        }
        for child in &node.children {
            walk(child, seen, out);
        }
    }
    let (mut seen, mut out) = (HashSet::new(), Vec::new());
    walk(tree, &mut seen, &mut out);
    out
}

/// The nodes of a tree that say they have focus. A correct tree has at most one.
pub fn focused_nodes(tree: &Widget) -> Vec<&Widget> {
    fn walk<'a>(node: &'a Widget, out: &mut Vec<&'a Widget>) {
        if node.states.focused {
            out.push(node);
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}
