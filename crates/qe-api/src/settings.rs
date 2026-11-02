#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Integer(i64),
    Text(&'static str),
}

macro_rules! settings {
    ($($name:ident => $default:expr;)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum SettingKey { $($name,)* }
        impl SettingKey {
            pub const ALL: &'static [Self] = &[$(Self::$name,)*];
            pub fn mac_default(self) -> SettingValue { match self { $(Self::$name => $default,)* } }
            pub const fn name(self) -> &'static str { match self { $(Self::$name => stringify!($name),)* } }
        }
    };
}
settings! {
    ShowPageBreaks => SettingValue::Bool(false);
    WindowWidthCharacters => SettingValue::Integer(90);
    WindowHeightLines => SettingValue::Integer(30);
    PlainTextFont => SettingValue::Text("System fixed-pitch 13 pt");
    KeyboardShortcuts => SettingValue::Text("");
    CheckSpellingWhileTyping => SettingValue::Bool(false);
    CorrectSpellingAutomatically => SettingValue::Bool(false);
    SmartCopyPaste => SettingValue::Bool(true);
    SmartQuotes => SettingValue::Bool(false);
    SmartDashes => SettingValue::Bool(false);
    TextReplacement => SettingValue::Text("system");
    ShowLineNumbers => SettingValue::Bool(false);
    ShowDocumentMap => SettingValue::Bool(true);
    AddTxtExtension => SettingValue::Bool(true);
    OpenEncoding => SettingValue::Text("Automatic");
    SaveEncoding => SettingValue::Text("Automatic");
    EnabledEncodings => SettingValue::Text("UTF-16,UTF-8,Mac Roman,CP1252,Mac Japanese,Shift JIS,Mac Chinese Traditional,Mac Korean,Mac Chinese Simplified,GB 18030");
    TabWidth => SettingValue::Integer(8);
    AutosavingDelaySeconds => SettingValue::Integer(30);
    NumberPagesWhenPrinting => SettingValue::Bool(false);
    WrapToFitWhenPrinting => SettingValue::Bool(true);
    PaperSize => SettingValue::Text("system: Letter in US/CA, A4 elsewhere");
}
