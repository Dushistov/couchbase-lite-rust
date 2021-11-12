use crate::{
    ffi::{c4error_getDescription, c4error_getMessage, C4Error, C4ErrorDomain, FLError},
    fl_slice::FlSliceOwner,
};
use std::fmt;

/// Enum listing possible errors.
pub enum Error {
    /// couchbase-lite-core error
    DbError(C4Error),
    /// UTF-8 encoding problem
    Utf8,
    /// `serde_json::Error`
    SerdeJson(serde_json::Error),
    /// some invariant was broken
    LogicError(String),
    /// `json5::Error`
    Json5(json5::Error),
    /// fleece library errors
    FlError(FLError),
    /// argument contains 0 character
    NulError(std::ffi::NulError),
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
            Error::Utf8 => write!(fmt, "Utf8 encoding/decoding error"),
            Error::Json5(err) => write!(fmt, "Json5: {}", err),
            Error::LogicError(msg) => write!(fmt, "LogicError: {}", msg),
            Error::SerdeJson(err) => write!(fmt, "SerdeJson: {}", err),
            Error::FlError(err) => write!(fmt, "FlError: {}", err.0),
            Error::NulError(err) => write!(fmt, "NulError: {}", err),
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
            Error::Utf8 => write!(fmt, "Utf8 encoding/decoding error"),
            Error::Json5(err) => write!(fmt, "Json5: {:?}", err),
            Error::LogicError(msg) => write!(fmt, "LogicError: {}", msg),
            Error::SerdeJson(err) => write!(fmt, "SerdeJson: {:?}", err),
            Error::FlError(err) => write!(fmt, "FlError: {}", err.0),
            Error::NulError(err) => write!(fmt, "NulError: {:?}", err),
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
        domain: C4ErrorDomain::kC4MaxErrorDomainPlus1,
        code: 0,
        internal_info: 0,
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::SerdeJson(e)
    }
}

impl From<json5::Error> for Error {
    fn from(error: json5::Error) -> Self {
        Error::Json5(error)
    }
}

impl From<std::ffi::NulError> for Error {
    fn from(err: std::ffi::NulError) -> Self {
        Error::NulError(err)
    }
}
