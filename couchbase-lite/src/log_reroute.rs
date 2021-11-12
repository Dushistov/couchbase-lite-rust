#![allow(non_upper_case_globals)]

use crate::ffi::{
    c4log_setRustCallback, kC4DatabaseLog, kC4DefaultLog, kC4QueryLog, kC4SyncLog, kC4WebSocketLog,
    C4LogDomain, C4LogLevel,
};
use once_cell::sync::Lazy;
use std::{ffi::CStr, os::raw::c_char};

pub(crate) static DB_LOGGER: Lazy<()> = Lazy::new(|| {
    unsafe { c4log_setRustCallback(C4LogLevel::kC4LogDebug, Some(db_logger_callback)) };
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
        C4LogLevel::kC4LogDebug => Trace,
        C4LogLevel::kC4LogVerbose => Debug,
        C4LogLevel::kC4LogInfo => Info,
        C4LogLevel::kC4LogWarning => Warn,
        C4LogLevel::kC4LogError | C4LogLevel::kC4LogNone => Error,
        _ => Info,
    };

    if !msg.is_null() {
        fn lifetime_marker(ptr_ref: &*const c_char) -> &CStr {
            unsafe { CStr::from_ptr(*ptr_ref) }
        }
        let msg = lifetime_marker(&msg);
        log::log!(level, "{} {}", domain_name, msg.to_string_lossy());
    } else {
        log::log!(level, "{} <null>", domain_name);
    }
}
