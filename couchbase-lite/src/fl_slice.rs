use crate::ffi::{FLSlice, FLSliceResult, FLSliceResult_Release};
use std::{borrow::Cow, os::raw::c_void, ptr, slice, str};

pub(crate) trait AsFlSlice {
    fn as_flslice(&self) -> FLSlice;
}

impl<'a> AsFlSlice for &'a [u8] {
    fn as_flslice(&self) -> FLSlice {
        FLSlice {
            buf: self.as_ptr() as *const c_void,
            size: self.len(),
        }
    }
}

impl<'a> AsFlSlice for &'a str {
    fn as_flslice(&self) -> FLSlice {
        FLSlice {
            buf: self.as_ptr() as *const c_void,
            size: self.len(),
        }
    }
}

pub(crate) fn fl_slice_empty() -> FLSlice {
    FLSlice {
        buf: ptr::null(),
        size: 0,
    }
}

#[repr(transparent)]
pub(crate) struct FlSliceOwner(FLSliceResult);

impl FlSliceOwner {
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.0.buf as *const u8, self.0.size) }
    }
    #[inline]
    pub fn as_utf8_lossy(&self) -> Cow<str> {
        String::from_utf8_lossy(self.as_bytes())
    }
}

impl Drop for FlSliceOwner {
    fn drop(&mut self) {
        unsafe { FLSliceResult_Release(self.0) };
    }
}

impl From<FLSliceResult> for FlSliceOwner {
    fn from(x: FLSliceResult) -> Self {
        Self(x)
    }
}

impl Default for FlSliceOwner {
    fn default() -> Self {
        Self(FLSliceResult {
            buf: ptr::null(),
            size: 0,
        })
    }
}

#[inline]
pub(crate) unsafe fn fl_slice_to_str_unchecked<'a>(s: FLSlice) -> &'a str {
    let bytes: &[u8] = slice::from_raw_parts(s.buf as *const u8, s.size);
    str::from_utf8_unchecked(bytes)
}
