use crate::{
    error::{c4error_init, Error, Result},
    ffi::{
        c4db_getCollection, c4dbobs_createOnCollection, c4dbobs_free, c4dbobs_getChanges,
        c4dbobs_releaseChanges, kC4DefaultCollectionSpec, C4CollectionChange, C4CollectionObserver,
        C4RevisionFlags,
    },
    Database,
};
use log::error;
use std::{mem::MaybeUninit, os::raw::c_void, panic::catch_unwind, process::abort, ptr::NonNull};

pub(crate) struct DatabaseObserver {
    inner: NonNull<C4CollectionObserver>,
    free_callback_f: unsafe extern "C" fn(_: *mut c_void),
    boxed_callback_f: NonNull<c_void>,
}

impl Drop for DatabaseObserver {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            c4dbobs_free(self.inner.as_ptr());
            (self.free_callback_f)(self.boxed_callback_f.as_ptr());
        }
    }
}

impl DatabaseObserver {
    pub(crate) fn new<F>(db: &Database, callback_f: F) -> Result<DatabaseObserver>
    where
        F: FnMut(*const C4CollectionObserver) + Send + 'static,
    {
        unsafe extern "C" fn call_boxed_closure<F>(
            obs: *mut C4CollectionObserver,
            context: *mut c_void,
        ) where
            F: FnMut(*const C4CollectionObserver) + Send,
        {
            let r = catch_unwind(|| {
                let boxed_f = context as *mut F;
                assert!(
                    !boxed_f.is_null(),
                    "DatabaseObserver: Internal error - null function pointer"
                );
                (*boxed_f)(obs);
            });
            if r.is_err() {
                error!("DatabaseObserver::call_boxed_closure catch panic aborting");
                abort();
            }
        }
        let boxed_f: *mut F = Box::into_raw(Box::new(callback_f));
        let mut error = c4error_init();
        let collection = unsafe {
            c4db_getCollection(db.inner.0.as_ptr(), kC4DefaultCollectionSpec, &mut error)
        };
        if collection.is_null() {
            return Err(error.into());
        }
        let mut error = c4error_init();
        let obs = unsafe {
            c4dbobs_createOnCollection(
                collection,
                Some(call_boxed_closure::<F>),
                boxed_f as *mut c_void,
                &mut error,
            )
        };
        NonNull::new(obs)
            .map(|inner| DatabaseObserver {
                inner,
                free_callback_f: free_boxed_value::<F>,
                boxed_callback_f: unsafe { NonNull::new_unchecked(boxed_f as *mut c_void) },
            })
            .ok_or_else(|| {
                unsafe { free_boxed_value::<F>(boxed_f as *mut c_void) };
                error.into()
            })
    }

    pub(crate) fn match_obs_ptr(&self, obs_ptr: usize) -> bool {
        self.inner.as_ptr() as usize == obs_ptr
    }
    pub(crate) fn changes_iter<'a>(&'a self) -> DbChangesIter<'a> {
        DbChangesIter { obs: self }
    }
}

unsafe extern "C" fn free_boxed_value<T>(p: *mut c_void) {
    drop(Box::from_raw(p as *mut T));
}

pub(crate) struct DbChangesIter<'obs> {
    obs: &'obs DatabaseObserver,
}

#[derive(Debug)]
pub struct DbChange {
    inner: C4CollectionChange,
    external: bool,
}

impl DbChange {
    #[inline]
    pub fn external(&self) -> bool {
        self.external
    }
    #[inline]
    pub fn doc_id(&self) -> Result<&str> {
        self.inner
            .docID
            .as_fl_slice()
            .try_into()
            .map_err(|_| Error::InvalidUtf8)
    }
    #[inline]
    pub fn revision_id(&self) -> Result<&str> {
        self.inner
            .revID
            .as_fl_slice()
            .try_into()
            .map_err(|_| Error::InvalidUtf8)
    }
    #[inline]
    pub fn body_size(&self) -> u32 {
        self.inner.bodySize
    }
    #[inline]
    pub fn revision_flags(&self) -> C4RevisionFlags {
        self.inner.flags
    }
}

impl Drop for DbChange {
    #[inline]
    fn drop(&mut self) {
        unsafe { c4dbobs_releaseChanges(&mut self.inner, 1) };
    }
}

impl<'obs> Iterator for DbChangesIter<'obs> {
    type Item = DbChange;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let mut item = MaybeUninit::<C4CollectionChange>::uninit();
        let observation =
            unsafe { c4dbobs_getChanges(self.obs.inner.as_ptr(), item.as_mut_ptr(), 1) };
        if observation.numChanges > 0 {
            let item = unsafe { item.assume_init() };
            Some(DbChange {
                inner: item,
                external: observation.external,
            })
        } else {
            None
        }
    }
}

pub struct ObserverdChangesIter<'db> {
    pub(crate) db: &'db Database,
    pub(crate) obs_it: Option<DbChangesIter<'db>>,
}

impl<'db> Iterator for ObserverdChangesIter<'db> {
    type Item = DbChange;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(obs_it) = self.obs_it.as_mut() {
                if let Some(item) = obs_it.next() {
                    return Some(item);
                }
                self.obs_it = None;
            }
            let obs_ptr = {
                let mut db_events = self.db.db_events.lock().expect("db_events lock failed");
                if db_events.is_empty() {
                    return None;
                }
                let next_event = db_events.iter().next();
                let obs_ptr = match next_event {
                    Some(obs_ptr) => *obs_ptr,
                    None => return None,
                };
                db_events.remove(&obs_ptr);
                obs_ptr
            };
            let obs: Option<&'db DatabaseObserver> = self
                .db
                .db_observers
                .iter()
                .find(|obs| obs.match_obs_ptr(obs_ptr));
            if let Some(obs) = obs {
                self.obs_it = Some(obs.changes_iter());
            }
        }
    }
}
