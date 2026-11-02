//! Writes null over a method's out value before the method runs.
//!
//! UIA's "none" answer is S_OK with a null object. The `windows` crate's generated glue can only
//! write a real object, and it leaves the out value alone for anything else. A caller may pass
//! that value uninitialized, so a caller that trusts S_OK would then use whatever was there.
//!
//! So each object that can answer "none" gets a copy of its generated method table, with those
//! methods wrapped. The wrapper clears the out value, then calls the generated method. A method
//! table is per type and per interface, because the glue inside it knows its place in the object.
//!
//! The wrapper also protects the caller's arguments. COM's rule is that an `[in]` value stays the
//! caller's, but the generated `FindAttribute` takes a `VARIANT` by value, and that type's `Drop`
//! clears it when the glue returns. That would free a string or release an object that the caller
//! still owns. So the glue gets its own copy to clear, and the caller's value is never dropped
//! here. Of the interfaces this crate implements, `FindAttribute` is the only method with a
//! `VARIANT` by value. `FindText` gets its `BSTR` as a plain pointer, which has no `Drop`.
#![allow(non_snake_case)]
use std::{ffi::c_void, mem::ManuallyDrop, ptr, sync::OnceLock};
use windows::{
    Win32::{
        Foundation::E_POINTER,
        System::Variant::{VARIANT, VariantCopy},
        UI::Accessibility::*,
    },
    core::{BOOL, HRESULT, Interface},
};

/// An argument on its way from the caller, who owns it, to the generated glue.
trait Incoming: Sized {
    /// What the glue may treat as its own, or the error that stops the call.
    fn for_glue(held: &Self) -> Result<Self, HRESULT>;
}
macro_rules! plain_incoming {
    ($( $kind:ty ),+ $(,)?) => {
        $( impl Incoming for $kind {
            fn for_glue(held: &Self) -> Result<Self, HRESULT> {
                Ok(*held)
            }
        } )+
    };
}
plain_incoming!(
    *mut c_void,
    BOOL,
    UIA_TEXTATTRIBUTE_ID,
    UIA_PATTERN_ID,
    NavigateDirection
);
impl Incoming for VARIANT {
    fn for_glue(held: &Self) -> Result<Self, HRESULT> {
        let mut copy = Self::default();
        // SAFETY: both point at live values, and `copy` is empty.
        unsafe { VariantCopy(&mut copy, held) }.map_err(|error| error.code())?;
        Ok(copy)
    }
}

/// Puts `table` in the interface's slot of a new object. Call it before anyone else can see the
/// object.
fn install<I: Interface>(interface: &I, table: *const I::Vtable) {
    // SAFETY: an interface pointer is the address of its object's table slot, a plain field of
    // the object's own heap memory. The object is new, so no other thread reads that slot yet.
    unsafe { *interface.as_raw().cast::<*const I::Vtable>() = table };
}

macro_rules! guard {
    ($module:ident, $interface:ty, $table:ty, $( $method:ident ( $( $arg:ident : $kind:ty ),* ) ),+ $(,)?) => {
        pub(crate) mod $module {
            use super::*;
            /// The generated table, then the copy with wrapped methods.
            static TABLES: OnceLock<($table, $table)> = OnceLock::new();
            fn original() -> &'static $table {
                // The wrapper only runs from a table that `install` put there.
                match TABLES.get() {
                    Some(tables) => &tables.0,
                    None => std::process::abort(),
                }
            }
            $(
                unsafe extern "system" fn $method(
                    this: *mut c_void,
                    $( $arg: $kind, )*
                    out: *mut *mut c_void,
                ) -> HRESULT {
                    // These stay the caller's on every path, so none of them drops here.
                    $( let $arg = ManuallyDrop::new($arg); )*
                    if out.is_null() {
                        return E_POINTER;
                    }
                    // SAFETY: the caller gave this pointer for the answer.
                    unsafe { out.write(ptr::null_mut()) };
                    $(
                        let $arg = match Incoming::for_glue(&*$arg) {
                            Ok(value) => value,
                            Err(code) => return code,
                        };
                    )*
                    // SAFETY: the glue gets what its method table asks for.
                    unsafe { (original().$method)(this, $( $arg, )* out) }
                }
            )+
            pub(crate) fn protect(interface: &$interface) {
                let slot = interface.as_raw().cast::<*const $table>();
                let tables = TABLES.get_or_init(|| {
                    // SAFETY: a method table is a plain struct of function pointers, and the
                    // first object to come here still has its generated table.
                    let (original, mut copy) = unsafe { (ptr::read(*slot), ptr::read(*slot)) };
                    $( copy.$method = $method; )+
                    (original, copy)
                });
                install(interface, &tables.1);
            }
        }
    };
}

guard!(
    range,
    ITextRangeProvider,
    ITextRangeProvider_Vtbl,
    FindText(text: *mut c_void, backward: BOOL, ignore_case: BOOL),
    FindAttribute(id: UIA_TEXTATTRIBUTE_ID, value: VARIANT, backward: BOOL),
);
guard!(
    node_simple,
    IRawElementProviderSimple,
    IRawElementProviderSimple_Vtbl,
    GetPatternProvider(id: UIA_PATTERN_ID),
    HostRawElementProvider(),
);
guard!(
    node_fragment,
    IRawElementProviderFragment,
    IRawElementProviderFragment_Vtbl,
    Navigate(direction: NavigateDirection),
);
guard!(
    node_selection_item,
    ISelectionItemProvider,
    ISelectionItemProvider_Vtbl,
    SelectionContainer(),
);
guard!(
    root_simple,
    IRawElementProviderSimple,
    IRawElementProviderSimple_Vtbl,
    GetPatternProvider(id: UIA_PATTERN_ID),
    HostRawElementProvider(),
);
guard!(
    root_fragment,
    IRawElementProviderFragment,
    IRawElementProviderFragment_Vtbl,
    Navigate(direction: NavigateDirection),
);
guard!(
    root_face,
    IRawElementProviderFragmentRoot,
    IRawElementProviderFragmentRoot_Vtbl,
    GetFocus(),
);
