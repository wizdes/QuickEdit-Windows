//! What the tests share: a private folder for each test, a measurer, and real key order.
#![allow(dead_code)]
use qe_api::{
    Dip, DisplayList, Handled, InputEvent, InputKind, Modifiers, MouseButton, Point, Rect,
    TextMeasurer, TextMetrics, Theme, ThemeKind, View, ViewContext, WidgetId,
};
use qe_settings::{SettingsPanel, SettingsStore};
use std::path::PathBuf;

/// A folder of its own under the build's folder. The user's settings are never touched.
pub fn test_folder(name: &str) -> PathBuf {
    let folder = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("qe-settings")
        .join(name);
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).expect("make the test folder");
    folder
}
pub fn settings_path(name: &str) -> PathBuf {
    test_folder(name).join("settings.txt")
}

pub struct FixedMeasure;
impl TextMeasurer for FixedMeasure {
    fn measure(&self, text: &str, _: &str) -> TextMetrics {
        TextMetrics {
            width: Dip(text.chars().count() as f32 * 6.5),
            height: Dip(16.0),
            baseline: Dip(12.0),
        }
    }
}
pub const CTRL: Modifiers = Modifiers {
    ctrl: true,
    alt: false,
    shift: false,
    win: false,
};
pub const ALT: Modifiers = Modifiers {
    ctrl: false,
    alt: true,
    shift: false,
    win: false,
};
pub const SHIFT: Modifiers = Modifiers {
    ctrl: false,
    alt: false,
    shift: true,
    win: false,
};
pub const CTRL_SHIFT: Modifiers = Modifiers {
    ctrl: true,
    alt: false,
    shift: true,
    win: false,
};
pub const CTRL_ALT: Modifiers = Modifiers {
    ctrl: true,
    alt: true,
    shift: false,
    win: false,
};
pub const WIN: Modifiers = Modifiers {
    ctrl: false,
    alt: false,
    shift: false,
    win: true,
};
pub const NONE: Modifiers = Modifiers {
    ctrl: false,
    alt: false,
    shift: false,
    win: false,
};
pub const VK_CTRL: u16 = 0x11;
pub const VK_SHIFT: u16 = 0x10;
pub const VK_ALT: u16 = 0x12;
pub const VK_TAB: u16 = 0x09;
pub const VK_ENTER: u16 = 0x0d;
pub const VK_ESC: u16 = 0x1b;
pub const VK_SPACE: u16 = 0x20;
pub const VK_LEFT: u16 = 0x25;
pub const VK_RIGHT: u16 = 0x27;
pub const VK_UP: u16 = 0x26;
pub const VK_DOWN: u16 = 0x28;
pub const VK_DELETE: u16 = 0x2e;
pub const VK_BACK: u16 = 0x08;
pub const VK_F4: u16 = 0x73;
pub const VK_F5: u16 = 0x74;
pub const VK_F10: u16 = 0x79;

