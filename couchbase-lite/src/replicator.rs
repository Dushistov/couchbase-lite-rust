use crate::{
    error::{c4error_init, Error},
    ffi::{
        c4address_fromURL, c4repl_free, c4repl_getStatus, c4repl_new, c4repl_start, c4repl_stop,
        kC4Continuous, kC4ReplicatorOptionCookies, kC4ReplicatorOptionOutgoingConflicts, C4Address,
        C4Replicator, C4ReplicatorActivityLevel, C4ReplicatorMode, C4ReplicatorParameters,
        C4ReplicatorStatus, C4ReplicatorStatusChangedCallback, FLEncoder_BeginDict,
        FLEncoder_EndDict, FLEncoder_Finish, FLEncoder_Free, FLEncoder_New, FLEncoder_WriteBool,
        FLEncoder_WriteKey, FLEncoder_WriteString, FLError_kFLNoError,
    },
    fl_slice::{fl_slice_empty, AsFlSlice, FlSliceOwner},
    Database, Result,
};
use log::{error, info};
use std::{
    convert::TryFrom, mem, os::raw::c_void, panic::catch_unwind, process::abort, ptr, ptr::NonNull,
};

pub(crate) struct Replicator {
    inner: NonNull<C4Replicator>,
    c_callback_on_status_changed: C4ReplicatorStatusChangedCallback,
    free_callback_f: unsafe fn(_: *mut c_void),
    boxed_callback_f: NonNull<c_void>,
}

/// it should be safe to call replicator API from any thread
/// according to https://github.com/couchbase/couchbase-lite-core/wiki/Thread-Safety
unsafe impl Send for Replicator {}

impl Drop for Replicator {
    fn drop(&mut self) {
        unsafe {
            c4repl_free(self.inner.as_ptr());
            (self.free_callback_f)(self.boxed_callback_f.as_ptr());
        }
    }
}

impl Replicator {
    /// For example: url "ws://192.168.1.132:4984/demo/"
    pub(crate) fn new<F>(
        db: &Database,
        url: &str,
        token: Option<&str>,
        state_changed_callback: F,
    ) -> Result<Self>
    where
        F: FnMut(C4ReplicatorStatus) + Send + 'static,
    {
        unsafe extern "C" fn call_on_status_changed<F>(
            c4_repl: *mut C4Replicator,
            status: C4ReplicatorStatus,
            ctx: *mut c_void,
        ) where
            F: FnMut(C4ReplicatorStatus) + Send,
        {
            info!("on_status_changed: repl {:?}, status {:?}", c4_repl, status);
            let r = catch_unwind(|| {
                let boxed_f = ctx as *mut F;
                assert!(
                    !boxed_f.is_null(),
                    "DatabaseObserver: Internal error - null function pointer"
                );
                (*boxed_f)(status);
            });
            if r.is_err() {
                error!("Replicator::call_on_status_changed catch panic aborting");
                abort();
            }
        }

        let boxed_f: *mut F = Box::into_raw(Box::new(state_changed_callback));
        Replicator::do_new(
            db,
            url,
            token,
            free_boxed_value::<F>,
            unsafe { NonNull::new_unchecked(boxed_f as *mut c_void) },
            Some(call_on_status_changed::<F>),
        )
    }

    pub(crate) fn start(&mut self) {
        unsafe { c4repl_start(self.inner.as_ptr(), false) };
    }

    pub(crate) fn restart(self, db: &Database, url: &str, token: Option<&str>) -> Result<Self> {
        let Replicator {
            inner: prev_inner,
            free_callback_f,
            boxed_callback_f,
            c_callback_on_status_changed,
        } = self;
        mem::forget(self);
        unsafe {
            c4repl_stop(prev_inner.as_ptr());
            c4repl_free(prev_inner.as_ptr());
        }
        let mut repl = Replicator::do_new(
            db,
            url,
            token,
            free_callback_f,
            boxed_callback_f,
            c_callback_on_status_changed,
        )?;
        repl.start();
        Ok(repl)
    }

