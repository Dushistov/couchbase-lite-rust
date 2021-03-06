#![allow(non_upper_case_globals)]

use crate::ffi::{
    c4log_setRustCallback, kC4DatabaseLog, kC4DefaultLog, kC4QueryLog, kC4SyncLog, kC4WebSocketLog,
    C4LogDomain, C4LogLevel,
};
use once_cell::sync::Lazy;
use std::{ffi::CStr, os::raw::c_char};

macro_rules! define_log_level {
    ($const_name:ident) => {
        const $const_name: C4LogLevel = crate::ffi::$const_name as C4LogLevel;
    };
}

define_log_level!(kC4LogDebug);
define_log_level!(kC4LogVerbose);
define_log_level!(kC4LogInfo);
define_log_level!(kC4LogWarning);
define_log_level!(kC4LogError);
define_log_level!(kC4LogNone);

pub(crate) static DB_LOGGER: Lazy<()> = Lazy::new(|| {
    unsafe { c4log_setRustCallback(kC4LogDebug as C4LogLevel, Some(db_logger_callback)) };
});

unsafe extern "C" fn db_logger_callback(
    domain: C4LogDomain,
    level: C4LogLevel,
    msg: *const c_char,
) {
    let domain_name = if domain == kC4DefaultLog {
        "def"
    } else if domain == kC4DatabaseLog {
        "db"
    } else if domain == kC4QueryLog {
        "query"
    } else if domain == kC4SyncLog {
        "sync"
    } else if domain == kC4WebSocketLog {
        "websock"
    } else {
        "unkndmn"
    };

    use log::Level::*;

    let level = match level {
        kC4LogDebug => Trace,
        kC4LogVerbose => Debug,
        kC4LogInfo => Info,
        kC4LogWarning => Warn,
        kC4LogError | kC4LogNone => Error,
        _ => Info,
    };

    if !msg.is_null() {
        fn lifetime_marker<'a>(ptr_ref: &'a *const c_char) -> &'a CStr {
            unsafe { CStr::from_ptr(*ptr_ref) }
        }
        let msg = lifetime_marker(&msg);
        log::log!(level, "{} {}", domain_name, msg.to_string_lossy());
    } else {
        log::log!(level, "{} <null>", domain_name);
    }
}
