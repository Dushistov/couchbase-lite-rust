//! Code to help deal with C API

use crate::{FLHeapSlice, FLSlice, FLSliceResult, FLSliceResult_Release, FLString};
use std::{borrow::Cow, os::raw::c_void, ptr, slice, str};

impl Default for FLSlice {
    fn default() -> Self {
        Self {
            buf: ptr::null(),
            size: 0,
        }
    }
}

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

impl Default for FLSliceResult {
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

    fn try_from(value: FLString) -> Result<Self, Self::Error> {
        let bytes: &'a [u8] = value.into();
        str::from_utf8(bytes)
    }
}

impl FLHeapSlice {
    #[inline]
    pub fn as_fl_slice(&self) -> FLSlice {
        self._base
    }
}
