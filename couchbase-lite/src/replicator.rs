#[cfg(feature = "use-tokio-websocket")]
mod tokio_socket;

use crate::{
    error::{c4error_init, Error, Result},
    ffi::{
        c4address_fromURL, c4repl_free, c4repl_getStatus, c4repl_new, c4repl_retry, c4repl_start,
        c4repl_stop, C4Address, C4DocumentEnded, C4Progress, C4Replicator,
        C4ReplicatorActivityLevel, C4ReplicatorDocumentsEndedCallback, C4ReplicatorMode,
        C4ReplicatorParameters, C4ReplicatorStatus, C4ReplicatorStatusChangedCallback,
        C4ReplicatorValidationFunction, C4RevisionFlags, C4String, FLDict, FLSliceResult,
    },
    Database,
};
use log::{error, info, trace};
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

/// Replicator of database
pub struct Replicator {
    inner: NonNull<C4Replicator>,
    validation: C4ReplicatorValidationFunction,
    c_callback_on_status_changed: C4ReplicatorStatusChangedCallback,
    c_callback_on_documents_ended: C4ReplicatorDocumentsEndedCallback,
    free_callback_f: unsafe fn(_: *mut c_void),
    boxed_callback_f: NonNull<c_void>,
}

struct CallbackContext<
    ValidationCb: ReplicatorValidationFunction,
    StateCb: ReplicatorStatusChangedCallback,
    DocumentsEndedCb: ReplicatorDocumentsEndedCallback,
> {
    validation_cb: ValidationCb,
    state_cb: StateCb,
    docs_ended_cb: DocumentsEndedCb,
}

#[derive(Clone)]
pub enum ReplicatorAuthentication {
    SessionToken(String),
    Basic { username: String, password: String },
    None,
}

/// it should be safe to call replicator API from any thread
/// according to <https://github.com/couchbase/couchbase-lite-core/wiki/Thread-Safety>
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

macro_rules! define_trait_alias {
    ($alias:ident, $($tt:tt)+) => {
        pub trait $alias: $($tt)+ {}
        impl<T> $alias for T where T: $($tt)+ {}
    };
}

