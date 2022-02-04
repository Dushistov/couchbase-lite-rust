use couchbase_lite_core_sys::_FLValue;

use crate::{
    ffi::{
        FLError, FLMutableDict_New, FLMutableDict_Release, FLMutableDict_SetInt,
        FLMutableDict_SetString, FLSlice, FLValue_AsData, _FLDict,
    },
    Error, NonNullConst,
};
use std::ptr::NonNull;

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
