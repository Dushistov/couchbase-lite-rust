mod c4_header;

use std::{borrow::Cow, mem, os::raw::c_void, ptr, slice, str};

pub use c4_header::*;

impl<'a> From<&'a str> for FLSlice {
    fn from(s: &'a str) -> Self {
        Self {
            buf: if !s.is_empty() {
                s.as_ptr() as *const c_void
            } else {
                ptr::null()
            },
            size: s.len(),
        }
    }
}

impl<'a> From<&'a [u8]> for FLSlice {
    fn from(s: &'a [u8]) -> Self {
        Self {
            buf: if !s.is_empty() {
                s.as_ptr() as *const c_void
            } else {
                ptr::null()
            },
            size: s.len(),
        }
    }
}

impl<'a> From<FLSlice> for &'a [u8] {
    fn from(s: FLSlice) -> Self {
        unsafe { slice::from_raw_parts(s.buf as *const u8, s.size) }
    }
}

impl Drop for FLSliceResult {
    fn drop(&mut self) {
        unsafe {
            FLSliceResult_Release(FLSliceResult {
                buf: self.buf,
                size: self.size,
            })
        };
    }
}

impl FLSliceResult {
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.buf as *const u8, self.size) }
    }
    #[inline]
    pub fn as_utf8_lossy(&self) -> Cow<str> {
        String::from_utf8_lossy(self.as_bytes())
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
    pub fn as_fl_slice(&self) -> FLSlice {
        FLSlice {
            buf: self.buf,
            size: self.size,
        }
    }
}

impl<'a> TryFrom<FLString> for &'a str {
    type Error = str::Utf8Error;

    fn try_from(value: FLString) -> Result<Self, Self::Error> {
        let bytes: &'a [u8] = value.into();
        str::from_utf8(bytes)
    }
}

// bindgen can not handle inline functions,
// see https://github.com/rust-lang/rust-bindgen/issues/1344

#[inline]
pub unsafe fn c4db_release(db: *mut C4Database) {
    c4base_release(db as *mut c_void)
}

#[inline]
#[allow(non_snake_case)]
pub unsafe fn FLSliceResult_Release(s: FLSliceResult) {
    _FLBuf_Release(s.buf);
    mem::forget(s);
}
