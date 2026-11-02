//! The checker, behind a small trait. All of it runs on the worker thread.
//!
//! The Windows factory's `ThreadingModel` is `Free` in the registry. So the worker joins the
//! multithreaded apartment, where it can use the object directly, with no proxy and no need
//! to pump messages. A single-threaded apartment would need a message loop on the worker.
use windows::{
    Win32::{
        Globalization::{
            CORRECTIVE_ACTION_DELETE, CORRECTIVE_ACTION_GET_SUGGESTIONS, CORRECTIVE_ACTION_REPLACE,
            GetUserDefaultLocaleName, GetUserPreferredUILanguages, ISpellChecker,
            ISpellCheckerFactory, MUI_LANGUAGE_NAME, SpellCheckerFactory,
        },
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IEnumString},
    },
    core::{PCWSTR, PWSTR},
};

/// What the checker wants done about an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Ask for suggestions.
    Suggest,
    /// One sure replacement.
    Replace(String),
    /// A doubled word. The Mac's spelling leaves these alone.
    Delete,
    None,
}

/// One error, in UTF-16 units of the text that was checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawError {
    pub start: usize,
    pub length: usize,
    pub action: Action,
}

/// What the worker needs from a spell checker. `text` ends with a 0 unit.
pub trait Checker {
    fn check(&mut self, text: &[u16]) -> Result<Vec<RawError>, String>;
    fn suggest(&mut self, word: &str, most: usize) -> Result<Vec<String>, String>;
    fn add(&mut self, word: &str) -> Result<(), String>;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The user's language, such as `en-US`, or `None` when Windows gives none.
pub fn default_language() -> Option<String> {
    let mut name = [0_u16; 85];
    // SAFETY: the buffer outlives the call, and its length goes with it.
    let length = unsafe { GetUserDefaultLocaleName(&mut name) };
    let length = usize::try_from(length).ok()?.checked_sub(1)?;
    Some(without_sort_order(&String::from_utf16_lossy(
        name.get(..length)?,
    )))
}

/// `de-DE_phoneb` names a sort order as well as a language. A dictionary's tag never does.
pub fn without_sort_order(tag: &str) -> String {
    tag.split('_').next().unwrap_or(tag).to_owned()
}

/// The languages the user chose for Windows' own display, best first.
fn display_languages() -> Vec<String> {
    let (mut count, mut size) = (0, 0);
    // SAFETY: both out values live for the call, and asking for the size takes no buffer.
    let asked =
        unsafe { GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut size) };
    if asked.is_err() || size == 0 {
        return Vec::new();
    }
    let mut names = vec![0_u16; size as usize];
    // SAFETY: the buffer has the size that the call above asked for.
    let read = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(names.as_mut_ptr())),
            &mut size,
        )
    };
    if read.is_err() {
        return Vec::new();
    }
    names
        .split(|unit| *unit == 0)
        .filter(|name| !name.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// The dictionary to use from the ones Windows has: the tag itself, then the first with the
/// same language (`en-CA` finds `en-US`), then the same for each of Windows' display languages.
pub fn choose_language(wanted: &str, display: &[String], supported: &[String]) -> Option<String> {
    let same_language = |one: &str, tag: &str| one.split('-').next() == tag.split('-').next();
    let wanted = without_sort_order(wanted);
    std::iter::once(wanted.as_str())
        .chain(display.iter().map(String::as_str))
        .find_map(|tag| {
            supported
                .iter()
                .find(|one| one.eq_ignore_ascii_case(tag))
                .or_else(|| supported.iter().find(|one| same_language(one, tag)))
                .cloned()
        })
}

/// Windows' `ISpellChecker`.
pub struct WindowsChecker {
    checker: ISpellChecker,
}
impl WindowsChecker {
    /// Loads the checker on this thread, which must be in a COM apartment.
    pub fn new(language: Option<&str>) -> Result<Self, String> {
        let unavailable =
            |e: windows::core::Error| format!("Windows spelling is not available: {e}");
        // SAFETY: plain COM calls on a thread in an apartment, with strings that outlive them.
        unsafe {
            let factory: ISpellCheckerFactory =
                CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(unavailable)?;
            let wanted = language
                .map(str::to_owned)
                .or_else(default_language)
                .unwrap_or_else(|| "en-US".to_owned());
            // A language the caller named is that language. Only the user's default may fall
            // back to Windows' display languages.
            let display = if language.is_some() {
                Vec::new()
            } else {
                display_languages()
            };
            let tag = pick_language(&factory, &wanted, &display)
                .map_err(unavailable)?
                .ok_or_else(|| {
                    format!(
                        "Windows has no spelling dictionary for {wanted}. Add this language in \
                     Settings, under Time & language."
                    )
                })?;
            let tag = wide(&tag);
            let checker = factory
                .CreateSpellChecker(PCWSTR(tag.as_ptr()))
                .map_err(unavailable)?;
            Ok(Self { checker })
        }
    }
    /// Whether Windows has a dictionary for this tag. Loads the checker, so tests call this.
    pub fn is_supported(language: &str) -> Result<bool, String> {
        // SAFETY: plain COM calls on a thread in an apartment, with strings that outlive them.
        unsafe {
            let factory: ISpellCheckerFactory =
                CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| e.to_string())?;
            let tag = wide(language);
            factory
                .IsSupported(PCWSTR(tag.as_ptr()))
                .map(|b| b.as_bool())
                .map_err(|e| e.to_string())
        }
    }
}

