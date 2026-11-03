/// A key is a Windows virtual-key code plus modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyBinding {
    pub virtual_key: u16,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}
impl KeyBinding {
    pub const fn new(virtual_key: u16, ctrl: bool, alt: bool, shift: bool) -> Self {
        Self {
            virtual_key,
            ctrl,
            alt,
            shift,
        }
    }
    pub fn is_letter(self) -> bool {
        (b'A' as u16..=b'Z' as u16).contains(&self.virtual_key)
    }
}

macro_rules! commands {
    ($($name:ident => $path:literal, $key:expr;)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum CommandId { $($name,)* }
        impl CommandId {
            pub const ALL: &'static [Self] = &[$(Self::$name,)*];
            pub const fn menu_path(self) -> &'static str { match self { $(Self::$name => $path,)* } }
            pub const fn default_key(self) -> Option<KeyBinding> { match self { $(Self::$name => $key,)* } }
        }
    };
}
const fn ctrl(k: u16) -> Option<KeyBinding> {
    Some(KeyBinding::new(k, true, false, false))
}
const fn ctrl_shift(k: u16) -> Option<KeyBinding> {
    Some(KeyBinding::new(k, true, false, true))
}
const fn plain(k: u16) -> Option<KeyBinding> {
    Some(KeyBinding::new(k, false, false, false))
}
const fn shift(k: u16) -> Option<KeyBinding> {
    Some(KeyBinding::new(k, false, false, true))
}

commands! {
    New => "File/New", ctrl(0x4E);
    NewTab => "File/New Tab", ctrl(0x54);
    Open => "File/Open", ctrl(0x4F);
    OpenRecent => "File/Open Recent", None;
    ClearRecent => "File/Open Recent/Clear Menu", None;
    Close => "File/Close", ctrl(0x57);
    Save => "File/Save", ctrl(0x53);
    Duplicate => "File/Duplicate", ctrl_shift(0x53);
    SaveAs => "File/Save As", plain(0x7B);
    Rename => "File/Rename", None;
    MoveTo => "File/Move To", None;
    SaveAll => "File/Save All", None;
    RevertToSaved => "File/Revert to Saved", None;
    ExportPdf => "File/Export as PDF", None;
    Print => "File/Print", ctrl(0x50);
    Settings => "File/Settings", ctrl(0xBC);
    Undo => "Edit/Undo", ctrl(0x5A);
    Redo => "Edit/Redo", ctrl_shift(0x5A);
    Cut => "Edit/Cut", ctrl(0x58);
    Copy => "Edit/Copy", ctrl(0x43);
    Paste => "Edit/Paste", ctrl(0x56);
    Delete => "Edit/Delete", None;
    Complete => "Edit/Complete", ctrl(0x20);
    SelectAll => "Edit/Select All", ctrl(0x41);
    Find => "Edit/Find/Find", ctrl(0x46);
    FindReplace => "Edit/Find/Find and Replace", ctrl(0x48);
    FindNext => "Edit/Find/Find Next", plain(0x72);
    FindPrevious => "Edit/Find/Find Previous", shift(0x72);
    UseSelectionForFind => "Edit/Find/Use Selection for Find", ctrl(0x45);
    JumpToSelection => "Edit/Find/Jump to Selection", ctrl(0x4A);
    SelectLine => "Edit/Find/Select Line", ctrl(0x4C);
    ShowSpelling => "Edit/Spelling/Show Spelling", ctrl_shift(0xBA);
    CheckDocumentSpelling => "Edit/Spelling/Check Document Now", ctrl(0xBA);
    CheckSpellingWhileTyping => "Edit/Spelling/Check Spelling While Typing", None;
    CorrectSpellingAutomatically => "Edit/Spelling/Correct Spelling Automatically", None;
    ShowSubstitutions => "Edit/Substitutions/Show Substitutions", None;
    SmartCopyPaste => "Edit/Substitutions/Smart Copy/Paste", None;
    SmartQuotes => "Edit/Substitutions/Smart Quotes", None;
    SmartDashes => "Edit/Substitutions/Smart Dashes", None;
    UpperCase => "Edit/Transformations/Make Upper Case", None;
    LowerCase => "Edit/Transformations/Make Lower Case", None;
    Capitalize => "Edit/Transformations/Capitalize", None;
    PreventEditing => "Format/Prevent Editing", None;
    WrapToPage => "Format/Wrap to Page", ctrl_shift(0x57);
    ActualSize => "View/Actual Size", ctrl(0xBE);
    ZoomIn => "View/Zoom In", ctrl(0xBB);
    ZoomOut => "View/Zoom Out", ctrl(0xBD);
    ShowLineNumbers => "View/Show Line Numbers", None;
    ShowDocumentMap => "View/Show Document Map", None;
    FullScreen => "View/Enter Full Screen", plain(0x7A);
    Minimize => "Window/Minimize", ctrl(0x4D);
    ZoomWindow => "Window/Zoom", None;
    PreviousTab => "Window/Show Previous Tab", ctrl_shift(0x09);
    NextTab => "Window/Show Next Tab", ctrl(0x09);
    Tab1 => "Window/Tab 1", ctrl(0x31);
    Tab2 => "Window/Tab 2", ctrl(0x32);
    Tab3 => "Window/Tab 3", ctrl(0x33);
    Tab4 => "Window/Tab 4", ctrl(0x34);
    Tab5 => "Window/Tab 5", ctrl(0x35);
    Tab6 => "Window/Tab 6", ctrl(0x36);
    Tab7 => "Window/Tab 7", ctrl(0x37);
    Tab8 => "Window/Tab 8", ctrl(0x38);
    Tab9 => "Window/Tab 9", ctrl(0x39);
    LastTab => "Window/Last Tab", ctrl(0x30);
    BringAllToFront => "Window/Bring All to Front", None;
    MoveTabToNewWindow => "Window/Move Tab to New Window", None;
    MergeAllWindows => "Window/Merge All Windows", None;
}