/// A panel in a dark theme's bounds, driven as Windows would drive it.
pub struct Rig {
    pub store: SettingsStore,
    pub panel: SettingsPanel,
    pub ctx: ViewContext,
    pub time_us: u64,
}
impl Rig {
    pub fn new(name: &str) -> Self {
        Self::with_store(SettingsStore::open(settings_path(name)))
    }
    pub fn with_store(store: SettingsStore) -> Self {
        let panel = SettingsPanel::new(WidgetId(0x1000), store.clone());
        let mut rig = Self {
            store,
            panel,
            ctx: ViewContext::default(),
            time_us: 1_000_000,
        };
        rig.lay_out();
        rig
    }
    /// Twice, as the window does: the first pass finds how tall the text makes the panel.
    pub fn lay_out(&mut self) {
        for _ in 0..2 {
            let height = self.panel.wanted_height();
            self.panel.layout(
                Rect {
                    x: Dip(0.0),
                    y: Dip(0.0),
                    width: Dip(440.0),
                    height: Dip(height),
                },
                &FixedMeasure,
            );
        }
    }
    pub fn send(&mut self, kind: InputKind) -> Handled {
        self.time_us += 1_000_000;
        let answer = self.panel.handle(
            &InputEvent {
                time_us: self.time_us,
                kind,
            },
            &mut self.ctx,
        );
        self.lay_out();
        answer
    }
    pub fn down(&mut self, virtual_key: u16, modifiers: Modifiers) -> Handled {
        self.send(InputKind::KeyDown {
            virtual_key,
            modifiers,
            repeat: false,
        })
    }
    pub fn held(&mut self, virtual_key: u16, modifiers: Modifiers) -> Handled {
        self.send(InputKind::KeyDown {
            virtual_key,
            modifiers,
            repeat: true,
        })
    }
    pub fn up(&mut self, virtual_key: u16, modifiers: Modifiers) -> Handled {
        self.send(InputKind::KeyUp {
            virtual_key,
            modifiers,
        })
    }
    /// A plain key: down, its character if it has one, up.
    pub fn tap(&mut self, virtual_key: u16) -> Handled {
        let answer = self.down(virtual_key, NONE);
        self.up(virtual_key, NONE);
        answer
    }
    pub fn tap_with(&mut self, virtual_key: u16, modifiers: Modifiers) -> Handled {
        let answer = self.down(virtual_key, modifiers);
        self.up(virtual_key, modifiers);
        answer
    }
    pub fn typed(&mut self, text: &str) {
        for ch in text.chars() {
            let key = ch.to_ascii_uppercase() as u16;
            self.down(key, NONE);
            self.send(InputKind::Char(ch));
            self.up(key, NONE);
        }
    }
    /// A chord in the order Windows sends it: modifiers down, key down, key up,
    /// modifiers up.
    pub fn chord(&mut self, virtual_key: u16, modifiers: Modifiers) {
        let mut held = NONE;
        if modifiers.ctrl {
            held.ctrl = true;
            self.down(VK_CTRL, held);
        }
        if modifiers.alt {
            held.alt = true;
            self.down(VK_ALT, held);
        }
        if modifiers.shift {
            held.shift = true;
            self.down(VK_SHIFT, held);
        }
        self.down(virtual_key, modifiers);
        self.up(virtual_key, modifiers);
        if modifiers.shift {
            held.shift = false;
            self.up(VK_SHIFT, held);
        }
        if modifiers.alt {
            held.alt = false;
            self.up(VK_ALT, held);
        }
        if modifiers.ctrl {
            held.ctrl = false;
            self.up(VK_CTRL, held);
        }
    }
    pub fn click(&mut self, x: f32, y: f32) {
        let at = Point {
            x: Dip(x),
            y: Dip(y),
        };
        self.send(InputKind::MouseMove { at });
        self.send(InputKind::MouseDown {
            at,
            button: MouseButton::Left,
            modifiers: NONE,
        });
        self.send(InputKind::MouseUp {
            at,
            button: MouseButton::Left,
            modifiers: NONE,
        });
    }
    /// Tab until the control named `name` has the keyboard.
    pub fn tab_to(&mut self, name: &str) {
        for _ in 0..64 {
            if self.focused().as_deref() == Some(name) {
                return;
            }
            self.tap(VK_TAB);
        }
        panic!("Tab never reached {name:?}");
    }
    pub fn focused(&self) -> Option<String> {
        self.panel.focused_name()
    }
    pub fn go_to_tab(&mut self, tab: usize) {
        self.panel.select_tab(tab, &mut self.ctx);
        self.lay_out();
    }
    pub fn go_to_keyboard_table(&mut self) {
        self.go_to_tab(qe_settings::TAB_KEYBOARD);
        self.tab_to("Keyboard shortcuts");
    }
    pub fn list(&self, theme: &Theme) -> DisplayList {
        let mut list = DisplayList::default();
        self.panel.draw(&mut list, theme, &FixedMeasure);
        list
    }
    pub fn light(&self) -> DisplayList {
        self.list(&Theme::new(ThemeKind::Light))
    }
    pub fn recording(&self) -> bool {
        self.panel.table().is_some_and(|t| t.is_recording())
    }
    /// Starts recording a command from the keyboard, as a user does.
    pub fn record(&mut self, command: qe_api::CommandId) {
        self.go_to_keyboard_table();
        for _ in 0..200 {
            if self.panel.table().and_then(|t| t.selected_command()) == Some(command) {
                self.tap(VK_ENTER);
                assert!(self.recording(), "Enter starts the recording");
                return;
            }
            self.tap(VK_DOWN);
        }
        panic!("no row for {command:?}");
    }
}
/// All the text the list paints, in order.
pub fn painted_text(list: &DisplayList) -> Vec<String> {
    list.commands
        .iter()
        .filter_map(|command| match command {
            qe_api::DrawCommand::TextRun { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}
