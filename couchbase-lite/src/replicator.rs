#[cfg(feature = "use-tokio-websocket")]
mod tokio_socket;

use crate::{
    error::{c4error_init, Error, Result},
    ffi::{
        c4address_fromURL, c4repl_free, c4repl_getStatus, c4repl_new, c4repl_start, c4repl_stop,
        kC4ReplicatorOptionCookies, C4Address, C4DocumentEnded, C4Replicator,
        C4ReplicatorActivityLevel, C4ReplicatorDocumentsEndedCallback, C4ReplicatorMode,
        C4ReplicatorParameters, C4ReplicatorStatus, C4ReplicatorStatusChangedCallback, C4String,
        FLSliceResult,
    },
    Database,
};
use log::{debug, error, info, trace};
use std::{
    convert::TryFrom,
    mem::{self, MaybeUninit},
    os::raw::c_void,
    panic::catch_unwind,
    process::abort,
    ptr,
    ptr::NonNull,
    slice, str,
    sync::Once,
};

pub(crate) struct Replicator {
    inner: NonNull<C4Replicator>,
    c_callback_on_status_changed: C4ReplicatorStatusChangedCallback,
    c_callback_on_documents_ended: C4ReplicatorDocumentsEndedCallback,
    free_callback_f: unsafe fn(_: *mut c_void),
    boxed_callback_f: NonNull<c_void>,
}

struct CallbackContext<
    StateCallback: FnMut(C4ReplicatorStatus) + Send + 'static,
    DocumentsEndedCallback: FnMut(bool, &mut dyn Iterator<Item = &C4DocumentEnded>) + Send + 'static,
> {
    state_cb: StateCallback,
    docs_ended_cb: DocumentsEndedCallback,
}

/// it should be safe to call replicator API from any thread
/// according to https://github.com/couchbase/couchbase-lite-core/wiki/Thread-Safety
unsafe impl Send for Replicator {}

impl Drop for Replicator {
    fn drop(&mut self) {
        trace!("repl drop {:?}", self.inner.as_ptr());
        unsafe {
            c4repl_free(self.inner.as_ptr());
            (self.free_callback_f)(self.boxed_callback_f.as_ptr());
        }
    }
}

impl Replicator {
    /// # Arguments
    /// * `url` - should be something like "ws://192.168.1.132:4984/demo/"
    /// * `state_changed_callback` - reports back change of replicator state
    /// * `documents_ended_callback` - reports about the replication status of documents
    pub(crate) fn new<StateCallback, DocumentsEndedCallback>(
        db: &Database,
        url: &str,
        token: Option<&str>,
        state_changed_callback: StateCallback,
        documents_ended_callback: DocumentsEndedCallback,
    ) -> Result<Self>
    where
        StateCallback: FnMut(C4ReplicatorStatus) + Send + 'static,
        DocumentsEndedCallback:
            FnMut(bool, &mut dyn Iterator<Item = &C4DocumentEnded>) + Send + 'static,
    {
        unsafe extern "C" fn call_on_status_changed<F, F2>(
            c4_repl: *mut C4Replicator,
            status: C4ReplicatorStatus,
            ctx: *mut c_void,
        ) where
            F: FnMut(C4ReplicatorStatus) + Send + 'static,
            F2: FnMut(bool, &mut dyn Iterator<Item = &C4DocumentEnded>) + Send + 'static,
        {
            info!("on_status_changed: repl {:?}, status {:?}", c4_repl, status);
            let r = catch_unwind(|| {
                let ctx = ctx as *mut CallbackContext<F, F2>;
                assert!(
                    !ctx.is_null(),
                    "Replicator::call_on_status_changed: Internal error - null function pointer"
                );
                ((*ctx).state_cb)(status);
            });
            if r.is_err() {
                error!("Replicator::call_on_status_changed: catch panic aborting");
                abort();
            }
        }

        unsafe extern "C" fn call_on_documents_ended<F1, F>(
            c4_repl: *mut C4Replicator,
            pushing: bool,
            num_docs: usize,
            docs: *mut *const C4DocumentEnded,
            ctx: *mut ::std::os::raw::c_void,
        ) where
            F1: FnMut(C4ReplicatorStatus) + Send + 'static,
            F: FnMut(bool, &mut dyn Iterator<Item = &C4DocumentEnded>) + Send + 'static,
        {
            debug!(
                "on_documents_ended: repl {:?} pushing {}, num_docs {}",
                c4_repl, pushing, num_docs
            );
            let r = catch_unwind(|| {
                let ctx = ctx as *mut CallbackContext<F1, F>;
                assert!(
                    !ctx.is_null(),
                    "Replicator::call_on_documents_ended: Internal error - null function pointer"
                );
                let docs: &[*const C4DocumentEnded] = slice::from_raw_parts(docs, num_docs);
                let mut it = docs.iter().map(|x| &**x);
                ((*ctx).docs_ended_cb)(pushing, &mut it);
            });
            if r.is_err() {
                error!("Replicator::call_on_documents_ended: catch panic aborting");
                abort();
            }
        }

        let ctx = Box::new(CallbackContext {
            state_cb: state_changed_callback,
            docs_ended_cb: documents_ended_callback,
        });
        let ctx_p = Box::into_raw(ctx);
        Replicator::do_new(
            db,
            url,
            token,
            free_boxed_value::<CallbackContext<StateCallback, DocumentsEndedCallback>>,
            unsafe { NonNull::new_unchecked(ctx_p as *mut c_void) },
            Some(call_on_status_changed::<StateCallback, DocumentsEndedCallback>),
            Some(call_on_documents_ended::<StateCallback, DocumentsEndedCallback>),
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
            c_callback_on_documents_ended,
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
            c_callback_on_documents_ended,
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
        call_on_documents_ended: C4ReplicatorDocumentsEndedCallback,
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
            onDocumentsEnded: call_on_documents_ended,
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
        trace!("repl new result {:?}", repl);
        NonNull::new(repl)
            .map(|inner| Replicator {
                inner,
                free_callback_f,
                boxed_callback_f,
                c_callback_on_status_changed: call_on_status_changed,
                c_callback_on_documents_ended: call_on_documents_ended,
            })
            .ok_or_else(|| {
                unsafe { free_callback_f(boxed_callback_f.as_ptr()) };
                c4err.into()
            })
    }

    pub(crate) fn stop(self) {
        trace!("repl stop {:?}", self.inner.as_ptr());
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

static WEBSOCKET_IMPL: Once = Once::new();

#[cfg(feature = "use-couchbase-lite-websocket")]
pub(crate) fn init_builtin_socket_impl() {
    WEBSOCKET_IMPL.call_once(|| {
        unsafe { crate::ffi::C4RegisterBuiltInWebSocket() };
    });
}

#[cfg(feature = "use-tokio-websocket")]
pub(crate) fn init_tokio_socket_impl(handle: tokio::runtime::Handle) {
    WEBSOCKET_IMPL.call_once(|| {
        tokio_socket::c4socket_init(handle);
    });
}
