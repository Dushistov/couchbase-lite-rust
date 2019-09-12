use crate::{
    error::{c4error_init, Error},
    ffi::{
        c4address_fromURL, c4repl_free, c4repl_new, c4repl_stop, kC4Continuous,
        kC4ReplicatorOptionCookies, C4Address, C4Replicator, C4ReplicatorParameters,
        C4ReplicatorStatus, FLEncoder_BeginDict, FLEncoder_EndDict, FLEncoder_Finish,
        FLEncoder_Free, FLEncoder_New, FLEncoder_WriteKey, FLEncoder_WriteString,
        FLError_kFLNoError,
    },
    fl_slice::{fl_slice_empty, AsFlSlice, FlSliceOwner},
    Database, Result,
};
use log::info;
use std::{ffi::CStr, os::raw::c_void, ptr, ptr::NonNull};

pub(crate) struct Replicator {
    inner: NonNull<C4Replicator>,
}

/// it should be safe to call replicator API from any thread
/// according to https://github.com/couchbase/couchbase-lite-core/wiki/Thread-Safety
unsafe impl Send for Replicator {}

impl Drop for Replicator {
    fn drop(&mut self) {
        unsafe { c4repl_free(self.inner.as_ptr()) };
    }
}

impl Replicator {
    /// For example: url "ws://192.168.1.132:4984/demo/"
    pub(crate) fn new(db: &Database, url: &str, token: Option<&str>) -> Result<Replicator> {
        let mut remote_addr = C4Address {
            scheme: fl_slice_empty(),
            hostname: fl_slice_empty(),
            port: 0,
            path: fl_slice_empty(),
        };
        let mut db_name = fl_slice_empty();
        if !unsafe {
            c4address_fromURL(url.as_bytes().as_flslice(), &mut remote_addr, &mut db_name)
        } {
            return Err(Error::LogicError(format!("Can not parse URL {}", url)));
        }

        let token_cookie = format!("{}={}", "SyncGatewaySession", token.unwrap_or(""));
        let option_cookies = CStr::from_bytes_with_nul(kC4ReplicatorOptionCookies)
            .expect("Invalid kC4ReplicatorOptionCookies constant");
        let options: FlSliceOwner = if token.is_some() {
            unsafe {
                let enc = FLEncoder_New();

                FLEncoder_BeginDict(enc, 1);
                FLEncoder_WriteKey(enc, option_cookies.to_bytes().as_flslice());
                FLEncoder_WriteString(enc, token_cookie.as_bytes().as_flslice());
                FLEncoder_EndDict(enc);

                let mut fl_err = FLError_kFLNoError;
                let res = FLEncoder_Finish(enc, &mut fl_err);
                FLEncoder_Free(enc);
                if fl_err != FLError_kFLNoError {
                    return Err(Error::FlError(fl_err));
                }
                res.into()
            }
        } else {
            FlSliceOwner::default()
        };

        let repl_params = C4ReplicatorParameters {
            push: kC4Continuous as i32,
            pull: kC4Continuous as i32,
            optionsDictFleece: options.as_bytes().as_flslice(),
            pushFilter: None,
            validationFunc: None,
            onStatusChanged: Some(on_status_changed),
            onDocumentsEnded: None,
            onBlobProgress: None,
            callbackContext: ptr::null_mut(),
            socketFactory: ptr::null_mut(),
            dontStart: false,
        };

        let mut c4err = c4error_init();
        let repl = unsafe {
            c4repl_new(
                db.inner.0.as_ptr(),
                remote_addr,
                db_name,
                ptr::null_mut(),
                repl_params,
                &mut c4err,
            )
        };
        NonNull::new(repl)
            .map(|inner| Replicator { inner })
            .ok_or_else(|| c4err.into())
    }

    pub(crate) fn stop(self) {
        unsafe { c4repl_stop(self.inner.as_ptr()) };
    }
}

extern "C" fn on_status_changed(
    c4_repl: *mut C4Replicator,
    status: C4ReplicatorStatus,
    ctx: *mut c_void,
) {
    info!("on_status_changed: repl {:?} {:?}", c4_repl, status);
}
