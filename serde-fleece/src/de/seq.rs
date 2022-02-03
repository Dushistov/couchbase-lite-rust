use std::marker::PhantomData;

use crate::{
    de::Deserializer,
    ffi::{FLArray_Get, _FLArray},
    Error,
};
use serde::de;

use super::NonNullConst;

pub(crate) struct ArrayAccess<'a> {
    arr: NonNullConst<_FLArray>,
    n: usize,
    i: usize,
    marker: PhantomData<&'a [u8]>,
}

impl<'a> ArrayAccess<'a> {
    pub fn new(arr: NonNullConst<_FLArray>, n: usize) -> Self {
        Self {
            arr,
            n,
            i: 0,
            marker: PhantomData,
        }
    }
}

impl<'de, 'a> de::SeqAccess<'de> for ArrayAccess<'a> {
    type Error = Error;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Error>
    where
        T: de::DeserializeSeed<'de>,
    {
        if self.i < self.n {
            let value = unsafe { FLArray_Get(self.arr.as_ptr(), self.i as u32) };
            let value = NonNullConst::new(value)
                .ok_or_else(|| Error::InvalidFormat("not expecting null value in array".into()))?;
            let value = seed.deserialize(&mut Deserializer::new(value)).map(Some)?;
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