    fn do_new(
        db: &Database,
        url: &str,
        token: Option<&str>,
        free_callback_f: unsafe fn(_: *mut c_void),
        boxed_callback_f: NonNull<c_void>,
        call_on_status_changed: C4ReplicatorStatusChangedCallback,
    ) -> Result<Self> {
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

        let option_allow_conflicts = slice_without_nul!(kC4ReplicatorOptionOutgoingConflicts);
        let options: FlSliceOwner = if token.is_some() {
            unsafe {
                let enc = FLEncoder_New();

                FLEncoder_BeginDict(enc, 2);
                FLEncoder_WriteKey(
                    enc,
                    slice_without_nul!(kC4ReplicatorOptionCookies).as_flslice(),
                );
                FLEncoder_WriteString(enc, token_cookie.as_bytes().as_flslice());

                FLEncoder_WriteKey(enc, option_allow_conflicts.as_flslice());
                FLEncoder_WriteBool(enc, true);
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
            unsafe {
                let enc = FLEncoder_New();

                FLEncoder_BeginDict(enc, 1);
                FLEncoder_WriteKey(enc, option_allow_conflicts.as_flslice());
                FLEncoder_WriteBool(enc, true);
                FLEncoder_EndDict(enc);

                let mut fl_err = FLError_kFLNoError;
                let res = FLEncoder_Finish(enc, &mut fl_err);
                FLEncoder_Free(enc);
                if fl_err != FLError_kFLNoError {
                    return Err(Error::FlError(fl_err));
                }
                res.into()
            }
        };

        let repl_params = C4ReplicatorParameters {
            push: kC4Continuous as C4ReplicatorMode,
            pull: kC4Continuous as C4ReplicatorMode,
            optionsDictFleece: options.as_bytes().as_flslice(),
            pushFilter: None,
            validationFunc: None,
            onStatusChanged: call_on_status_changed,
            onDocumentsEnded: None,
            onBlobProgress: None,
            callbackContext: boxed_callback_f.as_ptr() as *mut c_void,
            socketFactory: ptr::null_mut(),
        };

        let mut c4err = c4error_init();
        let repl = unsafe {
            c4repl_new(
                db.inner.0.as_ptr(),
                remote_addr,
                db_name,
                repl_params,
                &mut c4err,
            )
        };
        NonNull::new(repl)
            .map(|inner| Replicator {
                inner,
                free_callback_f,
                boxed_callback_f,
                c_callback_on_status_changed: call_on_status_changed,
            })
            .ok_or_else(|| {
                unsafe { free_callback_f(boxed_callback_f.as_ptr()) };
                c4err.into()
            })
    }

    pub(crate) fn stop(self) {
        unsafe { c4repl_stop(self.inner.as_ptr()) };
    }

    pub(crate) fn status(&self) -> C4ReplicatorStatus {
        unsafe { c4repl_getStatus(self.inner.as_ptr()) }
    }
}

/// The possible states of a replicator
#[derive(Debug)]
pub enum ReplicatorState {
    /// Finished, or got a fatal error.
    Stopped(Error),
    /// Offline, replication doesn't not work
    Offline,
    /// Connection is in progress.
    Connecting,
    /// Continuous replicator has caught up and is waiting for changes.
    Idle,
    ///< Connected and actively working.
    Busy,
}

unsafe fn free_boxed_value<T>(p: *mut c_void) {
    drop(Box::from_raw(p as *mut T));
}

impl TryFrom<C4ReplicatorStatus> for ReplicatorState {
    type Error = Error;
    fn try_from(status: C4ReplicatorStatus) -> Result<Self> {
        #![allow(non_upper_case_globals)]
        macro_rules! define_activity_level {
            ($const_name:ident) => {
                const $const_name: C4ReplicatorActivityLevel =
                    crate::ffi::$const_name as C4ReplicatorActivityLevel;
            };
        }

        //TODO: use bindgen and https://github.com/rust-lang/rust/issues/44109
        //when it becomes stable
        define_activity_level!(kC4Stopped);
        define_activity_level!(kC4Offline);
        define_activity_level!(kC4Connecting);
        define_activity_level!(kC4Idle);
        define_activity_level!(kC4Busy);

        match status.level {
            kC4Stopped => Ok(ReplicatorState::Stopped(status.error.into())),
            kC4Offline => Ok(ReplicatorState::Offline),
            kC4Connecting => Ok(ReplicatorState::Connecting),
            kC4Idle => Ok(ReplicatorState::Idle),
            kC4Busy => Ok(ReplicatorState::Busy),
            _ => Err(Error::LogicError(format!("unknown level for {:?}", status))),
        }
    }
}
