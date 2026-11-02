use std::fmt;

/// A problem the window met. Each one reaches the owner as `ShellEvent::Problem` and
/// stays listed in `ShellHandle::problems`, so that nothing fails silently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellError {
    /// A renderer call failed. `stage` names the call, such as `draw` or `present_rect`.
    Renderer {
        stage: &'static str,
        message: String,
    },
    /// A Win32 call failed, with its error text.
    Win32 { call: &'static str, message: String },
    /// A DWM call failed. The frame may look wrong, and the window still works.
    Dwm { call: &'static str, message: String },
    /// The theme value could not be read, so the window uses the light theme.
    ThemeRead { status: u32 },
    /// A message arrived while the window's state was in use, so it took the default
    /// path. An owner's `with_strip` closure must not run a dialog or a message loop.
    Reentered { message: u32 },
    /// `WM_CHAR` gave half of a surrogate pair, with no other half. It was dropped.
    LoneSurrogate { unit: u16 },
    /// The window no longer exists.
    WindowGone,
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Renderer { stage, message } => write!(f, "renderer {stage}: {message}"),
            Self::Win32 { call, message } => write!(f, "{call}: {message}"),
            Self::Dwm { call, message } => write!(f, "DWM {call}: {message}"),
            Self::ThemeRead { status } => {
                write!(
                    f,
                    "reading the theme failed with status {status}; using light"
                )
            }
            Self::Reentered { message } => {
                write!(f, "message {message:#x} arrived while the state was in use")
            }
            Self::LoneSurrogate { unit } => {
                write!(f, "WM_CHAR gave a lone surrogate {unit:#x}; it was dropped")
            }
            Self::WindowGone => f.write_str("the window no longer exists"),
        }
    }
}

impl std::error::Error for ShellError {}
