#![allow(
    unknown_lints,
    non_upper_case_globals,
    dead_code,
    non_camel_case_types,
    improper_ctypes,
    non_snake_case,
    clippy::all
)]

include!(concat!(env!("OUT_DIR"), "/c4_header.rs"));

// bindgen can not handle inline functions,
// see https://github.com/rust-lang/rust-bindgen/issues/1344

#[inline]
pub unsafe fn c4db_release(db: *mut C4Database) {
    c4base_release(db as *mut std::os::raw::c_void)
}

#[inline]
#[allow(non_snake_case)]
pub unsafe fn FLSliceResult_Release(s: FLSliceResult) {
    _FLBuf_Release(s.buf);
    std::mem::forget(s);
}

#[inline]
#[allow(non_snake_case)]
pub unsafe fn FLMutableDict_Release(d: FLMutableDict) {
    FLValue_Release(d as *const _FLValue);
}

#[inline]
#[allow(non_snake_case)]
pub unsafe fn FLMutableDict_SetInt(d: FLMutableDict, key: FLString, val: i64) {
    FLSlot_SetInt(FLMutableDict_Set(d, key), val);
}

#[inline]
#[allow(non_snake_case)]
pub unsafe fn FLMutableDict_SetString(d: FLMutableDict, key: FLString, val: FLString) {
    FLSlot_SetString(FLMutableDict_Set(d, key), val);
}
