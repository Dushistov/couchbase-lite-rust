use crate::ffi::{
    c4log_getDomainName, kC4DatabaseLog, kC4DefaultLog, kC4QueryLog, kC4SyncLog, kC4WebSocketLog,
    C4LogDomain, C4LogLevel,
};
use std::{ffi::CStr, os::raw::c_char};
use va_list::VaList;

// TODO: because of https://github.com/rust-lang/rust-bindgen/issues/2154
// it is impossible to use generated by bindgen function signature
extern "C" {
    pub fn c4log_writeToCallback(
        level: C4LogLevel,
        callback: ::std::option::Option<
            unsafe extern "C" fn(
                arg1: C4LogDomain,
                arg2: C4LogLevel,
                arg3: *const ::std::os::raw::c_char,
                arg4: VaList,
            ),
        >,
        preformatted: bool,
    );
}

pub(crate) fn c4log_to_log_init() {
    unsafe { c4log_writeToCallback(C4LogLevel::kC4LogDebug, Some(db_log_callback), true) };
}

unsafe extern "C" fn db_log_callback(
    domain: C4LogDomain,
    level: C4LogLevel,
    fmt: *const c_char,
    _va: VaList,
) {
    fn lifetime_marker(ptr_ref: &*const c_char) -> &CStr {
        unsafe { CStr::from_ptr(*ptr_ref) }
    }
    let c_domain_name;
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
        c_domain_name = c4log_getDomainName(domain);
        let name = lifetime_marker(&c_domain_name);
        name.to_str().unwrap_or("unkndmn")
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

    if !fmt.is_null() {
        let msg = lifetime_marker(&fmt);
        log::log!(target: "couchbase", level, "{domain_name} {}", msg.to_string_lossy());
    } else {
        log::log!(target: "couchbase", level, "{domain_name} <null>");
    }
}