define_trait_alias!(ReplicatorValidationFunction, FnMut(C4String, C4String, C4String, C4RevisionFlags, FLDict) -> bool + Send + 'static);
define_trait_alias!(
    ReplicatorStatusChangedCallback,
    FnMut(ReplicatorState) + Send + 'static
);
define_trait_alias!(
    ReplicatorDocumentsEndedCallback,
    FnMut(bool, &mut dyn Iterator<Item = &C4DocumentEnded>) + Send + 'static
);

impl Replicator {
    /// # Arguments
    /// * `url` - should be something like "ws://192.168.1.132:4984/demo/"
    /// * `validation_cb` - Callback that can reject incoming revisions.
    ///    Arguments: collection_name, doc_id, rev_id, rev_flags, doc_body.
    ///    It should return false to reject document.
    /// * `state_changed_callback` - reports back change of replicator state
    /// * `documents_ended_callback` - reports about the replication status of documents
    pub fn new<StateCallback, DocumentsEndedCallback, ValidationF>(
        db: &Database,
        url: &str,
        auth: &ReplicatorAuthentication,
        validation_cb: ValidationF,
        state_changed_callback: StateCallback,
        documents_ended_callback: DocumentsEndedCallback,
    ) -> Result<Self>
    where
        ValidationF: ReplicatorValidationFunction,
        StateCallback: ReplicatorStatusChangedCallback,
        DocumentsEndedCallback: ReplicatorDocumentsEndedCallback,
    {
        unsafe extern "C" fn call_validation<F, F2, F3>(
            coll_name: C4String,
            doc_id: C4String,
            rev_id: C4String,
            flags: C4RevisionFlags,
            body: FLDict,
            ctx: *mut c_void,
        ) -> bool
        where
            F: ReplicatorValidationFunction,
            F2: ReplicatorStatusChangedCallback,
            F3: ReplicatorDocumentsEndedCallback,
        {
            let r = catch_unwind(|| {
                let ctx = ctx as *mut CallbackContext<F, F2, F3>;
                assert!(
                    !ctx.is_null(),
                    "Replicator::call_validation: Internal error - null function pointer"
                );
                ((*ctx).validation_cb)(coll_name, doc_id, rev_id, flags, body)
            });
            r.unwrap_or_else(|_| {
                error!("Replicator::call_validation: catch panic aborting");
                abort();
            })
        }

        unsafe extern "C" fn call_on_status_changed<F1, F, F3>(
            c4_repl: *mut C4Replicator,
            status: C4ReplicatorStatus,
            ctx: *mut c_void,
        ) where
            F1: ReplicatorValidationFunction,
            F: ReplicatorStatusChangedCallback,
            F3: ReplicatorDocumentsEndedCallback,
        {
            info!("on_status_changed: repl {:?}, status {:?}", c4_repl, status);
            let r = catch_unwind(|| {
                let ctx = ctx as *mut CallbackContext<F1, F, F3>;
                assert!(
                    !ctx.is_null(),
                    "Replicator::call_on_status_changed: Internal error - null function pointer"
                );
                match ReplicatorState::try_from(status) {
                    Ok(state) => ((*ctx).state_cb)(state),
                    Err(err) => {
                        error!("replicator status change: invalid status {}", err);
                    }
                }
            });
            if r.is_err() {
                error!("Replicator::call_on_status_changed: catch panic aborting");
                abort();
            }
        }

        unsafe extern "C" fn call_on_documents_ended<F1, F2, F>(
            c4_repl: *mut C4Replicator,
            pushing: bool,
            num_docs: usize,
            docs: *mut *const C4DocumentEnded,
            ctx: *mut ::std::os::raw::c_void,
        ) where
            F1: ReplicatorValidationFunction,
            F2: ReplicatorStatusChangedCallback,
            F: ReplicatorDocumentsEndedCallback,
        {
            trace!(
                "on_documents_ended: repl {:?} pushing {}, num_docs {}",
                c4_repl,
                pushing,
                num_docs
            );
            let r = catch_unwind(|| {
                let ctx = ctx as *mut CallbackContext<F1, F2, F>;
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
            validation_cb,
            state_cb: state_changed_callback,
            docs_ended_cb: documents_ended_callback,
        });
        let ctx_p = Box::into_raw(ctx);
        Replicator::do_new(
            db,
            url,
            auth,
            free_boxed_value::<CallbackContext<ValidationF, StateCallback, DocumentsEndedCallback>>,
            unsafe { NonNull::new_unchecked(ctx_p as *mut c_void) },
            Some(call_validation::<ValidationF, StateCallback, DocumentsEndedCallback>),
            Some(call_on_status_changed::<ValidationF, StateCallback, DocumentsEndedCallback>),
            Some(call_on_documents_ended::<ValidationF, StateCallback, DocumentsEndedCallback>),
        )
    }

    /// starts database replication
    /// * `reset` - If true, the replicator will reset its checkpoint
    ///             and start replication from the beginning.
    pub fn start(&mut self, reset: bool) -> Result<()> {
        unsafe { c4repl_start(self.inner.as_ptr(), reset) };
        let status: ReplicatorState = self.status().into();
        if let ReplicatorState::Stopped(err) = status {
            Err(err)
        } else {
            Ok(())
        }
    }

    /// Full recreation of database replicator except callbacks,
    ///
    /// * `url`   - new url
    /// * `auth`  - new auth information
    /// * `reset` - If true, the replicator will reset its checkpoint
    ///             and start replication from the beginning.
    pub fn restart(
        self,
        db: &Database,
        url: &str,
        auth: &ReplicatorAuthentication,
        reset: bool,
    ) -> Result<Self> {
        let Replicator {
            inner: prev_inner,
            free_callback_f,
            boxed_callback_f,
            validation,
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
            auth,
            free_callback_f,
            boxed_callback_f,
            validation,
            c_callback_on_status_changed,
            c_callback_on_documents_ended,
        )?;
        repl.start(reset)?;
        Ok(repl)
    }

    /// Tells a replicator that's in the offline state to reconnect immediately.
    /// return `true` if the replicator will reconnect, `false` if it won't.
    pub fn retry(&mut self) -> Result<bool> {
        trace!("repl retry {:?}", self.inner.as_ptr());
        let mut c4err = c4error_init();
        let will_reconnect = unsafe { c4repl_retry(self.inner.as_ptr(), &mut c4err) };
        if c4err.code == 0 {
            Ok(will_reconnect)
        } else {
            Err(c4err.into())
        }
    }

    fn do_new(
        db: &Database,
        url: &str,
        auth: &ReplicatorAuthentication,
        free_callback_f: unsafe fn(_: *mut c_void),
        boxed_callback_f: NonNull<c_void>,
        validation: C4ReplicatorValidationFunction,
        call_on_status_changed: C4ReplicatorStatusChangedCallback,
        call_on_documents_ended: C4ReplicatorDocumentsEndedCallback,
    ) -> Result<Self> {
        use consts::*;

        let mut remote_addr = MaybeUninit::<C4Address>::uninit();
        let mut db_name = C4String::default();
        if !unsafe { c4address_fromURL(url.into(), remote_addr.as_mut_ptr(), &mut db_name) } {
            return Err(Error::LogicError(format!("Can not parse URL {}", url)));
        }
        let remote_addr = unsafe { remote_addr.assume_init() };

        let options_dict: FLSliceResult = match auth {
            ReplicatorAuthentication::SessionToken(token) => serde_fleece::fleece!({
                kC4ReplicatorOptionAuthentication: {
                    kC4ReplicatorAuthType: kC4AuthTypeSession,
                    kC4ReplicatorAuthToken: token.as_str(),
                }
            }),
            ReplicatorAuthentication::Basic { username, password } => {
                serde_fleece::fleece!({
                    kC4ReplicatorOptionAuthentication: {
                        kC4ReplicatorAuthType: kC4AuthTypeBasic,
                        kC4ReplicatorAuthUserName: username.as_str(),
                        kC4ReplicatorAuthPassword: password.as_str()
                    }
                })
            }
            ReplicatorAuthentication::None => serde_fleece::fleece!({}),
        }?;

        let repl_params = C4ReplicatorParameters {
            push: C4ReplicatorMode::kC4Continuous,
            pull: C4ReplicatorMode::kC4Continuous,
            optionsDictFleece: options_dict.as_fl_slice(),
            pushFilter: None,
            validationFunc: validation,
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
                validation,
                c_callback_on_status_changed: call_on_status_changed,
                c_callback_on_documents_ended: call_on_documents_ended,
            })
            .ok_or_else(|| {
                unsafe { free_callback_f(boxed_callback_f.as_ptr()) };
                c4err.into()
            })
    }
    #[inline]
    pub fn stop(&mut self) {
        trace!("repl stop {:?}", self.inner.as_ptr());
        unsafe { c4repl_stop(self.inner.as_ptr()) };
    }
    #[inline]
    pub fn state(&self) -> ReplicatorState {
        self.status().into()
    }
    #[inline]
    pub(crate) fn status(&self) -> C4ReplicatorStatus {
        unsafe { c4repl_getStatus(self.inner.as_ptr()) }
    }
}

/// Represents the current progress of a replicator.
/// The `units` fields should not be used directly, but divided (`unitsCompleted`/`unitsTotal`)
/// to give a _very_ approximate progress fraction.

pub type ReplicatorProgress = C4Progress;

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
    Busy(ReplicatorProgress),
}

