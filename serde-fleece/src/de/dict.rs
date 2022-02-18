use super::NonNullConst;
use crate::{
    de::Deserializer,
    ffi::{
        FLDictIterator, FLDictIterator_Begin, FLDictIterator_End, FLDictIterator_GetCount,
        FLDictIterator_GetKeyString, FLDictIterator_GetValue, FLDictIterator_Next, FLDict_Get,
        _FLDict,
    },
    Error,
};
use serde::de;
use std::{marker::PhantomData, mem::MaybeUninit, str::FromStr};

/// Can not use `DictAccess`, because of order of fields
/// is not defined in fleee's dict, but defined in struct.
pub(crate) struct StructAccess<'a> {
    dict: NonNullConst<_FLDict>,
    fields: &'static [&'static str],
    i: usize,
    marker: PhantomData<&'a [u8]>,
}

impl<'a> StructAccess<'a> {
    pub fn new(dict: NonNullConst<_FLDict>, fields: &'static [&'static str]) -> Self {
        Self {
            dict,
            fields,
            i: 0,
            marker: PhantomData,
        }
    }
}

impl<'a, 'de> de::SeqAccess<'de> for StructAccess<'a> {
    type Error = Error;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: de::DeserializeSeed<'de>,
    {
        if let Some(key) = self.fields.get(self.i) {
            unsafe {
                let value = FLDict_Get(self.dict.as_ptr(), (*key).into());
                let value = NonNullConst::new(value).ok_or_else(|| {
                    Error::InvalidFormat(format!("missing field `{}` in fleece dict", key).into())
                })?;
                let value = seed.deserialize(&mut Deserializer::new(value))?;
                self.i += 1;
                Ok(Some(value))
            }
        } else {
            Ok(None)
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.fields.len())
    }
}

pub(crate) struct EnumAccess<'a> {
    dict: NonNullConst<_FLDict>,
    marker: PhantomData<&'a [u8]>,
}

impl<'a> EnumAccess<'a> {
    pub fn new(dict: NonNullConst<_FLDict>) -> Self {
        Self {
            dict,
            marker: PhantomData,
        }
    }
}

impl<'de, 'a> de::EnumAccess<'de> for EnumAccess<'a> {
    type Error = Error;
    type Variant = Deserializer<'de>;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error>
    where
        V: de::DeserializeSeed<'de>,
    {
        let mut it = MaybeUninit::<FLDictIterator>::uninit();
        unsafe {
            FLDictIterator_Begin(self.dict.as_ptr(), it.as_mut_ptr());
            let mut it = it.assume_init();
            let n = FLDictIterator_GetCount(&it);
            if n != 1 {
                FLDictIterator_End(&mut it);
                return Err(Error::InvalidFormat(
                    format!("enum should be dict with len 1, got {}", n).into(),
                ));
            }
            let key: &str = FLDictIterator_GetKeyString(&it).try_into()?;
            let key = <&str as de::IntoDeserializer<'_, Error>>::into_deserializer(key);
            let key = seed.deserialize(key)?;
            let value = FLDictIterator_GetValue(&it);
            let value = NonNullConst::new(value).ok_or_else(|| {
                Error::InvalidFormat("not expecting null value in enum dict".into())
            })?;
            Ok((key, Deserializer::new(value)))
        }
    }
}

impl<'de> de::VariantAccess<'de> for Deserializer<'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Self::Error> {
        // should be handled before
        unreachable!()
    }

    fn newtype_variant_seed<T>(mut self, seed: T) -> Result<T::Value, Self::Error>
    where
        T: de::DeserializeSeed<'de>,
    {
        seed.deserialize(&mut self)
    }

    fn tuple_variant<V>(mut self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        de::Deserializer::deserialize_seq(&mut self, visitor)
    }

    fn struct_variant<V>(
        mut self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        de::Deserializer::deserialize_struct(&mut self, "enum struct variant", fields, visitor)
    }
}

pub(crate) struct DictAccess<'a> {
    n: usize,
    it: FLDictIterator,
    marker: PhantomData<&'a [u8]>,
}

impl<'a> Drop for DictAccess<'a> {
    fn drop(&mut self) {
        // not strictly necessary, just to be safe
        unsafe { FLDictIterator_End(&mut self.it) };
    }
}

