use crate::{
    error::Error,
    ffi::{
        FLValue, FLValueType, FLValue_AsBool, FLValue_AsDouble, FLValue_AsInt, FLValue_AsString,
        FLValue_AsUnsigned, FLValue_GetType, FLValue_IsDouble, FLValue_IsInteger,
        FLValue_IsUnsigned,
    },
    fl_slice::fl_slice_to_str_unchecked,
    Result,
};

#[derive(Debug, Clone, Copy)]
pub enum ValueRef<'a> {
    Null,
    Bool(bool),
    SignedInt(i64),
    UnsignedInt(u64),
    Double(f64),
    String(&'a str),
}

impl ValueRef<'_> {
    pub fn as_str(&self) -> Result<&str> {
        FromValueRef::column_result(*self)
    }
    pub fn as_u64(&self) -> Result<u64> {
        FromValueRef::column_result(*self)
    }
}

impl<'a> Into<ValueRef<'a>> for FLValue {
    fn into(self) -> ValueRef<'a> {
        use FLValueType::*;
        match unsafe { FLValue_GetType(self) } {
            kFLUndefined | kFLNull => ValueRef::Null,
            kFLBoolean => ValueRef::Bool(unsafe { FLValue_AsBool(self) }),
            kFLNumber => {
                if unsafe { FLValue_IsInteger(self) } {
                    ValueRef::SignedInt(unsafe { FLValue_AsInt(self) })
                } else if unsafe { FLValue_IsUnsigned(self) } {
                    ValueRef::UnsignedInt(unsafe { FLValue_AsUnsigned(self) })
                } else {
                    assert!(unsafe { FLValue_IsDouble(self) });
                    ValueRef::Double(unsafe { FLValue_AsDouble(self) })
                }
            }
            kFLString => {
                let s = unsafe { fl_slice_to_str_unchecked(FLValue_AsString(self)) };
                ValueRef::String(s)
            }
            kFLData | kFLArray | kFLDict => unimplemented!(),
        }
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
