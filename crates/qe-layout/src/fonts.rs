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

/// Every font face this process has drawn with. It holds one entry per installed face
/// in use, whatever the sizes, so the installed fonts bound it.
#[derive(Default)]
struct FaceRegistry {
    /// Holds a reference to each face object, so that its pointer stays a true key.
    by_pointer: HashMap<usize, (u32, IDWriteFontFace)>,
    by_id: HashMap<u32, (String, IDWriteFontFace)>,
}
static FACES: OnceLock<Mutex<FaceRegistry>> = OnceLock::new();

/// The parts of a font file's `head` table that name its data: its revision, the checksum of
/// the whole file, and the date it was made. Reading them costs one table lookup, and they
/// are the same on every machine and in any load order. A font with no such table gets "none".
fn head_facts(head: Option<&[u8]>) -> String {
    let part = |range: std::ops::Range<usize>| head.and_then(|bytes| bytes.get(range));
    match (part(4..12), part(28..36)) {
        (Some(revision_and_sum), Some(modified)) => {
            let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
            format!("{}{}", hex(revision_and_sum), hex(modified))
        }
        _ => "none".into(),
    }
}

/// Reads the font file's `head` table for `head_facts`.
fn face_data_facts(face: &IDWriteFontFace) -> String {
    let (mut data, mut size, mut context, mut exists) =
        (std::ptr::null_mut(), 0u32, std::ptr::null_mut(), BOOL(0));
    let tag = u32::from_le_bytes(*b"head");
    let read =
        unsafe { face.TryGetFontTable(tag, &mut data, &mut size, &mut context, &mut exists) };
    if read.is_err() || !exists.as_bool() || data.is_null() {
        return head_facts(None);
    }
    let facts = head_facts(Some(unsafe {
        std::slice::from_raw_parts(data.cast::<u8>(), size as usize)
    }));
    unsafe { face.ReleaseFontTable(context) };
    facts
}

/// A variable font's axis values, which pick one instance from one file. Two weights of a
/// file have the same `head` table, so without these they would share one id and draw alike.
fn axis_facts(face: &IDWriteFontFace) -> String {
    let Ok(face) = face.cast::<IDWriteFontFace5>() else {
        return String::new();
    };
    let count = unsafe { face.GetFontAxisValueCount() } as usize;
    let mut values = vec![DWRITE_FONT_AXIS_VALUE::default(); count];
    if count == 0 || unsafe { face.GetFontAxisValues(&mut values) }.is_err() {
        return String::new();
    }
    values
        .iter()
        .map(|axis| format!("{:x}={}", axis.axisTag.0, axis.value))
        .collect::<Vec<_>>()
        .join(",")
}

/// Names a face by what it is, never by when it loaded: by the data of its font file, its
/// place in that file, its variable-font instance, and what is simulated on it.
fn face_key(face: &IDWriteFontFace) -> Result<String, LayoutError> {
    let simulations = unsafe { face.GetSimulations().0 };
    let data = format!("{}|{}", axis_facts(face), face_data_facts(face));
    let number = unsafe { face.GetIndex() };
    let mut collection = None;
    unsafe {
        factory()?
            .GetSystemFontCollection(&mut collection, false)
            .map_err(dw("GetSystemFontCollection"))?;
    }
    let collection = collection.ok_or(LayoutError::NoFontCollection)?;
    // A face outside the system collection has no family name to give. Its shape still
    // and data say what it is, and those do not depend on load order either.
    let Ok(font) = (unsafe { collection.GetFontFromFontFace(face) }) else {
        let mut metrics = DWRITE_FONT_METRICS::default();
        unsafe { face.GetMetrics(&mut metrics) };
        let glyphs = unsafe { face.GetGlyphCount() };
        return Ok(format!(
            "unnamed|{glyphs}|{}|{}|{simulations}|{number}|{data}",
            metrics.designUnitsPerEm, metrics.ascent
        ));
    };
    let names = unsafe {
        font.GetFontFamily()
            .map_err(dw("GetFontFamily"))?
            .GetFamilyNames()
            .map_err(dw("GetFamilyNames"))?
    };
    let (mut index, mut exists) = (0, BOOL(0));
    unsafe {
        names
            .FindLocaleName(PCWSTR(wide("en-us").as_ptr()), &mut index, &mut exists)
            .map_err(dw("FindLocaleName"))?;
    }
    let index = if exists.as_bool() { index } else { 0 };
    let length = unsafe {
        names
            .GetStringLength(index)
            .map_err(dw("GetStringLength"))?
    };
    let mut name = vec![0u16; length as usize + 1];
    unsafe {
        names.GetString(index, &mut name).map_err(dw("GetString"))?;
    }
    name.truncate(length as usize);
    let (weight, stretch, style) =
        unsafe { (font.GetWeight().0, font.GetStretch().0, font.GetStyle().0) };
    // Windows and Windows Terminal each install a font with this same name and style, with
    // different glyphs. The file's own data keeps them apart, or a run would draw the wrong
    // letters. The glyph count alone would miss two files that happen to have the same count.
    let glyphs = unsafe { face.GetGlyphCount() };
    Ok(format!(
        "{}|{weight}|{stretch}|{style}|{simulations}|{glyphs}|{number}|{data}",
        String::from_utf16_lossy(&name)
    ))
}

