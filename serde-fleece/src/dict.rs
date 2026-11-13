use couchbase_lite_core_sys::{FLValue_AsString, FLValue_GetType};

use crate::{
    ffi::{
        _FLDict, _FLValue, FLDict, FLDict_Get, FLError, FLMutableDict_New, FLMutableDict_Release,
        FLMutableDict_SetInt, FLMutableDict_SetString, FLSlice, FLValueType, FLValue_AsData,
    },
    Error, NonNullConst, Value,
};
use std::{marker::PhantomData, ptr::NonNull};

#[repr(transparent)]
pub struct MutableDict(NonNull<_FLDict>);

impl MutableDict {
    #[inline]
    pub fn new() -> Result<Self, Error> {
        let dict = unsafe { FLMutableDict_New() };
        NonNull::new(dict)
            .ok_or(Error::Fleece(FLError::kFLMemoryError))
            .map(MutableDict)
    }
    #[inline]
    pub fn set_string(&mut self, key: &str, value: &str) {
        unsafe { FLMutableDict_SetString(self.0.as_ptr(), key.into(), value.into()) };
    }
    #[inline]
    pub fn set_i64(&mut self, key: &str, value: i64) {
        unsafe { FLMutableDict_SetInt(self.0.as_ptr(), key.into(), value) };
    }
    /// Borrows an immutable dictionary view
    #[inline]
    pub fn as_dict(&self) -> Dict<'_> {
        Dict {
            inner: self.0.into(),
            marker: PhantomData,
        }
    }
    #[inline]
    pub fn as_fleece_slice(&self) -> FLSlice {
        let value: NonNull<_FLValue> = self.0.cast();
        unsafe { FLValue_AsData(value.as_ptr()) }
    }
}

impl Drop for MutableDict {
    #[inline]
    fn drop(&mut self) {
        unsafe { FLMutableDict_Release(self.0.as_ptr()) };
    }
}

/// Immutable borrow of a Fleece dictionary and its backing data
#[repr(transparent)]
pub struct Dict<'a> {
    inner: NonNullConst<_FLDict>,
    marker: PhantomData<&'a _FLDict>,
}

impl<'a> Dict<'a> {
    /// # Safety
    ///
    /// If non-null, the dictionary and its data must stay valid and immutable for `'a`
    #[inline]
    pub unsafe fn new(dict: &FLDict) -> Option<Self> {
        let inner = NonNullConst::new(*dict)?;
        Some(Self {
            inner,
            marker: PhantomData,
        })
    }
    pub fn get_as_str(&self, prop_name: &str) -> Option<&str> {
        let val = unsafe { FLDict_Get(self.inner.as_ptr(), prop_name.into()) };
        let val = NonNullConst::new(val)?;
        if unsafe { FLValue_GetType(val.as_ptr()) } == FLValueType::kFLString {
            let raw_s = unsafe { FLValue_AsString(val.as_ptr()) };
            unsafe { raw_s.as_str_unchecked() }.ok()
        } else {
            None
        }
    }

    /// Borrows this dictionary as a value, preserving the backing data's lifetime
    #[inline]
    pub fn as_value(&self) -> Value<'a> {
        // SAFETY: The dictionary borrow covers the value and its data
        unsafe { Value::new(self.inner.cast()) }
    }
}
