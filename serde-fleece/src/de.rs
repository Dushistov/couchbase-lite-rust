mod dict;
mod seq;

use std::{borrow::Borrow, marker::PhantomData, ptr::NonNull};

use self::dict::DictAccess;
use crate::{
    de::{dict::EnumAccess, seq::ArrayAccess},
    ffi::{
        FLArray_Count, FLDict_Count, FLTrust, FLValueType, FLValue_AsArray, FLValue_AsBool,
        FLValue_AsDict, FLValue_AsDouble, FLValue_AsFloat, FLValue_AsInt, FLValue_AsString,
        FLValue_AsUnsigned, FLValue_FromData, FLValue_GetType, FLValue_IsDouble, FLValue_IsInteger,
        FLValue_IsUnsigned, _FLDict, _FLValue,
    },
    Error,
};
use itoa::Integer;
use serde::de::{self, IntoDeserializer};

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct NonNullConst<T>(*const T);

impl<T> NonNullConst<T> {
    #[inline]
    pub fn new(p: *const T) -> Option<Self> {
        if !p.is_null() {
            Some(Self(p))
        } else {
            None
        }
    }
    /// # Safety
    ///
    /// the caller must guarantee that `ptr` is non-null.
    #[inline]
    pub const unsafe fn new_unchecked(ptr: *const T) -> Self {
        NonNullConst(ptr)
    }
    #[must_use]
    #[inline]
    pub const fn as_ptr(&self) -> *const T {
        self.0
    }
    #[must_use]
    #[inline]
    pub const fn cast<U>(self) -> NonNullConst<U> {
        // SAFETY: `self` is a `NonNull` pointer which is necessarily non-null
        unsafe { NonNullConst::new_unchecked(self.as_ptr() as *mut U) }
    }
}

impl<T> From<NonNull<T>> for NonNullConst<T> {
    #[inline]
    fn from(x: NonNull<T>) -> Self {
        NonNullConst(x.as_ptr())
    }
}

pub(crate) struct Deserializer<'de> {
    pub value: NonNullConst<_FLValue>,
    marker: PhantomData<&'de [u8]>,
}

impl<'de> Deserializer<'de> {
    fn new(value: NonNullConst<_FLValue>) -> Self {
        Self {
            value,
            marker: PhantomData,
        }
    }
    fn from_slice(input: &'de [u8]) -> Result<Self, Error> {
        let fl_val = unsafe { FLValue_FromData(input.into(), FLTrust::kFLUntrusted) };
        let fl_val = NonNullConst::new(fl_val)
            .ok_or_else(|| Error::InvalidFormat("untrusted data validation failed".into()))?;
        Ok(Self::new(fl_val))
    }

    fn parse_signed<T: Integer + TryFrom<i64>>(&self) -> Result<T, Error> {
        let ty = unsafe { FLValue_GetType(self.value.as_ptr()) };
        if ty == FLValueType::kFLNumber && unsafe { FLValue_IsInteger(self.value.as_ptr()) } {
            let ret: T = unsafe { FLValue_AsInt(self.value.as_ptr()) }
                .try_into()
                .map_err(|_err| {
                    Error::InvalidFormat("Can not shrink i64 to smaller integer".into())
                })?;
            Ok(ret)
        } else {
            Err(Error::InvalidFormat(
                format!("Wrong data type: expect kFLNumber and integer, got {ty:?}").into(),
            ))
        }
    }

    fn parse_unsigned<T: Integer + TryFrom<u64>>(&self) -> Result<T, Error> {
        let ty = unsafe { FLValue_GetType(self.value.as_ptr()) };
        if ty == FLValueType::kFLNumber && unsafe { FLValue_IsInteger(self.value.as_ptr()) } {
            let ret: T = unsafe { FLValue_AsUnsigned(self.value.as_ptr()) }
                .try_into()
                .map_err(|_err| {
                    Error::InvalidFormat("Can not shrink u64 to smaller unsigned integer".into())
                })?;
            Ok(ret)
        } else {
            Err(Error::InvalidFormat(
                format!("Wrong data type: expect kFLNumber and integer, got {ty:?}").into(),
            ))
        }
    }

    fn parse_str(&self) -> Result<&'de str, Error> {
        let ty = unsafe { FLValue_GetType(self.value.as_ptr()) };
        if ty == FLValueType::kFLString {
            let s: &str = unsafe { FLValue_AsString(self.value.as_ptr()) }.try_into()?;
            Ok(s)
        } else {
            Err(Error::InvalidFormat(
                format!("Wrong data type: expect kFLString, got {ty:?}").into(),
            ))
        }
    }
}

