use couchbase_lite_core_sys::{FLValue_AsString, FLValue_GetType};

use crate::{
    ffi::{
        FLDict, FLDict_Get, FLError, FLMutableDict_New, FLMutableDict_Release,
        FLMutableDict_SetInt, FLMutableDict_SetString, FLSlice, FLValueType, FLValue_AsData,
        _FLDict, _FLValue,
    },
    Error, NonNullConst,
};
use std::{borrow::Borrow, marker::PhantomData, ptr::NonNull};

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
    #[inline]
    pub fn as_dict(&self) -> NonNullConst<_FLDict> {
        self.0.into()
    }
    #[inline]
    pub fn as_fleece_slice(&self) -> FLSlice {
        let value: NonNull<_FLValue> = self.0.cast();
        unsafe { FLValue_AsData(value.as_ptr()) }
    }
}

impl Drop for MutableDict {
    fn drop(&mut self) {
        unsafe { FLMutableDict_Release(self.0.as_ptr()) };
    }
}

pub struct Dict<'a> {
    inner: NonNullConst<_FLDict>,
    marker: PhantomData<&'a FLDict>,
}

impl<'a> Dict<'a> {
    pub fn new(dict: &FLDict) -> Option<Self> {
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
            raw_s.try_into().ok()
        } else {
            None
        }
    }
}

impl<'a> Borrow<NonNullConst<_FLDict>> for Dict<'a> {
    fn borrow(&self) -> &NonNullConst<_FLDict> {
        &self.inner
    }
}
