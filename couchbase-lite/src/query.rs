use crate::{
    error::{c4error_init, Error},
    ffi::{
        c4query_free, c4query_new, c4query_new2, c4query_run, c4query_setParameters,
        c4queryenum_free, c4queryenum_next, kC4DefaultQueryOptions, kC4N1QLQuery, C4Query,
        C4QueryEnumerator, FLArrayIterator_GetCount, FLArrayIterator_GetValueAt,
    },
    fl_slice::{fl_slice_empty, AsFlSlice},
    value::{FromValueRef, ValueRef},
    Database, Result,
};
use fallible_streaming_iterator::FallibleStreamingIterator;
use serde::Serialize;
use std::ptr::NonNull;

pub struct Query<'db> {
    _db: &'db Database,
    inner: NonNull<C4Query>,
}

impl Drop for Query<'_> {
    fn drop(&mut self) {
        unsafe { c4query_free(self.inner.as_ptr()) };
    }
}

impl Query<'_> {
    pub(crate) fn new<'a, 'b>(db: &'a Database, query_json: &'b str) -> Result<Query<'a>> {
        let mut c4err = c4error_init();
        let query = unsafe {
            c4query_new(
                db.inner.0.as_ptr(),
                query_json.as_bytes().as_flslice(),
                &mut c4err,
            )
        };

        NonNull::new(query)
            .map(|inner| Query { _db: db, inner })
            .ok_or_else(|| c4err.into())
    }

    pub(crate) fn new_n1ql<'a, 'b>(db: &'a Database, query_n1ql: &'b str) -> Result<Query<'a>> {
        let mut c4err = c4error_init();
        let mut out_error_pos: std::os::raw::c_int = -1;
        let query = unsafe {
            c4query_new2(
                db.inner.0.as_ptr(),
                kC4N1QLQuery,
                query_n1ql.as_bytes().as_flslice(),
                &mut out_error_pos,
                &mut c4err,
            )
        };

        NonNull::new(query)
            .map(|inner| Query { _db: db, inner })
            .ok_or_else(|| c4err.into())
    }

    pub fn set_parameters<T>(&self, parameters: &T) -> Result<()>
    where
        T: Serialize,
    {
        let param_string = serde_json::to_string(parameters)?;
        let param_slice = param_string.as_bytes().as_flslice();
        unsafe {
            c4query_setParameters(self.inner.as_ptr(), param_slice);
        }
        Ok(())
    }

    pub fn run(&self) -> Result<Enumerator> {
        let mut c4err = c4error_init();
        let it = unsafe {
            c4query_run(
                self.inner.as_ptr(),
                &kC4DefaultQueryOptions,
                fl_slice_empty(),
                &mut c4err,
            )
        };

        NonNull::new(it)
            .map(|inner| Enumerator {
                _query: self,
                reach_end: false,
                inner,
            })
            .ok_or_else(|| c4err.into())
    }
}

pub struct Enumerator<'query> {
    _query: &'query Query<'query>,
    reach_end: bool,
    inner: NonNull<C4QueryEnumerator>,
}

impl Drop for Enumerator<'_> {
    fn drop(&mut self) {
        unsafe { c4queryenum_free(self.inner.as_ptr()) };
    }
}

impl<'en> FallibleStreamingIterator for Enumerator<'en> {
    type Error = crate::error::Error;
    type Item = Enumerator<'en>;

    fn advance(&mut self) -> Result<()> {
        if self.reach_end {
            return Ok(());
        }
        let mut c4err = c4error_init();
        if unsafe { c4queryenum_next(self.inner.as_ptr(), &mut c4err) } {
            Ok(())
        } else {
            if c4err.code == 0 {
                self.reach_end = true;
                Ok(())
            } else {
                Err(c4err.into())
            }
        }
    }

    fn get(&self) -> Option<&Enumerator<'en>> {
        if !self.reach_end {
            Some(self)
        } else {
            None
        }
    }
}

impl<'a> Enumerator<'a> {
    pub fn get_raw_checked(&self, i: u32) -> Result<ValueRef<'a>> {
        let n = unsafe { FLArrayIterator_GetCount(&self.inner.as_ref().columns) };
        if i >= n {
            return Err(Error::LogicError(format!(
                "Enumerator::get_raw_checked: Index out of bounds {} / {}",
                i, n
            )));
        }

        let val: ValueRef =
            unsafe { FLArrayIterator_GetValueAt(&self.inner.as_ref().columns, i) }.into();
        Ok(val)
    }

    pub fn get_checked<T>(&self, i: u32) -> Result<T>
    where
        T: FromValueRef<'a>,
    {
        let value_ref = self.get_raw_checked(i)?;
        FromValueRef::column_result(value_ref)
    }
}
