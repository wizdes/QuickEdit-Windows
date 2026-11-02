use crate::{Dip, Point};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    X1,
    X2,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputKind {
    KeyDown {
        virtual_key: u16,
        modifiers: Modifiers,
        repeat: bool,
    },
    KeyUp {
        virtual_key: u16,
        modifiers: Modifiers,
    },
    Char(char),
    ImeStart,
    ImeUpdate {
        text: String,
        cursor_utf16: usize,
    },
    ImeEnd {
        text: String,
    },
    MouseMove {
        at: Point,
    },
    MouseDown {
        at: Point,
        button: MouseButton,
        modifiers: Modifiers,
    },
    MouseUp {
        at: Point,
        button: MouseButton,
        modifiers: Modifiers,
    },
    Wheel {
        at: Point,
        delta_x: f32,
        delta_y: f32,
        modifiers: Modifiers,
    },
    Focus,
    Blur,
    DpiChange {
        dpi: u32,
    },
    Resize {
        width: Dip,
        height: Dip,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputEvent {
    pub time_us: u64,
    pub kind: InputKind,
}