impl<'a> DictAccess<'a> {
    pub fn new(dict: &'a _FLDict, n: usize) -> Self {
        let mut it = MaybeUninit::<FLDictIterator>::uninit();
        let it = unsafe {
            FLDictIterator_Begin(dict, it.as_mut_ptr());
            it.assume_init()
        };
        Self {
            n,
            it,
            marker: PhantomData,
        }
    }
}

impl<'a, 'de> de::MapAccess<'de> for DictAccess<'a> {
    type Error = Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: de::DeserializeSeed<'de>,
    {
        if unsafe { FLDictIterator_GetCount(&self.it) } > 0 {
            let key: &str = unsafe { FLDictIterator_GetKeyString(&self.it) }.try_into()?;
            let key = de::DeserializeSeed::deserialize(seed, DictKeySerializer(key))?;
            Ok(Some(key))
        } else {
            Ok(None)
        }
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: de::DeserializeSeed<'de>,
    {
        unsafe {
            debug_assert!(FLDictIterator_GetCount(&self.it) > 0);
            let value = FLDictIterator_GetValue(&self.it);
            let value = NonNullConst::new(value)
                .ok_or_else(|| Error::InvalidFormat("not expecting null value in dict".into()))?;
            let value = de::DeserializeSeed::deserialize(seed, &mut Deserializer::new(value))?;
            FLDictIterator_Next(&mut self.it);
            Ok(value)
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.n)
    }
}

macro_rules! visit_from_str {
    ($this:ident, $v:ident, $ty:ty, $m:ident) => {{
        let val = <$ty>::from_str($this.0)?;
        $v.$m(val)
    }};
}

struct DictKeySerializer<'a>(&'a str);

impl<'de, 'a> de::Deserializer<'de> for DictKeySerializer<'a> {
    type Error = Error;

    fn deserialize_any<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported(
            "DictKeySerializer can not work with any",
        ))
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visit_from_str!(self, visitor, bool, visit_bool)
    }

    fn deserialize_i8<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visit_from_str!(self, visitor, i8, visit_i8)
    }

    fn deserialize_i16<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visit_from_str!(self, visitor, i16, visit_i16)
    }

    fn deserialize_i32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visit_from_str!(self, visitor, i32, visit_i32)
    }

    fn deserialize_i64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visit_from_str!(self, visitor, i64, visit_i64)
    }

    fn deserialize_u8<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visit_from_str!(self, visitor, u8, visit_u8)
    }

    fn deserialize_u16<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visit_from_str!(self, visitor, u16, visit_u16)
    }

    fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visit_from_str!(self, visitor, u32, visit_u32)
    }

    fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visit_from_str!(self, visitor, u64, visit_u64)
    }

    fn deserialize_f32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visit_from_str!(self, visitor, f32, visit_f32)
    }

    fn deserialize_f64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visit_from_str!(self, visitor, f64, visit_f64)
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visit_from_str!(self, visitor, char, visit_char)
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_str(self.0)
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_str(self.0)
    }

    fn deserialize_bytes<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported(
            "Can not deserialize dict key from bytes",
        ))
    }

    fn deserialize_byte_buf<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported(
            "Can not deserialize dict key from byte buf",
        ))
    }

    fn deserialize_option<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported(
            "Can not deserialize dict key from option",
        ))
    }

    fn deserialize_unit<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported("Can not deserialize dict key from unit"))
    }

    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported(
            "Can not deserialize dict key from unit struct",
        ))
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

    fn deserialize_seq<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported("Can not deserialize dict key from seq"))
    }

    fn deserialize_tuple<V>(self, _len: usize, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported(
            "Can not deserialize dict key from tuple",
        ))
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported(
            "Can not deserialize dict key from tuple struct",
        ))
    }

    fn deserialize_map<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported("Can not deserialize dict key from map"))
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported(
            "Can not deserialize dict key from struct",
        ))
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported("Can not deserialize dict key from enum"))
    }

    fn deserialize_identifier<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported(
            "Can not deserialize dict key from identifier",
        ))
    }

    fn deserialize_ignored_any<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: de::Visitor<'de>,
    {
        Err(Error::Unsupported("Can not deserialize dict key from any"))
    }
}
