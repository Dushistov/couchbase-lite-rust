use crate::{
    ffi::{FLEncoder_EndDict, FLEncoder_GetError, FLEncoder_WriteKey},
    ser::Serializer,
    Error,
};
use itoa::Integer;
use ryu::Float;
use serde::{ser, Serialize};

pub(crate) struct MapKeySerializer<'a> {
    pub(crate) ser: &'a mut Serializer,
}

pub struct InvalidKey;

impl<'a, 'b> ser::Serializer for &'a mut MapKeySerializer<'b>
where
    'b: 'a,
{
    type Ok = ();
    type Error = Error;
    type SerializeSeq = InvalidKey;
    type SerializeTuple = InvalidKey;
    type SerializeTupleStruct = InvalidKey;
    type SerializeTupleVariant = InvalidKey;
    type SerializeMap = InvalidKey;
    type SerializeStruct = InvalidKey;
    type SerializeStructVariant = InvalidKey;

    fn serialize_bool(self, v: bool) -> Result<Self::Ok, Self::Error> {
        encoder_write!(
            self.ser,
            FLEncoder_WriteKey,
            if v { "true" } else { "false" }.into()
        )
    }

    fn serialize_i8(self, v: i8) -> Result<Self::Ok, Self::Error> {
        itoa_write_key(self.ser, v)
    }

    fn serialize_i16(self, v: i16) -> Result<Self::Ok, Self::Error> {
        itoa_write_key(self.ser, v)
    }

    fn serialize_i32(self, v: i32) -> Result<Self::Ok, Self::Error> {
        itoa_write_key(self.ser, v)
    }

    fn serialize_i64(self, v: i64) -> Result<Self::Ok, Self::Error> {
        itoa_write_key(self.ser, v)
    }

    fn serialize_u8(self, v: u8) -> Result<Self::Ok, Self::Error> {
        itoa_write_key(self.ser, v)
    }

    fn serialize_u16(self, v: u16) -> Result<Self::Ok, Self::Error> {
        itoa_write_key(self.ser, v)
    }

    fn serialize_u32(self, v: u32) -> Result<Self::Ok, Self::Error> {
        itoa_write_key(self.ser, v)
    }

    fn serialize_u64(self, v: u64) -> Result<Self::Ok, Self::Error> {
        itoa_write_key(self.ser, v)
    }

    fn serialize_f32(self, v: f32) -> Result<Self::Ok, Self::Error> {
        ryu_write_key(self.ser, v)
    }

    fn serialize_f64(self, v: f64) -> Result<Self::Ok, Self::Error> {
        ryu_write_key(self.ser, v)
    }

    fn serialize_char(self, v: char) -> Result<Self::Ok, Self::Error> {
        let mut tmp = [0u8; 4];
        let s: &str = v.encode_utf8(&mut tmp);
        encoder_write!(self.ser, FLEncoder_WriteKey, s.into())
    }

    fn serialize_str(self, v: &str) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self.ser, FLEncoder_WriteKey, v.into())
    }

    fn serialize_bytes(self, _v: &[u8]) -> Result<Self::Ok, Self::Error> {
        Err(Error::Unsupported("key must be a string (bytes)"))
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        Err(Error::Unsupported("key must be a string (none)"))
    }

    fn serialize_some<T>(self, _value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        Err(Error::Unsupported("key must be a string (some)"))
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Err(Error::Unsupported("key must be a string (unit)"))
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        Err(Error::Unsupported("key must be a string (unit struct)"))
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        encoder_write!(self.ser, FLEncoder_WriteKey, variant.into())
    }

    fn serialize_newtype_struct<T>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(&mut *self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        Err(Error::Unsupported("key must be a string (newtype variant)"))
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Err(Error::Unsupported("key must be a string (seq)"))
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Err(Error::Unsupported("key must be a string (tuple)"))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Err(Error::Unsupported("key must be a string (tuple struct)"))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(Error::Unsupported("key must be a string (tuple variant)"))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Err(Error::Unsupported("key must be a string (map)"))
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Err(Error::Unsupported("key must be a string (struct)"))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(Error::Unsupported("key must be a string (struct variant)"))
    }
}

/// Some `Serialize` types are not able to hold a key and value in memory at the
/// same time so `SerializeMap` implementations are required to support
/// `serialize_key` and `serialize_value` individually.
///
/// There is a third optional method on the `SerializeMap` trait. The
/// `serialize_entry` method allows serializers to optimize for the case where
/// key and value are both available simultaneously. In JSON it doesn't make a
/// difference so the default behavior for `serialize_entry` is fine.
impl<'a> ser::SerializeMap for MapKeySerializer<'a> {
    type Ok = ();
    type Error = Error;

    /// The Serde data model allows map keys to be any serializable type. JSON
    /// only allows string keys so the implementation below will produce invalid
    /// JSON if the key serializes as something other than a string.
    ///
    /// A real JSON serializer would need to validate that map keys are strings.
    /// This can be done by using a different Serializer to serialize the key
    /// (instead of `&mut **self`) and having that other serializer only
    /// implement `serialize_str` and return an error on any other data type.
    fn serialize_key<T>(&mut self, key: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        key.serialize(&mut *self)
    }

    /// It doesn't make a difference whether the colon is printed at the end of
    /// `serialize_key` or at the beginning of `serialize_value`. In this case
    /// the code is a bit simpler having it here.
    fn serialize_value<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(&mut *self.ser)
    }

    fn end(self) -> Result<(), Self::Error> {
        encoder_write!(self.ser, FLEncoder_EndDict)
    }
}

fn itoa_write_key<T: Integer>(ser: &mut Serializer, v: T) -> Result<(), Error> {
    let mut buffer = itoa::Buffer::new();
    let s = buffer.format(v);
    encoder_write!(ser, FLEncoder_WriteKey, s.into())
}

fn ryu_write_key<T: Float>(ser: &mut Serializer, v: T) -> Result<(), Error> {
    let mut buffer = ryu::Buffer::new();
    let s = buffer.format_finite(v);
    encoder_write!(ser, FLEncoder_WriteKey, s.into())
}

impl ser::SerializeMap for InvalidKey {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T>(&mut self, _key: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        unreachable!()
    }

    fn serialize_value<T>(&mut self, _value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        unreachable!()
    }

    fn end(self) -> Result<(), Self::Error> {
        unreachable!()
    }
}

impl ser::SerializeSeq for InvalidKey {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T>(&mut self, _value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        unreachable!()
    }
    fn end(self) -> Result<(), Self::Error> {
        unreachable!()
    }
}

impl ser::SerializeTuple for InvalidKey {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T>(&mut self, _value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        unreachable!()
    }
    fn end(self) -> Result<(), Self::Error> {
        unreachable!()
    }
}

impl ser::SerializeTupleStruct for InvalidKey {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, _value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        unreachable!()
    }

    fn end(self) -> Result<(), Self::Error> {
        unreachable!()
    }
}

impl ser::SerializeTupleVariant for InvalidKey {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T>(&mut self, _value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        unreachable!()
    }
    fn end(self) -> Result<(), Self::Error> {
        unreachable!()
    }
}

impl ser::SerializeStruct for InvalidKey {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T>(&mut self, _key: &'static str, _value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        unreachable!()
    }
    fn end(self) -> Result<(), Self::Error> {
        unreachable!()
    }
}

impl ser::SerializeStructVariant for InvalidKey {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T>(&mut self, _key: &'static str, _value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        unreachable!()
    }
    fn end(self) -> Result<(), Self::Error> {
        unreachable!()
    }
}
