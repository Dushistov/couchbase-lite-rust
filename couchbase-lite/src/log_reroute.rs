use crate::ffi::{
    c4log_setRustCallback, kC4DatabaseLog, kC4DefaultLog, kC4LogDebug, kC4LogError, kC4LogInfo,
    kC4LogNone, kC4LogVerbose, kC4LogWarning, kC4QueryLog, kC4SyncLog, kC4WebSocketLog,
    C4LogDomain, C4LogLevel,
};
use once_cell::sync::Lazy;
use std::{ffi::CStr, os::raw::c_char};

pub(crate) static DB_LOGGER: Lazy<()> = Lazy::new(|| {
    unsafe { c4log_setRustCallback(kC4LogDebug as C4LogLevel, Some(db_logger_callback)) };
    ()
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
        "unknown"
    };

    use log::Level::*;

    let level = if level == (kC4LogDebug as C4LogLevel) {
        Trace
    } else if level == (kC4LogVerbose as C4LogLevel) {
        Debug
    } else if level == (kC4LogInfo as C4LogLevel) {
        Info
    } else if level == (kC4LogWarning as C4LogLevel) {
        Warn
    } else if level == (kC4LogError as C4LogLevel) {
        Error
    } else if level == (kC4LogNone as C4LogLevel) {
        Info
    } else {
        Info
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