pub fn from_slice<'a, T>(s: &'a [u8]) -> Result<T, Error>
where
    T: de::Deserialize<'a>,
{
    let mut deserializer = Deserializer::from_slice(s)?;
    T::deserialize(&mut deserializer)
}

pub fn from_fl_dict<'a, T, Dict>(dict: Dict) -> Result<T, Error>
where
    T: de::Deserialize<'a>,
    Dict: Borrow<NonNullConst<_FLDict>>,
{
    let value: NonNullConst<_FLValue> = dict.borrow().cast();
    let mut deserializer = Deserializer::<'a>::new(value);
    T::deserialize(&mut deserializer)
}

pub fn from_fl_value<'a, T: de::Deserialize<'a>>(
    value: NonNullConst<_FLValue>,
) -> Result<T, Error> {
    let mut deserializer = Deserializer::new(value);
    T::deserialize(&mut deserializer)
}

impl<'de> de::Deserializer<'de> for &mut Deserializer<'de> {
    type Error = Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        let fl_type = unsafe { FLValue_GetType(self.value.as_ptr()) };
        match fl_type {
            FLValueType::kFLUndefined => Err(Error::Unsupported(
                "deserialize self described: `undefined` not supported",
            )),
            FLValueType::kFLNull => self.deserialize_unit(visitor),
            FLValueType::kFLBoolean => self.deserialize_bool(visitor),
            FLValueType::kFLNumber => {
                if unsafe { FLValue_IsUnsigned(self.value.as_ptr()) } {
                    self.deserialize_u64(visitor)
                } else if unsafe { FLValue_IsInteger(self.value.as_ptr()) } {
                    self.deserialize_i64(visitor)
                } else if unsafe { FLValue_IsDouble(self.value.as_ptr()) } {
                    self.deserialize_f64(visitor)
                } else {
                    self.deserialize_f32(visitor)
                }
            }
            FLValueType::kFLString => self.deserialize_str(visitor),
            FLValueType::kFLData => Err(Error::Unsupported(
                "deserialize self described: `data` not supported",
            )),
            FLValueType::kFLArray => self.deserialize_seq(visitor),
            FLValueType::kFLDict => self.deserialize_map(visitor),
        }
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        let ty = unsafe { FLValue_GetType(self.value.as_ptr()) };
        if ty == FLValueType::kFLBoolean {
            visitor.visit_bool(unsafe { FLValue_AsBool(self.value.as_ptr()) })
        } else {
            Err(Error::InvalidFormat(
                format!("Wrong data type: expect kFLBoolean, got {ty:?}").into(),
            ))
        }
    }

    fn deserialize_i8<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_i8(self.parse_signed()?)
    }

    fn deserialize_i16<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_i16(self.parse_signed()?)
    }

    fn deserialize_i32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_i32(self.parse_signed()?)
    }

    fn deserialize_i64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_i64(self.parse_signed()?)
    }

    fn deserialize_u8<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_u8(self.parse_unsigned()?)
    }

    fn deserialize_u16<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_u16(self.parse_unsigned()?)
    }

    fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_u32(self.parse_unsigned()?)
    }

    fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_u64(self.parse_unsigned()?)
    }

    fn deserialize_f32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        let ty = unsafe { FLValue_GetType(self.value.as_ptr()) };
        if ty == FLValueType::kFLNumber {
            visitor.visit_f32(unsafe { FLValue_AsFloat(self.value.as_ptr()) })
        } else {
            Err(Error::InvalidFormat(
                format!("Wrong data type: expect kFLNumber, got {ty:?}").into(),
            ))
        }
    }

    fn deserialize_f64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        let ty = unsafe { FLValue_GetType(self.value.as_ptr()) };
        if ty == FLValueType::kFLNumber {
            visitor.visit_f64(unsafe { FLValue_AsDouble(self.value.as_ptr()) })
        } else {
            Err(Error::InvalidFormat(
                format!("Wrong data type: expect kFLNumber, got {ty:?}").into(),
            ))
        }
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        let s = self.parse_str()?;
        let mut it = s.chars();
        let ch = it.next();
        let end = it.next();
        if let (Some(ch), None) = (ch, end) {
            visitor.visit_char(ch)
        } else {
            Err(Error::InvalidFormat(
                format!("string({s}) should contain exactly one char").into(),
            ))
        }
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_borrowed_str(self.parse_str()?)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported("deserialization of bytes not supported"))
    }

    fn deserialize_byte_buf<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported(
            "deserialization of byte buf not supported",
        ))
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        if unsafe { FLValue_GetType(self.value.as_ptr()) } != FLValueType::kFLNull {
            visitor.visit_some(self)
        } else {
            visitor.visit_none()
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        if unsafe { FLValue_GetType(self.value.as_ptr()) } == FLValueType::kFLNull {
            visitor.visit_unit()
        } else {
            Err(Error::InvalidFormat(
                "Expect null in the place of unit".into(),
            ))
        }
    }

    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        let ty = unsafe { FLValue_GetType(self.value.as_ptr()) };
        if ty == FLValueType::kFLArray {
            let arr = unsafe { FLValue_AsArray(self.value.as_ptr()) };
            let arr = NonNullConst::new(arr)
                .ok_or_else(|| Error::InvalidFormat("array is not array type".into()))?;
            let n = unsafe { FLArray_Count(arr.as_ptr()) };
            let n: usize = n.try_into().map_err(|err| {
                Error::InvalidFormat(format!("Can not convert {n} to usize: {err}").into())
            })?;
            visitor.visit_seq(ArrayAccess::new(arr, n))
        } else {
            Err(Error::InvalidFormat(
                format!("Wrong data type: expect kFLArray, got {ty:?}").into(),
            ))
        }
    }

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        let ftype = unsafe { FLValue_GetType(self.value.as_ptr()) };
        if ftype != FLValueType::kFLDict {
            return Err(Error::InvalidFormat(
                format!("map has {ftype:?} type, should be kFLDict").into(),
            ));
        }

        let dict = unsafe { FLValue_AsDict(self.value.as_ptr()) };
        let dict = NonNullConst::new(dict)
            .ok_or_else(|| Error::InvalidFormat("map: value to dict return null".into()))?;
        let n = unsafe { FLDict_Count(dict.as_ptr()) };
        let n: usize = n.try_into().map_err(|err| {
            Error::InvalidFormat(format!("Can not convert {n} to usize: {err}").into())
        })?;
        visitor.visit_map(DictAccess::new(dict, n))
    }

    fn deserialize_struct<V>(
        self,
        name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        let fv_type = unsafe { FLValue_GetType(self.value.as_ptr()) };
        if fv_type != FLValueType::kFLDict {
            return Err(Error::InvalidFormat(
                format!("For struct {name} fleece data should be dict type, but got: {fv_type:?}")
                    .into(),
            ));
        }

        let dict = unsafe { FLValue_AsDict(self.value.as_ptr()) };
        let dict = NonNullConst::new(dict).ok_or_else(|| {
            Error::InvalidFormat(format!("struct {name} has not dict type (null)").into())
        })?;
        let dict_size = unsafe { FLDict_Count(dict.as_ptr()) };
        let dict_size: usize = dict_size.try_into().map_err(|err| {
            Error::InvalidFormat(format!("Can not convert {dict_size} to usize: {err}").into())
        })?;

        visitor.visit_map(DictAccess::new(dict, dict_size))
    }

    fn deserialize_enum<V>(
        self,
        name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        let ftype = unsafe { FLValue_GetType(self.value.as_ptr()) };
        match ftype {
            FLValueType::kFLString => {
                let s: &str = unsafe { FLValue_AsString(self.value.as_ptr()) }.try_into()?;
                visitor.visit_enum(s.into_deserializer())
            }
            FLValueType::kFLDict => {
                let dict = unsafe { FLValue_AsDict(self.value.as_ptr()) };
                let dict = NonNullConst::new(dict).ok_or_else(|| {
                    Error::InvalidFormat(format!("enum {name} has not dict type (null)").into())
                })?;
                visitor.visit_enum(EnumAccess::new(dict))
            }
            _ => Err(Error::InvalidFormat(
                format!("Invalid type {ftype:?} for enum {name}").into(),
            )),
        }
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        // Called in case like there is unexpected key in dictionary, and we
        // want to ignore value for this key. In our case we can do nothing.
        // This is IgnoreAny visitor, and visit_unit do nothing.
        visitor.visit_unit()
    }
}
