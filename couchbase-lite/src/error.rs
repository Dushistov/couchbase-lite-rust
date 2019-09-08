use crate::{
    ffi::{
        c4error_getDescription, c4error_getMessage, kC4MaxErrorDomainPlus1, C4Error, C4ErrorDomain,
    },
    fl_slice::FlSliceOwner,
};
use std::fmt;

/// Enum listing possible errors.
pub enum Error {
    /// couchbase-lite-core error
    DbError(C4Error),
    /// UTF-8 encoding problem
    Utf8,
}

impl std::error::Error for Error {}

pub(crate) type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::DbError(err) => {
                let (msg, desc) = into_msg_desc(*err);
                write!(
                    fmt,
                    "c4 error {}: {}",
                    desc.as_utf8_lossy(),
                    msg.as_utf8_lossy()
                )
            }
            Error::Utf8 => write!(fmt, "utf8 encoding/decoding error"),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::DbError(err) => {
                let (msg, desc) = into_msg_desc(*err);
                write!(
                    fmt,
                    "{:?} /  {}: {}",
                    *err,
                    desc.as_utf8_lossy(),
                    msg.as_utf8_lossy()
                )
            }
            Error::Utf8 => write!(fmt, "utf8 encoding/decoding error"),
        }
    }
}

impl From<C4Error> for Error {
    fn from(err: C4Error) -> Self {
        Error::DbError(err)
    }
}

fn into_msg_desc(err: C4Error) -> (FlSliceOwner, FlSliceOwner) {
    let msg: FlSliceOwner = unsafe { c4error_getMessage(err) }.into();
    let desc: FlSliceOwner = unsafe { c4error_getDescription(err) }.into();
    (msg, desc)
}

#[inline]
pub(crate) fn c4error_init() -> C4Error {
    C4Error {
        domain: kC4MaxErrorDomainPlus1 as C4ErrorDomain,
        code: 0,
        internal_info: 0,
    }
}
