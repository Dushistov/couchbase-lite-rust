mod c4_header;

pub use c4_header::*;

use std::os::raw::c_void;

//bindgen can not handle inline functions so

#[inline]
pub unsafe fn c4db_release(db: *mut C4Database) {
    c4base_release(db as *mut c_void)
}

#[inline]
#[allow(non_snake_case)]
pub unsafe fn FLSliceResult_Release(s: FLSliceResult) {
    _FLBuf_Release(s.buf);
}

#[inline]
pub unsafe fn c4query_release(r: *mut C4Query) {
    c4base_release(r as *mut c_void)
}

impl From<FLHeapSlice> for FLSlice {
    fn from(x: FLHeapSlice) -> Self {
        x._base
    }
}
