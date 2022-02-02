macro_rules! encoder_write {
    ($this:expr, $func:ident $(, $arg:expr)*) => {
        unsafe {
            if $func($this.inner, $($arg)*) {
                Ok(())
            } else {
                Err(Error::from(FLEncoder_GetError($this.inner)))
            }
        }
    };
}

mod map;

use self::map::MapKeySerializer;
use crate::error::Error;
use crate::ffi::{
    FLEncoder_BeginArray, FLEncoder_BeginDict, FLEncoder_EndArray, FLEncoder_EndDict,
    FLEncoder_Finish, FLEncoder_Free, FLEncoder_GetError, FLEncoder_New, FLEncoder_Reset,
    FLEncoder_WriteBool, FLEncoder_WriteDouble, FLEncoder_WriteFloat, FLEncoder_WriteInt,
    FLEncoder_WriteKey, FLEncoder_WriteNull, FLEncoder_WriteString, FLEncoder_WriteUInt, FLError,
    FLSliceResult, _FLEncoder,
};
use serde::{ser, Serialize};
use std::fmt::Display;
use std::ops::{Deref, DerefMut};

pub(crate) struct Serializer<'a> {
    inner: &'a mut _FLEncoder,
}

/// Helper struct for multiple uses of `FLEncoder`
pub struct FlEncoderSession<'a> {
    inner: &'a mut _FLEncoder,
}

impl<'a> FlEncoderSession<'a> {
    pub fn new(inner: &'a mut _FLEncoder) -> Self {
        Self { inner }
    }
}

impl<'a> Drop for FlEncoderSession<'a> {
    fn drop(&mut self) {
        unsafe { FLEncoder_Reset(self.inner) }
    }
}

impl<'a> Deref for FlEncoderSession<'a> {
    type Target = _FLEncoder;
    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

impl<'a> DerefMut for FlEncoderSession<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
    }
}

pub fn to_fl_slice_result<T>(value: &T) -> Result<FLSliceResult, Error>
where
    T: Serialize,
{
    let enc = unsafe {
        let enc = FLEncoder_New();
        if enc.is_null() {
            return Err(Error::Fleece(FLError::kFLMemoryError));
        }
        &mut *enc
    };
    let ret = to_fl_slice_result_with_encoder(value, &mut *enc);
    unsafe { FLEncoder_Free(enc) };
    ret
}

pub fn to_fl_slice_result_with_encoder<T, FleeceEncoder>(
    value: &T,
    mut encoder: FleeceEncoder,
) -> Result<FLSliceResult, Error>
where
    T: Serialize,
    FleeceEncoder: DerefMut<Target = _FLEncoder>,
{
    let mut serializer = Serializer {
        inner: encoder.deref_mut(),
    };
    value.serialize(&mut serializer)?;
    let mut err = FLError::kFLNoError;
    let ret = unsafe { FLEncoder_Finish(serializer.inner, &mut err) };
    if !ret.is_empty() {
        Ok(ret)
    } else {
        Err(err.into())
    }
}

