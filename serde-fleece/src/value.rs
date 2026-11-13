use std::marker::PhantomData;

use crate::{ffi::_FLValue, NonNullConst};

/// Immutable borrow of a Fleece value and its backing data
#[repr(transparent)]
pub struct Value<'a> {
    inner: NonNullConst<_FLValue>,
    marker: PhantomData<&'a _FLValue>,
}

impl<'a> Value<'a> {
    /// # Safety
    ///
    /// The value and its data must stay valid and immutable for `'a`
    #[inline]
    pub unsafe fn new(inner: NonNullConst<_FLValue>) -> Self {
        Self {
            inner,
            marker: PhantomData,
        }
    }

    pub(crate) fn as_raw(&self) -> NonNullConst<_FLValue> {
        self.inner
    }
}
