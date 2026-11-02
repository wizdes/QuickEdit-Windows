macro_rules! encodings {
    ($($variant:ident => $name:literal,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum TextEncoding { $($variant,)* }
        impl TextEncoding {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];
            pub const fn name(self) -> &'static str {
                match self { $(Self::$variant => $name,)* }
            }
        }
    };
}

encodings! {
    Utf8 => "UTF-8",
    Utf16 => "UTF-16",
    Utf16Le => "UTF-16 LE",
    Utf16Be => "UTF-16 BE",
    Utf32 => "UTF-32",
    Utf32Le => "UTF-32 LE",
    Utf32Be => "UTF-32 BE",
    Windows1250 => "Windows-1250",
    Windows1251 => "Windows-1251",
    Windows1252 => "Windows-1252",
    Windows1253 => "Windows-1253",
    Windows1254 => "Windows-1254",
    Windows1255 => "Windows-1255",
    Windows1256 => "Windows-1256",
    Windows1257 => "Windows-1257",
    Windows1258 => "Windows-1258",
    ShiftJis => "Shift JIS (932)",
    Gbk => "GBK (936)",
    Gb18030 => "GB 18030",
    Big5 => "Big5 (950)",
    Korean949 => "Korean (949)",
    EucJp => "EUC-JP",
    Iso8859_1 => "ISO-8859-1",
    Iso8859_2 => "ISO-8859-2",
    Iso8859_3 => "ISO-8859-3",
    Iso8859_4 => "ISO-8859-4",
    Iso8859_5 => "ISO-8859-5",
    Iso8859_6 => "ISO-8859-6",
    Iso8859_7 => "ISO-8859-7",
    Iso8859_8 => "ISO-8859-8",
    Iso8859_9 => "ISO-8859-9",
    Iso8859_10 => "ISO-8859-10",
    Iso8859_11 => "ISO-8859-11",
    Iso8859_13 => "ISO-8859-13",
    Iso8859_14 => "ISO-8859-14",
    Iso8859_15 => "ISO-8859-15",
    Iso8859_16 => "ISO-8859-16",
    Koi8R => "KOI8-R",
    MacRoman => "Mac Roman",
    MacJapanese => "Mac Japanese",
    MacChineseTraditional => "Mac Chinese Traditional",
    MacKorean => "Mac Korean",
    MacChineseSimplified => "Mac Chinese Simplified",
}

impl TextEncoding {
    /// The enabled list in the Mac app's default settings.
    pub const DEFAULT_LIST: &'static [Self] = &[
        Self::Utf16,
        Self::Utf8,
        Self::MacRoman,
        Self::Windows1252,
        Self::MacJapanese,
        Self::ShiftJis,
        Self::MacChineseTraditional,
        Self::MacKorean,
        Self::MacChineseSimplified,
        Self::Gb18030,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EncodingChoice {
    Automatic,
    Specific(TextEncoding),
}
