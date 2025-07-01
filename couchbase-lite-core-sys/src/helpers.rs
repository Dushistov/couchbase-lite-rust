//! Code to help deal with C API

use crate::{
    C4CollectionSpec, C4String, FLHeapSlice, FLSlice, FLSliceResult, FLSliceResult_Release,
    FLString,
};
use std::{borrow::Cow, os::raw::c_void, ptr, slice, str};

impl Default for FLSlice {
    #[inline]
    fn default() -> Self {
        Self {
            buf: ptr::null(),
            size: 0,
        }
    }
}

impl<'a> From<&'a str> for FLSlice {
    #[inline]
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
    #[inline]
    fn from(ba: &'a [u8]) -> Self {
        Self {
            buf: if !ba.is_empty() {
                ba.as_ptr() as *const c_void
            } else {
                ptr::null()
            },
            size: ba.len(),
        }
    }
}

impl From<FLSlice> for &[u8] {
    #[inline]
    fn from(s: FLSlice) -> Self {
        if s.size != 0 {
            unsafe { slice::from_raw_parts(s.buf as *const u8, s.size) }
        } else {
            // pointer should not be null, even in zero case
            // but pointer from FLSlice can be null in zero case, so:
            &[]
        }
    }
}

impl Drop for FLSliceResult {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            FLSliceResult_Release(FLSliceResult {
                buf: self.buf,
                size: self.size,
            })
        };
    }
}

impl Default for FLSliceResult {
    #[inline]
    fn default() -> Self {
        Self {
            buf: ptr::null(),
            size: 0,
        }
    }
}

impl FLSliceResult {
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        if self.size != 0 {
            unsafe { slice::from_raw_parts(self.buf as *const u8, self.size) }
        } else {
            // pointer should not be null, even in zero case
            // but pointer from FLSlice can be null in zero case, so:
            &[]
        }
    }
    #[inline]
    pub fn as_utf8_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(self.as_bytes())
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
    #[inline]
    pub fn as_fl_slice(&self) -> FLSlice {
        FLSlice {
            buf: self.buf,
            size: self.size,
        }
    }
}

impl<'a> TryFrom<FLString> for &'a str {
    type Error = str::Utf8Error;
    #[inline]
    fn try_from(value: FLString) -> Result<Self, Self::Error> {
        let bytes: &'a [u8] = value.into();
        str::from_utf8(bytes)
    }
}

impl FLHeapSlice {
    #[inline]
    pub fn as_fl_slice(&self) -> FLSlice {
        *self
    }
}

// bindgen can not handle these constants properly, so redefine them
// see https://github.com/rust-lang/rust-bindgen/issues/316
macro_rules! flstr {
    ($str:expr) => {
        C4String {
            buf: $str.as_bytes().as_ptr() as *const c_void,
            size: $str.as_bytes().len() - 1,
        }
    };
}

/// #define kC4DefaultScopeID FLSTR("_default")
#[allow(non_upper_case_globals)]
pub const kC4DefaultScopeID: C4String = flstr!("_default\0");

/// #define kC4DefaultCollectionName FLSTR("_default")
#[allow(non_upper_case_globals)]
pub const kC4DefaultCollectionName: C4String = flstr!("_default\0");

#[allow(non_upper_case_globals)]
pub const kC4DefaultCollectionSpec: C4CollectionSpec = C4CollectionSpec {
    name: kC4DefaultCollectionName,
    scope: kC4DefaultScopeID,
};

#[test]
fn test_null_slice_handling() {
    let ffi_null_slice = FLSlice {
        buf: ptr::null(),
        size: 0,
    };
    let slice: &[u8] = ffi_null_slice.into();
    assert!(slice.is_empty());

    let ffi_null_slice: FLSliceResult = unsafe { crate::FLSliceResult_New(0) };
    let slice: &[u8] = ffi_null_slice.as_bytes();
    assert!(slice.is_empty());

    let ffi_null_slice = FLSliceResult {
        buf: ptr::null(),
        size: 0,
    };
    let slice: &[u8] = ffi_null_slice.as_bytes();
    assert!(slice.is_empty());
}
