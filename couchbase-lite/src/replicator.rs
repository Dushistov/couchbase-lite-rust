use crate::{
    error::{c4error_init, Error, Result},
    ffi::{
        c4address_fromURL, c4repl_free, c4repl_getStatus, c4repl_new, c4repl_start, c4repl_stop,
        kC4ReplicatorOptionCookies, C4Address, C4Replicator, C4ReplicatorActivityLevel,
        C4ReplicatorMode, C4ReplicatorParameters, C4ReplicatorStatus,
        C4ReplicatorStatusChangedCallback, C4String, FLSliceResult,
    },
    Database,
};
use lazy_static::lazy_static;
use log::{debug, error, info};
use std::{
    convert::TryFrom,
    mem::{self, MaybeUninit},
    os::raw::c_void,
    panic::catch_unwind,
    process::abort,
    ptr,
    ptr::NonNull,
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
        lazy_static::initialize(&WEBSOCKET_IMPL);
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

    pub(crate) fn start(&mut self) -> Result<()> {
        unsafe { c4repl_start(self.inner.as_ptr(), false) };
        let status: ReplicatorState = self.status().try_into()?;
        if let ReplicatorState::Stopped(err) = status {
            Err(err)
        } else {
            Ok(())
        }
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
        repl.start()?;
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
        let mut remote_addr = MaybeUninit::<C4Address>::uninit();
        let mut db_name = C4String::default();
        if !unsafe { c4address_fromURL(url.into(), remote_addr.as_mut_ptr(), &mut db_name) } {
            return Err(Error::LogicError(format!("Can not parse URL {}", url)));
        }
        let remote_addr = unsafe { remote_addr.assume_init() };
        let options_dict: FLSliceResult = if let Some(token) = token {
            let opt_cookie = slice_without_null_char(kC4ReplicatorOptionCookies);
            let token_cookie = format!("{}={}", "SyncGatewaySession", token);
            let token_cookie = token_cookie.as_str();
            serde_fleece::fleece!({ opt_cookie: token_cookie })
        } else {
            serde_fleece::fleece!({})
        }?;

        let repl_params = C4ReplicatorParameters {
            push: C4ReplicatorMode::kC4Continuous,
            pull: C4ReplicatorMode::kC4Continuous,
            optionsDictFleece: options_dict.as_fl_slice(),
            pushFilter: None,
            validationFunc: None,
            onStatusChanged: call_on_status_changed,
            onDocumentsEnded: None,
            onBlobProgress: None,
            propertyEncryptor: ptr::null_mut(),
            propertyDecryptor: ptr::null_mut(),
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
        match status.level {
            C4ReplicatorActivityLevel::kC4Stopped => {
                Ok(ReplicatorState::Stopped(status.error.into()))
            }
            C4ReplicatorActivityLevel::kC4Offline => Ok(ReplicatorState::Offline),
            C4ReplicatorActivityLevel::kC4Connecting => Ok(ReplicatorState::Connecting),
            C4ReplicatorActivityLevel::kC4Idle => Ok(ReplicatorState::Idle),
            C4ReplicatorActivityLevel::kC4Busy => Ok(ReplicatorState::Busy),
            _ => Err(Error::LogicError(format!("unknown level for {:?}", status))),
        }
    }
}

/// Convert C contant strings to slices excluding last null char
#[inline]
fn slice_without_null_char(cnst: &[u8]) -> &[u8] {
    &cnst[0..(cnst.len() - 1)]
}

lazy_static! {
    static ref WEBSOCKET_IMPL: Result<()> = {
        debug!("init websocket implementation");
        c4socket_init()
    };
}

#[cfg(feature = "use-couchbase-lite-websocket")]
fn c4socket_init() -> Result<()> {
    unsafe { crate::ffi::C4RegisterBuiltInWebSocket() };
    Ok(())
}

#[cfg(feature = "use-tokio-websocket")]
fn c4socket_init() -> Result<()> {
    unimplemented!()
}