impl<'a, 'b> ser::Serializer for &'a mut Serializer<'b> {
    type Ok = ();
    type Error = Error;

    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = MapKeySerializer<'a, 'b>;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    #[inline]
    fn serialize_bool(self, v: bool) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteBool, v)
    }
    #[inline]
    fn serialize_i8(self, v: i8) -> Result<Self::Ok, Self::Error> {
        let v = i64::from(v);
        encoder_write!(self, FLEncoder_WriteInt, v)
    }
    #[inline]
    fn serialize_i16(self, v: i16) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteInt, i64::from(v))
    }
    #[inline]
    fn serialize_i32(self, v: i32) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteInt, i64::from(v))
    }
    #[inline]
    fn serialize_i64(self, v: i64) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteInt, v)
    }
    #[inline]
    fn serialize_u8(self, v: u8) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteUInt, u64::from(v))
    }
    #[inline]
    fn serialize_u16(self, v: u16) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteUInt, u64::from(v))
    }
    #[inline]
    fn serialize_u32(self, v: u32) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteUInt, u64::from(v))
    }
    #[inline]
    fn serialize_u64(self, v: u64) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteUInt, v)
    }
    #[inline]
    fn serialize_f32(self, v: f32) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteFloat, v)
    }
    #[inline]
    fn serialize_f64(self, v: f64) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteDouble, v)
    }
    #[inline]
    fn serialize_char(self, v: char) -> Result<Self::Ok, Self::Error> {
        let mut tmp = [0u8; 4];
        let s: &str = v.encode_utf8(&mut tmp);
        encoder_write!(self, FLEncoder_WriteString, s.into())
    }
    #[inline]
    fn serialize_str(self, v: &str) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteString, v.into())
    }
    #[inline]
    fn serialize_bytes(self, _v: &[u8]) -> Result<Self::Ok, Self::Error> {
        Err(Error::Unsupported("Write raw bytes unsupported"))
    }
    #[inline]
    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteNull)
    }
    #[inline]
    fn serialize_some<T: ?Sized>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: Serialize,
    {
        value.serialize(&mut *self)
    }
    #[inline]
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteNull)
    }
    #[inline]
    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteNull)
    }
    #[inline]
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self, FLEncoder_WriteString, variant.into())
    }
    #[inline]
    fn serialize_newtype_struct<T: ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: Serialize,
    {
        value.serialize(&mut *self)
    }
    #[inline]
    fn serialize_newtype_variant<T: ?Sized>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: Serialize,
    {
        encoder_write!(self, FLEncoder_BeginDict, 1)?;
        encoder_write!(self, FLEncoder_WriteKey, variant.into())?;
        value.serialize(&mut *self)?;
        encoder_write!(self, FLEncoder_EndDict)
    }
    #[inline]
    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        encoder_write!(self, FLEncoder_BeginArray, len.unwrap_or(0))?;
        Ok(self)
    }
    #[inline]
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        encoder_write!(self, FLEncoder_BeginArray, len)?;
        Ok(self)
    }
    #[inline]
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        encoder_write!(self, FLEncoder_BeginArray, len)?;
        Ok(self)
    }
    #[inline]
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        encoder_write!(self, FLEncoder_BeginDict, 1)?;
        encoder_write!(self, FLEncoder_WriteKey, variant.into())?;
        encoder_write!(self, FLEncoder_BeginArray, len)?;
        Ok(self)
    }
    #[inline]
    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        encoder_write!(self, FLEncoder_BeginDict, len.unwrap_or(0))?;
        Ok(MapKeySerializer { ser: self })
    }
    #[inline]
    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        encoder_write!(self, FLEncoder_BeginDict, len)?;
        Ok(self)
    }
    #[inline]
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        encoder_write!(self, FLEncoder_BeginDict, 1)?;
        encoder_write!(self, FLEncoder_WriteKey, variant.into())?;
        encoder_write!(self, FLEncoder_BeginDict, len)?;
        Ok(self)
    }
    #[inline]
    fn collect_str<T: ?Sized>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: Display,
    {
        self.serialize_str(&value.to_string())
    }
}

impl<'a, 'b> ser::SerializeSeq for &'a mut Serializer<'b> {
    type Ok = ();
    type Error = Error;

    /// Serialize a single element of the sequence.
    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(&mut **self)
    }

    /// Close the sequence.
    fn end(self) -> Result<(), Self::Error> {
        encoder_write!(self, FLEncoder_EndArray)
    }
}

impl<'a, 'b> ser::SerializeTuple for &'a mut Serializer<'b> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Self::Error> {
        encoder_write!(self, FLEncoder_EndArray)
    }
}

impl<'a, 'b> ser::SerializeTupleStruct for &'a mut Serializer<'b> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Self::Error> {
        encoder_write!(self, FLEncoder_EndArray)
    }
}

/// Tuple variants are a little different. Refer back to the
/// `serialize_tuple_variant` method above:
///
///    self.output += "{";
///    variant.serialize(&mut *self)?;
///    self.output += ":[";
///
/// So the `end` method in this impl is responsible for closing both the `]` and
/// the `}`.
impl<'a, 'b> ser::SerializeTupleVariant for &'a mut Serializer<'b> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Self::Error> {
        encoder_write!(self, FLEncoder_EndArray)?;
        encoder_write!(self, FLEncoder_EndDict)
    }
}

/// Structs are like maps in which the keys are constrained to be compile-time
/// constant strings.
impl<'a, 'b> ser::SerializeStruct for &'a mut Serializer<'b> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        encoder_write!(self, FLEncoder_WriteKey, key.into())?;
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Self::Error> {
        encoder_write!(self, FLEncoder_EndDict)
    }
}

/// Similar to `SerializeTupleVariant`, here the `end` method is responsible for
/// closing both of the curly braces opened by `serialize_struct_variant`.
impl<'a, 'b> ser::SerializeStructVariant for &'a mut Serializer<'b> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        encoder_write!(self, FLEncoder_WriteKey, key.into())?;
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Self::Error> {
        encoder_write!(self, FLEncoder_EndDict)?;
        encoder_write!(self, FLEncoder_EndDict)
    }
}
