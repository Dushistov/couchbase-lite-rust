use crate::ffi::FLError;
use std::{
    borrow::Cow,
    char::ParseCharError,
    fmt::{self, Display},
    num::{ParseFloatError, ParseIntError},
    str::{ParseBoolError, Utf8Error},
};

#[derive(Debug)]
pub enum Error {
    Fleece(FLError),
    Custom(String),
    Unsupported(&'static str),
    InvalidFormat(Cow<'static, str>),
}

impl From<FLError> for Error {
    fn from(v: FLError) -> Self {
        Error::Fleece(v)
    }
}

impl From<Utf8Error> for Error {
    fn from(_: Utf8Error) -> Self {
        Error::InvalidFormat("not valid utf-8".into())
    }
}

impl From<ParseBoolError> for Error {
    fn from(err: ParseBoolError) -> Self {
        Error::InvalidFormat(format!("parsing of bool failed: {}", err).into())
    }
}

impl From<ParseCharError> for Error {
    fn from(err: ParseCharError) -> Self {
        Error::InvalidFormat(format!("parsing of char failed: {}", err).into())
    }
}

impl From<ParseIntError> for Error {
    fn from(err: ParseIntError) -> Self {
        Error::InvalidFormat(format!("parsing of integer failed: {}", err).into())
    }
}

impl From<ParseFloatError> for Error {
    fn from(err: ParseFloatError) -> Self {
        Error::InvalidFormat(format!("parsing of float failed: {}", err).into())
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Fleece(err) => {
                let msg = match *err {
                    FLError::kFLNoError => "No error",
                    FLError::kFLMemoryError => "Out of memory, or allocation failed",
                    FLError::kFLOutOfRange => " Array index or iterator out of range",
                    FLError::kFLInvalidData => "Bad input data (NaN, non-string key, etc.)",
                    FLError::kFLEncodeError => {
                        "Structural error encoding (missing value, too many ends, etc.)"
                    }
                    FLError::kFLJSONError => "Error parsing JSON",
                    FLError::kFLUnknownValue => {
                        "Unparseable data in a Value (corrupt? Or from some distant future?)"
                    }
                    FLError::kFLInternalError => "Something that shouldn't happen",
                    FLError::kFLNotFound => "Key not found",
                    FLError::kFLSharedKeysStateError => {
                        "Misuse of shared keys (not in transaction, etc.)"
                    }
                    FLError::kFLPOSIXError => "Posix error",
                    FLError::kFLUnsupported => "Operation is unsupported",
                    _ => "Unknown fleece error",
                };
                write!(f, "FLeece error: {}", msg)
            }
            Error::Custom(msg) => write!(f, "Custom error: {}", msg),
            Error::Unsupported(msg) => write!(f, "Unsupporte operation: {}", msg),
            Error::InvalidFormat(msg) => write!(f, "invalid fleece data: {}", msg),
        }
    }
}

impl std::error::Error for Error {}

impl serde::ser::Error for Error {
    fn custom<T>(msg: T) -> Self
    where
        T: std::fmt::Display,
    {
        Self::Custom(msg.to_string())
    }
}

impl serde::de::Error for Error {
    fn custom<T>(msg: T) -> Self
    where
        T: Display,
    {
        Self::Custom(msg.to_string())
    }
}