/// The dictionary for this tag, with what `choose_language` allows.
unsafe fn pick_language(
    factory: &ISpellCheckerFactory,
    wanted: &str,
    display: &[String],
) -> windows::core::Result<Option<String>> {
    // SAFETY: the caller is on a COM thread.
    let languages = unsafe { factory.SupportedLanguages() }?;
    let supported = strings(&languages, usize::MAX)?;
    Ok(choose_language(wanted, display, &supported))
}

fn strings(values: &IEnumString, most: usize) -> windows::core::Result<Vec<String>> {
    let mut out = Vec::new();
    while out.len() < most {
        let mut item = [PWSTR::null()];
        // SAFETY: a live enumerator, and `item` has room for the one that it asks for.
        let status = unsafe { values.Next(&mut item, None) };
        if status.0 == 1 {
            break;
        }
        status.ok()?;
        if item[0].is_null() {
            break;
        }
        // SAFETY: the enumerator gives a string for the caller to free.
        let text = unsafe { item[0].to_string() };
        unsafe { CoTaskMemFree(Some(item[0].0.cast())) };
        out.push(
            text.map_err(|_| {
                windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)
            })?,
        );
    }
    Ok(out)
}

impl Checker for WindowsChecker {
    fn check(&mut self, text: &[u16]) -> Result<Vec<RawError>, String> {
        // Windows reads to the 0, so text with none would have it read past the end.
        if text.last() != Some(&0) {
            return Err("The spell check needs text that ends with a 0.".into());
        }
        let failed = |e: windows::core::Error| format!("The spell check failed: {e}");
        // SAFETY: `text` ends with 0 and outlives the call, as `Checker::check` requires.
        let errors = unsafe { self.checker.Check(PCWSTR(text.as_ptr())) }.map_err(failed)?;
        let mut out = Vec::new();
        loop {
            let mut error = None;
            // SAFETY: a live enumerator, with room for one.
            let status = unsafe { errors.Next(&mut error) };
            if status.0 == 1 {
                break;
            }
            status.ok().map_err(failed)?;
            let Some(error) = error else { break };
            // SAFETY: a live error, whose replacement string is the caller's to free.
            unsafe {
                let action = match error.CorrectiveAction().map_err(failed)? {
                    CORRECTIVE_ACTION_GET_SUGGESTIONS => Action::Suggest,
                    CORRECTIVE_ACTION_REPLACE => {
                        let replacement = error.Replacement().map_err(failed)?;
                        let text = replacement.to_string();
                        CoTaskMemFree(Some(replacement.0.cast()));
                        Action::Replace(text.map_err(|e| e.to_string())?)
                    }
                    CORRECTIVE_ACTION_DELETE => Action::Delete,
                    _ => Action::None,
                };
                out.push(RawError {
                    start: error.StartIndex().map_err(failed)? as usize,
                    length: error.Length().map_err(failed)? as usize,
                    action,
                });
            }
        }
        Ok(out)
    }
    fn suggest(&mut self, word: &str, most: usize) -> Result<Vec<String>, String> {
        let text = wide(word);
        // SAFETY: `text` ends with 0 and outlives the call.
        let values = unsafe { self.checker.Suggest(PCWSTR(text.as_ptr())) }
            .map_err(|e| format!("Windows gave no suggestions: {e}"))?;
        strings(&values, most).map_err(|e| format!("Windows gave no suggestions: {e}"))
    }
    fn add(&mut self, word: &str) -> Result<(), String> {
        let text = wide(word);
        // SAFETY: `text` ends with 0 and outlives the call.
        unsafe { self.checker.Add(PCWSTR(text.as_ptr())) }
            .map_err(|e| format!("Could not add \"{word}\" to the dictionary: {e}"))
    }
}
