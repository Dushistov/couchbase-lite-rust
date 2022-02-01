use crate::{
    de::Deserializer,
    ffi::{FLArray_Get, _FLArray, _FLValue},
    Error,
};
use serde::de;

pub(crate) struct ArrayAccess<'a> {
    arr: &'a _FLArray,
    n: usize,
    i: usize,
}

impl<'a> ArrayAccess<'a> {
    pub fn new(arr: &'a _FLArray, n: usize) -> Self {
        Self { arr, n, i: 0 }
    }
}

impl<'de, 'a> de::SeqAccess<'de> for ArrayAccess<'a> {
    type Error = Error;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Error>
    where
        T: de::DeserializeSeed<'de>,
    {
        if self.i < self.n {
            let value = unsafe { FLArray_Get(self.arr, self.i as u32) };
            if value.is_null() {
                return Err(Error::InvalidFormat(
                    "not expecting null value in array".into(),
                ));
            }
            let value: &_FLValue = unsafe { &*value };
            let value = seed.deserialize(&mut Deserializer { value }).map(Some)?;
            self.i += 1;
            Ok(value)
        } else {
            Ok(None)
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.n)
    }
}
