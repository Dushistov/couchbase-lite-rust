use crate::{
    error::Error,
    ffi::{
        FLArray, FLArray_Count, FLArray_Get, FLArray_IsEmpty, FLDict, FLDict_Count, FLDict_IsEmpty,
        FLValue, FLValueType, FLValue_AsArray, FLValue_AsBool, FLValue_AsDict, FLValue_AsDouble,
        FLValue_AsInt, FLValue_AsString, FLValue_AsUnsigned, FLValue_GetType, FLValue_IsDouble,
        FLValue_IsInteger, FLValue_IsUnsigned,
    },
    fl_slice::fl_slice_to_str_unchecked,
    Result,
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
    pub fn as_str(&self) -> Result<&str> {
        FromValueRef::column_result(*self)
    }
    pub fn as_u64(&self) -> Result<u64> {
        FromValueRef::column_result(*self)
    }
    pub fn is_null(&self) -> bool {
        match self {
            ValueRef::Null => true,
            _ => false,
        }
    }
}

impl<'a> From<FLValue> for ValueRef<'a> {
    fn from(value: FLValue) -> ValueRef<'a> {
        use FLValueType::*;
        match unsafe { FLValue_GetType(value) } {
            kFLUndefined | kFLNull => ValueRef::Null,
            kFLBoolean => ValueRef::Bool(unsafe { FLValue_AsBool(value) }),
            kFLNumber => {
                if unsafe { FLValue_IsInteger(value) } {
                    ValueRef::SignedInt(unsafe { FLValue_AsInt(value) })
                } else if unsafe { FLValue_IsUnsigned(value) } {
                    ValueRef::UnsignedInt(unsafe { FLValue_AsUnsigned(value) })
                } else {
                    assert!(unsafe { FLValue_IsDouble(value) });
                    ValueRef::Double(unsafe { FLValue_AsDouble(value) })
                }
            }
            kFLString => {
                let s = unsafe { fl_slice_to_str_unchecked(FLValue_AsString(value)) };
                ValueRef::String(s)
            }
            kFLArray => ValueRef::Array(ValueRefArray(unsafe { FLValue_AsArray(value) })),
            kFLDict => ValueRef::Dict(ValueRefDict(unsafe { FLValue_AsDict(value) })),
            kFLData => unimplemented!(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct ValueRefArray(FLArray);

impl ValueRefArray {
    pub fn len(&self) -> u32 {
        unsafe { FLArray_Count(self.0) }
    }
    pub fn is_empty(&self) -> bool {
        unsafe { FLArray_IsEmpty(self.0) }
    }
    pub(crate) unsafe fn get_raw(&self, idx: u32) -> FLValue {
        FLArray_Get(self.0, idx)
    }
    pub fn get<'a>(&'a self, idx: u32) -> ValueRef<'a> {
        unsafe { self.get_raw(idx) }.into()
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct ValueRefDict(FLDict);

impl ValueRefDict {
    pub fn len(&self) -> u32 {
        unsafe { FLDict_Count(self.0) }
    }
    pub fn is_empty(&self) -> bool {
        unsafe { FLDict_IsEmpty(self.0) }
    }
}

pub trait FromValueRef<'a>: Sized {
    fn column_result(value: ValueRef<'a>) -> Result<Self>;
}

impl<'a> FromValueRef<'a> for &'a str {
    fn column_result(value: ValueRef<'a>) -> Result<Self> {
        if let ValueRef::String(x) = value {
            Ok(x)
        } else {
            Err(Error::LogicError(format!(
                "Wrong ValueRef type, expect String, got {:?}",
                value
            )))
        }
    }
}

impl<'a> FromValueRef<'a> for u16 {
    fn column_result(value: ValueRef<'a>) -> Result<Self> {
        match value {
            ValueRef::SignedInt(x) => {
                if x >= 0 && x <= u16::max_value() as i64 {
                    Ok(x as u16)
                } else {
                    Err(Error::LogicError(format!(
                        "ValueRef -> u16, SignedInt too big or negative: {}",
                        x
                    )))
                }
            }
            ValueRef::UnsignedInt(x) => {
                if x <= u16::max_value() as u64 {
                    Ok(x as u16)
                } else {
                    Err(Error::LogicError(format!(
                        "ValueRef -> u16, UnsignedInt too big: {}",
                        x
                    )))
                }
            }
            _ => Err(Error::LogicError(format!(
                "Wrong ValueRef type, expect SignedInt|UnsignedInt (u16) got {:?}",
                value
            ))),
        }
    }
}

impl<'a> FromValueRef<'a> for u32 {
    fn column_result(value: ValueRef<'a>) -> Result<Self> {
        match value {
            ValueRef::SignedInt(x) => {
                if x >= 0 && x <= u32::max_value() as i64 {
                    Ok(x as u32)
                } else {
                    Err(Error::LogicError(format!(
                        "ValueRef -> u32, SignedInt too big or negative: {}",
                        x
                    )))
                }
            }
            ValueRef::UnsignedInt(x) => {
                if x <= u32::max_value() as u64 {
                    Ok(x as u32)
                } else {
                    Err(Error::LogicError(format!(
                        "ValueRef -> u32, UnsignedInt too big: {}",
                        x
                    )))
                }
            }
            _ => Err(Error::LogicError(format!(
                "Wrong ValueRef type, expect SignedInt|UnsignedInt (u32) got {:?}",
                value
            ))),
        }
    }
}

impl<'a> FromValueRef<'a> for u64 {
    fn column_result(value: ValueRef<'a>) -> Result<Self> {
        match value {
            ValueRef::SignedInt(x) => {
                if x >= 0 {
                    Ok(x as u64)
                } else {
                    Err(Error::LogicError(format!(
                        "ValueRef -> u64, SignedInt negative: {}",
                        x
                    )))
                }
            }
            ValueRef::UnsignedInt(x) => Ok(x),
            _ => Err(Error::LogicError(format!(
                "Wrong ValueRef type, expect SignedInt|UnsignedInt (u64) got {:?}",
                value
            ))),
        }
    }
}

impl<'a> FromValueRef<'a> for i64 {
    fn column_result(value: ValueRef<'a>) -> Result<Self> {
        match value {
            ValueRef::SignedInt(x) => Ok(x),
            ValueRef::UnsignedInt(x) => i64::try_from(x).map_err(|err| {
                Error::LogicError(format!(
                    "ValueRef (UnsignedInt) to i64 conversation failed: {}",
                    err
                ))
            }),
            _ => Err(Error::LogicError(format!(
                "Wrong ValueRef type, expect SignedInt|UnsignedInt (i64) got {:?}",
                value
            ))),
        }
    }
}
