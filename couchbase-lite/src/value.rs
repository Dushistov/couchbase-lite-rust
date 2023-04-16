use crate::{
    error::{Error, Result},
    ffi::{
        FLArray, FLArray_Count, FLArray_Get, FLArray_IsEmpty, FLDict, FLDict_Count, FLDict_Get,
        FLDict_IsEmpty, FLSlice, FLValue, FLValueType, FLValue_AsArray, FLValue_AsBool,
        FLValue_AsDict, FLValue_AsDouble, FLValue_AsInt, FLValue_AsString, FLValue_AsUnsigned,
        FLValue_GetType, FLValue_IsDouble, FLValue_IsInteger, FLValue_IsUnsigned,
    },
};
use std::convert::TryFrom;

#[derive(Debug, Clone, Copy)]
pub enum ValueRef<'a> {
    Null,
    Bool(bool),
    SignedInt(i64),
    UnsignedInt(u64),
    Double(f64),
    String(&'a str),
    Array(ValueRefArray),
    Dict(ValueRefDict),
}

impl ValueRef<'_> {
    #[inline]
    pub fn as_str(&self) -> Result<&str> {
        FromValueRef::column_result(*self)
    }
    #[inline]
    pub fn as_u64(&self) -> Result<u64> {
        FromValueRef::column_result(*self)
    }
    #[inline]
    pub fn is_null(&self) -> bool {
        matches!(self, ValueRef::Null)
    }
    pub(crate) unsafe fn new(value: FLValue) -> Self {
        use FLValueType::*;
        match FLValue_GetType(value) {
            kFLUndefined | kFLNull => ValueRef::Null,
            kFLBoolean => ValueRef::Bool(FLValue_AsBool(value)),
            kFLNumber => {
                if FLValue_IsInteger(value) {
                    ValueRef::SignedInt(FLValue_AsInt(value))
                } else if FLValue_IsUnsigned(value) {
                    ValueRef::UnsignedInt(FLValue_AsUnsigned(value))
                } else {
                    assert!(FLValue_IsDouble(value));
                    ValueRef::Double(FLValue_AsDouble(value))
                }
            }
            kFLString => {
                let s: &str = FLValue_AsString(value).try_into().expect("not valid utf-8");
                ValueRef::String(s)
            }
            kFLArray => ValueRef::Array(ValueRefArray(FLValue_AsArray(value))),
            kFLDict => ValueRef::Dict(ValueRefDict(FLValue_AsDict(value))),
            kFLData => unimplemented!(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct ValueRefArray(FLArray);

impl ValueRefArray {
    #[inline]
    pub fn len(&self) -> u32 {
        unsafe { FLArray_Count(self.0) }
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        unsafe { FLArray_IsEmpty(self.0) }
    }
    pub(crate) unsafe fn get_raw(&self, idx: u32) -> FLValue {
        FLArray_Get(self.0, idx)
    }
    #[inline]
    pub fn get(&self, idx: u32) -> ValueRef {
        unsafe { ValueRef::new(self.get_raw(idx)) }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct ValueRefDict(FLDict);

impl ValueRefDict {
    #[inline]
    pub fn len(&self) -> u32 {
        unsafe { FLDict_Count(self.0) }
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        unsafe { FLDict_IsEmpty(self.0) }
    }
    pub(crate) unsafe fn get_raw(&self, key: FLSlice) -> FLValue {
        FLDict_Get(self.0, key)
    }
    #[inline]
    pub fn get(&self, key: FLSlice) -> ValueRef {
        unsafe { ValueRef::new(self.get_raw(key)) }
    }
}

pub trait FromValueRef<'a>: Sized {
    fn column_result(val: ValueRef<'a>) -> Result<Self>;
}

impl<'a> FromValueRef<'a> for &'a str {
    #[inline]
    fn column_result(val: ValueRef<'a>) -> Result<Self> {
        if let ValueRef::String(x) = val {
            Ok(x)
        } else {
            Err(Error::LogicError(format!(
                "Wrong ValueRef type, expect String, got {val:?}"
            )))
        }
    }
}

impl<'a> FromValueRef<'a> for u16 {
    fn column_result(val: ValueRef<'a>) -> Result<Self> {
        match val {
            ValueRef::SignedInt(x) => u16::try_from(x).map_err(|err| {
                Error::LogicError(format!(
                    "ValueRef(Signed) {x} to u16 conversation error: {err}"
                ))
            }),
            ValueRef::UnsignedInt(x) => u16::try_from(x).map_err(|err| {
                Error::LogicError(format!(
                    "ValueRef(Unsigned) {x} to u16 conversation error: {err}"
                ))
            }),
            _ => Err(Error::LogicError(format!(
                "Wrong ValueRef type, expect SignedInt|UnsignedInt (u16) got {val:?}"
            ))),
        }
    }
}

impl<'a> FromValueRef<'a> for u32 {
    fn column_result(val: ValueRef<'a>) -> Result<Self> {
        match val {
            ValueRef::SignedInt(x) => u32::try_from(x).map_err(|err| {
                Error::LogicError(format!(
                    "ValueRef(Signed) {x} to u32 conversation error: {err}"
                ))
            }),
            ValueRef::UnsignedInt(x) => u32::try_from(x).map_err(|err| {
                Error::LogicError(format!(
                    "ValueRef(Unsigned) {x} to u32 conversation error: {err}"
                ))
            }),
            _ => Err(Error::LogicError(format!(
                "Wrong ValueRef type, expect SignedInt|UnsignedInt (u32) got {val:?}"
            ))),
        }
    }
}

impl<'a> FromValueRef<'a> for u64 {
    fn column_result(val: ValueRef<'a>) -> Result<Self> {
        match val {
            ValueRef::SignedInt(x) => u64::try_from(x).map_err(|err| {
                Error::LogicError(format!(
                    "ValueRef(Signed) {x} to u32 conversation error: {err}"
                ))
            }),
            ValueRef::UnsignedInt(x) => Ok(x),
            _ => Err(Error::LogicError(format!(
                "Wrong ValueRef type, expect SignedInt|UnsignedInt (u64) got {val:?}"
            ))),
        }
    }
}

impl<'a> FromValueRef<'a> for i64 {
    fn column_result(val: ValueRef<'a>) -> Result<Self> {
        match val {
            ValueRef::SignedInt(x) => Ok(x),
            ValueRef::UnsignedInt(x) => i64::try_from(x).map_err(|err| {
                Error::LogicError(format!(
                    "ValueRef (UnsignedInt) to i64 conversation failed: {err}"
                ))
            }),
            _ => Err(Error::LogicError(format!(
                "Wrong ValueRef type, expect SignedInt|UnsignedInt (i64) got {val:?}"
            ))),
        }
    }
}

impl<'a> FromValueRef<'a> for usize {
    fn column_result(val: ValueRef<'a>) -> Result<Self> {
        match val {
            ValueRef::SignedInt(x) => usize::try_from(x).map_err(|err| {
                Error::LogicError(format!(
                    "ValueRef(Signed) {x} to usize conversation error: {err}"
                ))
            }),
            ValueRef::UnsignedInt(x) => usize::try_from(x).map_err(|err| {
                Error::LogicError(format!(
                    "ValueRef(Unsigned) {x} to usize conversation error: {err}"
                ))
            }),
            _ => Err(Error::LogicError(format!(
                "Wrong ValueRef type, expect SignedInt|UnsignedInt (usize) got {val:?}"
            ))),
        }
    }
}
