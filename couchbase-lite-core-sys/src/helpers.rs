//! Code to help deal with C API

use crate::{
    C4CollectionSpec, C4String, FLHeapSlice, FLSlice, FLSliceResult, FLSliceResult_Release,
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

impl FLSlice {
    /// Borrows the bytes referenced by this FFI slice
    ///
    /// # Safety
    ///
    /// For nonempty slices, uphold [`slice::from_raw_parts`]'s contract for `'a`
    #[inline]
    pub unsafe fn as_bytes_unchecked<'a>(self) -> &'a [u8] {
        if self.size == 0 {
            &[]
        } else {
            slice::from_raw_parts(self.buf as *const u8, self.size)
        }
    }

    /// Borrows UTF-8 text referenced by this FFI slice
    ///
    /// # Safety
    ///
    /// Same as [`FLSlice::as_bytes_unchecked`]
    #[inline]
    pub unsafe fn as_str_unchecked<'a>(self) -> Result<&'a str, str::Utf8Error> {
        str::from_utf8(self.as_bytes_unchecked())
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

impl AsRef<[u8]> for FLSliceResult {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// looks like according C++ code it is safe from other thread
unsafe impl Send for FLSliceResult {}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::ManuallyDrop;

    #[test]
    fn test_null_slice_handling() {
        let ffi_null_slice = FLSlice::default();
        let slice: &[u8] = unsafe { ffi_null_slice.as_bytes_unchecked() };
        assert!(slice.is_empty());

        // This value has no native allocation. Suppress its native Release call
        let ffi_null_slice = ManuallyDrop::new(FLSliceResult::default());
        assert!(ffi_null_slice.as_bytes().is_empty());
        assert!(ffi_null_slice.is_empty());
    }

    #[test]
    fn test_nonempty_slice_handling() {
        let bytes = [0, 1, 255];
        let ffi_slice = FLSlice::from(bytes.as_slice());
        let slice: &[u8] = unsafe { ffi_slice.as_bytes_unchecked() };
        assert_eq!(slice, bytes);

        // The bytes belong to Rust, so the native Release call must not run
        let ffi_result = ManuallyDrop::new(FLSliceResult {
            buf: bytes.as_ptr().cast(),
            size: bytes.len(),
        });
        assert_eq!(ffi_result.as_bytes(), bytes);
        let slice: &[u8] = unsafe { ffi_result.as_fl_slice().as_bytes_unchecked() };
        assert_eq!(slice, bytes);
        assert!(!ffi_result.is_empty());
    }

    #[test]
    fn test_string_slice_handling() {
        let text = "héllo";
        let ffi_string = FLSlice::from(text);
        let decoded: &str = unsafe { ffi_string.as_str_unchecked() }.unwrap();
        assert_eq!(decoded, text);

        let ffi_result = ManuallyDrop::new(FLSliceResult {
            buf: text.as_ptr().cast(),
            size: text.len(),
        });
        assert_eq!(ffi_result.as_utf8_lossy(), text);

        let invalid_utf8 = [255];
        let invalid_string = FLSlice::from(invalid_utf8.as_slice());
        let decoded: Result<&str, _> = unsafe { invalid_string.as_str_unchecked() };
        assert!(decoded.is_err());
    }

    #[test]
    #[cfg(not(miri))]
    fn test_native_empty_slice_result() {
        let ffi_null_slice = unsafe { crate::FLSliceResult_New(0) };
        assert!(ffi_null_slice.as_bytes().is_empty());
    }
}