unsafe fn free_boxed_value<T>(p: *mut c_void) {
    drop(Box::from_raw(p as *mut T));
}

impl From<C4ReplicatorStatus> for ReplicatorState {
    fn from(status: C4ReplicatorStatus) -> Self {
        match status.level {
            C4ReplicatorActivityLevel::kC4Stopped => ReplicatorState::Stopped(status.error.into()),
            C4ReplicatorActivityLevel::kC4Offline => ReplicatorState::Offline,
            C4ReplicatorActivityLevel::kC4Connecting => ReplicatorState::Connecting,
            C4ReplicatorActivityLevel::kC4Idle => ReplicatorState::Idle,
            C4ReplicatorActivityLevel::kC4Busy => ReplicatorState::Busy(status.progress),
            C4ReplicatorActivityLevel::kC4Stopping => ReplicatorState::Busy(status.progress),
        }
    }
}

#[allow(non_upper_case_globals)]
pub(crate) mod consts {
    /// Convert C constant strings to slices excluding last null char
    #[inline]
    const fn slice_without_null_char(cnst: &'static [u8]) -> &'static [u8] {
        match cnst.split_last() {
            Some((last, elements)) => {
                if *last != 0 {
                    panic!("C string constant has no 0 character at the end");
                }
                elements
            }
            None => panic!("C string constant empty, not expected"),
        }
    }

    /// Convert C constant strings to str excluding last null char
    #[inline]
    const fn str_without_null_char(cnst: &'static [u8]) -> &'static str {
        if !is_valid_ascii_str(cnst) {
            panic!("C string constant not valid ascii string");
        }
        unsafe { std::str::from_utf8_unchecked(cnst) }
    }

    const fn is_valid_ascii_str(cnst: &'static [u8]) -> bool {
        match cnst.split_first() {
            Some((first, rest)) => first.is_ascii() && is_valid_ascii_str(rest),
            None => true,
        }
    }

    macro_rules! define_const_str {
	($($name:ident,)+) => {
	    $(pub(crate) const $name: &'static str = str_without_null_char($crate::ffi::$name);)*
	};
    }

    define_const_str!(
        kC4AuthTypeBasic,
        kC4AuthTypeSession,
        kC4ReplicatorAuthPassword,
        kC4ReplicatorAuthToken,
        kC4ReplicatorAuthType,
        kC4ReplicatorAuthUserName,
        kC4ReplicatorOptionAuthentication,
    );

    macro_rules! define_const_slice {
	($($name:ident,)+) => {
	    $(pub(crate) const $name: &'static [u8] = slice_without_null_char($crate::ffi::$name);)*
	};
    }
    define_const_slice!(
        kC4ReplicatorOptionExtraHeaders,
        kC4ReplicatorOptionCookies,
        kC4SocketOptionWSProtocols,
    );
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
