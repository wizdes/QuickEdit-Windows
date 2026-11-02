//! Font lookup and the DirectWrite objects that belong to each thread.
use crate::LayoutError;
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use windows::{
    Win32::Graphics::DirectWrite::*,
    core::{BOOL, Interface, PCWSTR},
};

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
pub(crate) fn dw(call: &'static str) -> impl Fn(windows::core::Error) -> LayoutError {
    move |e| LayoutError::DirectWrite {
        call,
        message: e.to_string(),
    }
}

static FACTORY: OnceLock<Result<IDWriteFactory, LayoutError>> = OnceLock::new();
pub(crate) fn factory() -> Result<&'static IDWriteFactory, LayoutError> {
    FACTORY
        .get_or_init(|| unsafe {
            DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).map_err(dw("DWriteCreateFactory"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

static FACES: OnceLock<Mutex<Vec<IDWriteFontFace>>> = OnceLock::new();
pub(crate) fn register_face(face: &IDWriteFontFace) -> Result<u32, LayoutError> {
    let mut faces = FACES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map_err(|_| LayoutError::Poisoned("font face registry"))?;
    if let Some(at) = faces.iter().position(|f| f.as_raw() == face.as_raw()) {
        return Ok(at as u32 + 1);
    }
    faces.push(face.clone());
    Ok(faces.len() as u32)
}
/// Resolves the `font_id` of a glyph run to its font face. The app uses this to
/// connect `qe-render`'s font registry to this crate. Returns `None` for an id
/// that this crate never gave out.
pub fn font_face(font_id: u32) -> Option<IDWriteFontFace> {
    FACES
        .get()?
        .lock()
        .ok()?
        .get(font_id.checked_sub(1)? as usize)
        .cloned()
}

/// A font at one size. It holds no COM text format, so it can be shared freely.
pub(crate) struct Font {
    pub(crate) uid: u64,
    pub(crate) id: u32,
    pub(crate) family: String,
    pub(crate) em_size: f32,
    pub(crate) weight: DWRITE_FONT_WEIGHT,
    pub(crate) ascii: [u16; 128],
    /// True when each printable ASCII glyph exists and takes exactly one cell.
    pub(crate) fast_ok: bool,
    pub(crate) advance: f32,
    pub(crate) advance_exact: f64,
    pub(crate) height: f32,
    pub(crate) baseline: f32,
}
#[derive(Clone, Eq, PartialEq, Hash)]
struct FontKey {
    family: String,
    size: u32,
    weight: i32,
}
static FONTS: OnceLock<Mutex<HashMap<FontKey, Arc<Font>>>> = OnceLock::new();
static NEXT_UID: AtomicU64 = AtomicU64::new(1);

/// How many fonts the process has loaded. Only installed families are ever
/// stored, so asking for missing names does not grow this.
#[doc(hidden)]
pub fn loaded_font_count() -> usize {
    FONTS
        .get()
        .and_then(|f| f.lock().ok().map(|f| f.len()))
        .unwrap_or(0)
}

fn fonts() -> Result<MutexGuard<'static, HashMap<FontKey, Arc<Font>>>, LayoutError> {
    FONTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| LayoutError::Poisoned("font cache"))
}

fn find_family(family: &str) -> Result<Option<(IDWriteFontCollection, u32)>, LayoutError> {
    let mut collection = None;
    unsafe {
        factory()?
            .GetSystemFontCollection(&mut collection, false)
            .map_err(dw("GetSystemFontCollection"))?;
    }
    let collection = collection.ok_or(LayoutError::NoFontCollection)?;
    let (mut index, mut exists) = (0, BOOL(0));
    unsafe {
        collection
            .FindFamilyName(PCWSTR(wide(family).as_ptr()), &mut index, &mut exists)
            .map_err(dw("FindFamilyName"))?;
    }
    Ok(exists.as_bool().then_some((collection, index)))
}

pub(crate) fn family_exists(family: &str) -> Result<bool, LayoutError> {
    Ok(find_family(family)?.is_some())
}

/// Finds a font. A family that is not installed is an error, and nothing is stored.
pub(crate) fn font(
    family: &str,
    em_size: f32,
    weight: DWRITE_FONT_WEIGHT,
) -> Result<Arc<Font>, LayoutError> {
    let key = FontKey {
        family: family.into(),
        size: em_size.to_bits(),
        weight: weight.0,
    };
    if let Some(f) = fonts()?.get(&key) {
        return Ok(f.clone());
    }
    let (collection, index) =
        find_family(family)?.ok_or_else(|| LayoutError::MissingFont(family.into()))?;
    let font_obj = unsafe {
        collection
            .GetFontFamily(index)
            .map_err(dw("GetFontFamily"))?
            .GetFirstMatchingFont(weight, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL)
            .map_err(dw("GetFirstMatchingFont"))?
    };
    let face = unsafe { font_obj.CreateFontFace().map_err(dw("CreateFontFace"))? };
    let monospaced = font_obj
        .cast::<IDWriteFont1>()
        .ok()
        .is_some_and(|f| unsafe { f.IsMonospacedFont().as_bool() });
    let points: Vec<u32> = (0..128).collect();
    let mut ascii = [0; 128];
    unsafe {
        face.GetGlyphIndices(points.as_ptr(), 128, ascii.as_mut_ptr())
            .map_err(dw("GetGlyphIndices"))?;
    }
    let mut metrics = DWRITE_FONT_METRICS::default();
    let mut glyphs = [DWRITE_GLYPH_METRICS::default(); 95];
    unsafe {
        face.GetMetrics(&mut metrics);
        face.GetDesignGlyphMetrics(ascii[32..127].as_ptr(), 95, glyphs.as_mut_ptr(), false)
            .map_err(dw("GetDesignGlyphMetrics"))?;
    }
    let advance_exact =
        glyphs[0].advanceWidth as f64 * em_size as f64 / metrics.designUnitsPerEm as f64;
    let fast_ok = monospaced
        && ascii[32..127].iter().all(|g| *g != 0)
        && glyphs
            .iter()
            .all(|g| g.advanceWidth == glyphs[0].advanceWidth);
    let mut made = Font {
        uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
        id: register_face(&face)?,
        family: family.into(),
        em_size,
        weight,
        ascii,
        fast_ok,
        advance: advance_exact as f32,
        advance_exact,
        height: 0.0,
        baseline: 0.0,
    };
    // The fast path must use the same row metrics as shaping, so take them from
    // a shaped line instead of working them out from the font tables.
    let sample: Vec<u16> = "x".encode_utf16().collect();
    let layout = made.create_layout(&sample, 1_000_000.0, 1_000_000.0)?;
    let mut line = [DWRITE_LINE_METRICS::default(); 1];
    let mut count = 0;
    unsafe {
        layout
            .GetLineMetrics(Some(&mut line), &mut count)
            .map_err(dw("GetLineMetrics"))?;
    }
    made.height = line[0].height;
    made.baseline = line[0].baseline;
    let made = Arc::new(made);
    Ok(fonts()?.entry(key).or_insert(made).clone())
}

thread_local! {
    /// Microsoft does not document `IDWriteTextFormat` as safe to share across
    /// threads, so each thread creates and keeps its own.
    static FORMATS: RefCell<HashMap<u64, IDWriteTextFormat>> = RefCell::new(HashMap::new());
    static NO_LIGATURES: RefCell<Option<IDWriteTypography>> = const { RefCell::new(None) };
}

impl Font {
    fn format(&self) -> Result<IDWriteTextFormat, LayoutError> {
        FORMATS.with(|cell| {
            if let Some(format) = cell.borrow().get(&self.uid) {
                return Ok(format.clone());
            }
            let format = unsafe {
                factory()?
                    .CreateTextFormat(
                        PCWSTR(wide(&self.family).as_ptr()),
                        None,
                        self.weight,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        self.em_size,
                        PCWSTR(wide("en-US").as_ptr()),
                    )
                    .map_err(dw("CreateTextFormat"))?
            };
            cell.borrow_mut().insert(self.uid, format.clone());
            Ok(format)
        })
    }
    pub(crate) fn create_layout(
        &self,
        text: &[u16],
        width: f32,
        height: f32,
    ) -> Result<IDWriteTextLayout, LayoutError> {
        let format = self.format()?;
        unsafe {
            factory()?
                .CreateTextLayout(text, &format, width, height)
                .map_err(dw("CreateTextLayout"))
        }
    }
}

/// Typography that turns off the default ligature features. Required ligatures
/// such as Arabic lam-alef use a different feature and stay on.
pub(crate) fn no_ligatures() -> Result<IDWriteTypography, LayoutError> {
    NO_LIGATURES.with(|cell| {
        if let Some(t) = cell.borrow().as_ref() {
            return Ok(t.clone());
        }
        let typography = unsafe {
            factory()?
                .CreateTypography()
                .map_err(dw("CreateTypography"))?
        };
        for tag in [
            DWRITE_FONT_FEATURE_TAG_STANDARD_LIGATURES,
            DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_LIGATURES,
            DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_ALTERNATES,
        ] {
            unsafe {
                typography
                    .AddFontFeature(DWRITE_FONT_FEATURE {
                        nameTag: tag,
                        parameter: 0,
                    })
                    .map_err(dw("AddFontFeature"))?;
            }
        }
        *cell.borrow_mut() = Some(typography.clone());
        Ok(typography)
    })
}