/// FNV-1a. It must stay the same across processes and Rust versions, so it cannot be
/// the standard library's hasher. Zero is left free to mean "no font".
fn stable_id(key: &str) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for byte in key.bytes() {
        hash = (hash ^ byte as u32).wrapping_mul(0x0100_0193);
    }
    hash.max(1)
}

/// The face's id. It comes from the face's family, weight, stretch, style, simulations,
/// glyph count, place in its file, axis values and the file's `head` facts, so it names the font's real
/// data, and the stable display text does not depend on which font or which thread loaded
/// first. Two different faces with the same hash would take ids in load
/// order, which is the one case that still depends on order. With a few dozen faces in
/// use, that chance is about one in ten million.
pub(crate) fn register_face(face: &IDWriteFontFace) -> Result<u32, LayoutError> {
    let mut faces = FACES
        .get_or_init(|| Mutex::new(FaceRegistry::default()))
        .lock()
        .map_err(|_| LayoutError::Poisoned("font face registry"))?;
    let pointer = face.as_raw() as usize;
    if let Some((id, _)) = faces.by_pointer.get(&pointer) {
        return Ok(*id);
    }
    let key = face_key(face)?;
    let mut id = stable_id(&key);
    loop {
        match faces.by_id.get(&id) {
            Some((taken, _)) if *taken == key => break,
            Some(_) => id = id.wrapping_add(1).max(1),
            None => {
                faces.by_id.insert(id, (key, face.clone()));
                break;
            }
        }
    }
    faces.by_pointer.insert(pointer, (id, face.clone()));
    Ok(id)
}
/// Resolves the `font_id` of a glyph run to its font face. The app uses this to
/// connect `qe-render`'s font registry to this crate. Returns `None` for an id
/// that this crate never gave out.
pub fn font_face(font_id: u32) -> Option<IDWriteFontFace> {
    let faces = FACES.get()?.lock().ok()?;
    faces.by_id.get(&font_id).map(|(_, face)| face.clone())
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

/// The most fonts kept per process, and the most text formats kept per thread. Past
/// this, each cache starts over, so that a slider through many sizes stays bounded. A
/// worker thread's formats go when the thread ends.
pub const MAX_LOADED_FONTS: usize = 64;

/// How many fonts the process has loaded. Only installed families are ever
/// stored, so asking for missing names does not grow this.
#[doc(hidden)]
pub fn loaded_font_count() -> usize {
    FONTS
        .get()
        .and_then(|f| f.lock().ok().map(|f| f.len()))
        .unwrap_or(0)
}

/// How many face objects the registry holds.
#[doc(hidden)]
pub fn registered_face_count() -> usize {
    FACES
        .get()
        .and_then(|f| f.lock().ok().map(|f| f.by_pointer.len()))
        .unwrap_or(0)
}

/// How many text formats this thread holds.
#[doc(hidden)]
pub fn thread_format_count() -> usize {
    FORMATS.with(|cell| cell.borrow().len())
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
    let mut loaded = fonts()?;
    if loaded.len() >= MAX_LOADED_FONTS && !loaded.contains_key(&key) {
        // Engines keep the fonts they hold. Asking again only costs a new load.
        loaded.clear();
    }
    Ok(loaded.entry(key).or_insert(made).clone())
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
            let mut formats = cell.borrow_mut();
            if formats.len() >= MAX_LOADED_FONTS {
                formats.clear();
            }
            formats.insert(self.uid, format.clone());
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

#[cfg(test)]
mod face_key_tests {
    use super::*;

    fn head(fill: u8) -> Vec<u8> {
        vec![fill; 54]
    }

    #[test]
    fn the_key_changes_with_the_files_revision_checksum_or_date() {
        let base = head_facts(Some(&head(1)));
        for at in [4, 8, 28, 35] {
            let mut other = head(1);
            other[at] = 2;
            assert_ne!(head_facts(Some(&other)), base, "byte {at}");
        }
        // Other bytes of the table say nothing about which file this is.
        let mut same = head(1);
        same[0] = 9;
        same[40] = 9;
        assert_eq!(head_facts(Some(&same)), base);
    }

    #[test]
    fn a_font_with_a_missing_or_short_table_still_has_a_key() {
        assert_eq!(head_facts(None), "none");
        assert_eq!(head_facts(Some(&[0; 20])), "none");
    }

    #[test]
    fn a_face_gets_the_same_key_each_time_it_is_asked() {
        let mut collection = None;
        unsafe {
            factory()
                .unwrap()
                .GetSystemFontCollection(&mut collection, false)
                .unwrap();
        }
        let collection = collection.unwrap();
        let family = unsafe { collection.GetFontFamily(0).unwrap() };
        let font = unsafe {
            family
                .GetFirstMatchingFont(
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                )
                .unwrap()
        };
        let face = unsafe { font.CreateFontFace().unwrap() };
        let key = face_key(&face).unwrap();
        assert_eq!(key, face_key(&face).unwrap());
        let data = face_data_facts(&face);
        assert_ne!(data, "none");
        assert!(key.ends_with(&data), "{key}");
    }

    #[test]
    fn two_weights_of_one_variable_font_get_two_ids() {
        let mut collection = None;
        unsafe {
            factory()
                .unwrap()
                .GetSystemFontCollection(&mut collection, false)
                .unwrap();
        }
        let collection = collection.unwrap();
        let (mut index, mut exists) = (0, BOOL(0));
        unsafe {
            collection
                .FindFamilyName(
                    PCWSTR(wide("Cascadia Mono").as_ptr()),
                    &mut index,
                    &mut exists,
                )
                .unwrap();
        }
        assert!(exists.as_bool(), "Windows 11 includes Cascadia Mono");
        let font = unsafe {
            collection
                .GetFontFamily(index)
                .unwrap()
                .GetFirstMatchingFont(
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                )
                .unwrap()
        };
        let face: IDWriteFontFace5 = unsafe { font.CreateFontFace().unwrap() }.cast().unwrap();
        let resource = unsafe { face.GetFontResource().unwrap() };
        let instance = |weight: f32| -> IDWriteFontFace {
            let axis = DWRITE_FONT_AXIS_VALUE {
                axisTag: DWRITE_FONT_AXIS_TAG(u32::from_le_bytes(*b"wght")),
                value: weight,
            };
            unsafe {
                resource
                    .CreateFontFace(DWRITE_FONT_SIMULATIONS_NONE, &[axis])
                    .unwrap()
                    .cast()
                    .unwrap()
            }
        };
        let (light, heavy) = (instance(325.0), instance(375.0));
        let (light_id, heavy_id) = (
            register_face(&light).unwrap(),
            register_face(&heavy).unwrap(),
        );
        assert_ne!(light_id, heavy_id);
        // Each id gives back its own instance, so a run draws with the weight it asked for.
        let weight_of = |id: u32| axis_facts(&font_face(id).unwrap());
        assert_eq!(weight_of(light_id), axis_facts(&light));
        assert_eq!(weight_of(heavy_id), axis_facts(&heavy));
        assert_ne!(axis_facts(&light), axis_facts(&heavy));
    }
}
