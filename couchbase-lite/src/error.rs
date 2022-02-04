use crate::ffi::{
    c4error_getDescription, c4error_getMessage, C4Error, C4ErrorDomain, FLSliceResult,
};
use std::{fmt, os::raw::c_int};

/// Enum listing possible errors.
pub enum Error {
    /// couchbase-lite-core error
    C4Error(C4Error),
    /// UTF-8 decoding problem
    InvalidUtf8,
    /// some invariant was broken
    LogicError(String),
    SerdeFleece(serde_fleece::Error),
    /// argument contains 0 character
    NulError(std::ffi::NulError),
    InvalidQuery {
        pos: c_int,
        query_expr: String,
        err: C4Error,
    },
}

impl std::error::Error for Error {}

pub(crate) type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::C4Error(err) => {
                let (msg, desc) = into_msg_desc(*err);
                write!(
                    fmt,
                    "c4 error {}: {}",
                    desc.as_utf8_lossy(),
                    msg.as_utf8_lossy()
                )
            }
            Error::InvalidUtf8 => fmt.write_str("Utf8 encoding error"),
            Error::LogicError(msg) => write!(fmt, "logic error: {}", msg),
            Error::SerdeFleece(err) => write!(fmt, "serde+flecce error: {}", err),
            Error::NulError(err) => write!(fmt, "argument contains null character: {}", err),
            Error::InvalidQuery {
                pos,
                query_expr,
                err,
            } => {
                let (msg, desc) = into_msg_desc(*err);
                write!(
                    fmt,
                    "Can not parse query {}, error at {}: {} {}",
                    query_expr,
                    pos,
                    desc.as_utf8_lossy(),
                    msg.as_utf8_lossy()
                )
            }
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::C4Error(err) => {
                let (msg, desc) = into_msg_desc(*err);
                write!(
                    fmt,
                    "{:?} /  {}: {}",
                    *err,
                    desc.as_utf8_lossy(),
                    msg.as_utf8_lossy()
                )
            }
            Error::InvalidUtf8 => write!(fmt, "Invalid UTF-8 error"),
            Error::LogicError(msg) => write!(fmt, "LogicError: {}", msg),
            Error::SerdeFleece(err) => write!(fmt, "SerdeFleece error: {}", err),
            Error::NulError(err) => write!(fmt, "NulError: {:?}", err),
            Error::InvalidQuery {
                pos,
                query_expr,
                err,
            } => {
                let (msg, desc) = into_msg_desc(*err);
                write!(
                    fmt,
                    "InvalidQuery {{ query_expr {}, pos {}, err {:?} / {}: {} }}",
                    query_expr,
                    pos,
                    *err,
                    desc.as_utf8_lossy(),
                    msg.as_utf8_lossy()
                )
            }
        }
    }
}

impl From<C4Error> for Error {
    fn from(err: C4Error) -> Self {
        Error::C4Error(err)
    }
}

impl From<serde_fleece::Error> for Error {
    fn from(err: serde_fleece::Error) -> Self {
        Error::SerdeFleece(err)
    }
}

impl From<std::ffi::NulError> for Error {
    fn from(err: std::ffi::NulError) -> Self {
        Error::NulError(err)
    }
}

#[inline]
pub(crate) fn c4error_init() -> C4Error {
    C4Error {
        domain: C4ErrorDomain::kC4MaxErrorDomainPlus1,
        code: 0,
        internal_info: 0,
    }
}

fn into_msg_desc(err: C4Error) -> (FLSliceResult, FLSliceResult) {
    let msg = unsafe { c4error_getMessage(err) };
    let desc = unsafe { c4error_getDescription(err) };
    (msg, desc)
}
