use crate::{
    doc_enumerator::{DocEnumerator, DocEnumeratorFlags},
    document::{C4DocumentOwner, Document},
    error::{c4error_init, Error, Result},
    ffi::{
        c4db_getDocumentCount, c4db_release, c4doc_get, C4Database, C4DatabaseConfig2,
        C4DatabaseFlags, C4EncryptionAlgorithm, C4EncryptionKey,
    },
    log_reroute::c4log_to_log_init,
    observer::{DatabaseObserver, ObserverdChangesIter},
    transaction::Transaction,
};
use couchbase_lite_core_sys::{c4db_getSharedFleeceEncoder, c4db_openNamed};
use lazy_static::lazy_static;
use log::{debug, error};
use serde_fleece::FlEncoderSession;
use std::{
    collections::HashSet,
    marker::PhantomData,
    path::Path,
    ptr::NonNull,
    sync::{Arc, Mutex},
};

/// Database configuration, used during open
pub struct DatabaseConfig<'a> {
    inner: Result<C4DatabaseConfig2>,
    phantom: PhantomData<&'a Path>,
}

impl<'a> DatabaseConfig<'a> {
    pub fn new(parent_directory: &'a Path, flags: C4DatabaseFlags) -> Self {
        let os_path_utf8 = match parent_directory.to_str() {
            Some(x) => x,
            None => {
                return Self {
                    inner: Err(Error::InvalidUtf8),
                    phantom: PhantomData,
                }
            }
        };
        Self {
            inner: Ok(C4DatabaseConfig2 {
                parentDirectory: os_path_utf8.into(),
                flags,
                encryptionKey: C4EncryptionKey {
                    algorithm: C4EncryptionAlgorithm::kC4EncryptionNone,
                    bytes: [0; 32],
                },
            }),
            phantom: PhantomData,
        }
    }
}

/// A connection to a couchbase-lite database.
pub struct Database {
    pub(crate) inner: DbInner,
    pub(crate) db_events: Arc<Mutex<HashSet<usize>>>,
    pub(crate) db_observers: Vec<DatabaseObserver>,
}

pub(crate) struct DbInner(pub NonNull<C4Database>);
/// According to
/// https://github.com/couchbase/couchbase-lite-core/wiki/Thread-Safety
/// it is possible to call from any thread, but not concurrently
unsafe impl Send for DbInner {}

impl Drop for DbInner {
    fn drop(&mut self) {
        unsafe { c4db_release(self.0.as_ptr()) };
    }
}

impl Database {
    pub fn open_named(name: &str, cfg: DatabaseConfig) -> Result<Self> {
        lazy_static::initialize(&DB_LOG_HANDLER);
        let cfg = cfg.inner?;
        let mut error = c4error_init();
        let db_ptr = unsafe { c4db_openNamed(name.into(), &cfg, &mut error) };
        NonNull::new(db_ptr)
            .map(|inner| Database {
                inner: DbInner(inner),
                db_events: Arc::new(Mutex::new(HashSet::new())),
                db_observers: vec![],
            })
            .ok_or_else(|| error.into())
    }
    pub fn open_with_flags(path: &Path, flags: C4DatabaseFlags) -> Result<Self> {
        let parent_path = path
            .parent()
            .ok_or_else(|| Error::LogicError(format!("path {:?} has no parent diretory", path)))?;
        let cfg = DatabaseConfig::new(parent_path, flags);
        let db_name = path
            .file_name()
            .ok_or_else(|| Error::LogicError(format!("path {:?} has no last part", path)))?
            .to_str()
            .ok_or_else(|| Error::InvalidUtf8)?
            .strip_suffix(".cblite2")
            .ok_or_else(|| {
                Error::LogicError(format!(
                    "path {:?} should have last part with .cblite2 suffix",
                    path
                ))
            })?;

        Database::open_named(db_name, cfg)
    }
    /// Begin a new transaction, the transaction defaults to rolling back
    /// when it is dropped. If you want the transaction to commit,
    /// you must call `Transaction::commit`
    pub fn transaction(&mut self) -> Result<Transaction> {
        Transaction::new(self)
    }
    /// Returns the number of (undeleted) documents in the database
    pub fn document_count(&self) -> u64 {
        unsafe { c4db_getDocumentCount(self.inner.0.as_ptr()) }
    }
    /// Return existing document from database
    pub fn get_existing(&self, doc_id: &str) -> Result<Document> {
        self.internal_get(doc_id, true)
            .map(|x| Document::new_internal(x, doc_id))
    }
    /// Creates an enumerator ordered by docID.
    pub fn enumerate_all_docs(&self, flags: DocEnumeratorFlags) -> Result<DocEnumerator> {
        DocEnumerator::enumerate_all_docs(self, flags)
    }

    /// Register a database observer, with a callback that will be invoked after the database
    /// changes. The callback will be called _once_, after the first change. After that it won't
    /// be called again until all of the changes have been read by calling `Database::observed_changes`.
    pub fn register_observer<F>(&mut self, mut callback_f: F) -> Result<()>
    where
        F: FnMut() + Send + 'static,
    {
        let db_events = self.db_events.clone();
        let obs = DatabaseObserver::new(self, move |obs| {
            {
                match db_events.lock() {
                    Ok(mut db_events) => {
                        db_events.insert(obs as usize);
                    }
                    Err(err) => {
                        error!(
                            "register_observer::DatabaseObserver::lambda db_events lock failed: {}",
                            err
                        );
                    }
                }
            }
            callback_f();
        })?;
        self.db_observers.push(obs);
        Ok(())
    }

    /// Remove all database observers
    pub fn clear_observers(&mut self) {
        self.db_observers.clear();
    }

    /// Get observed changes for this database
    pub fn observed_changes(&mut self) -> ObserverdChangesIter {
        ObserverdChangesIter {
            db: self,
            obs_it: None,
        }
    }

    /// Get shared "fleece" encoder, `&mut self` to make possible
    /// exists only one session
    pub fn shared_encoder_session(&mut self) -> Result<FlEncoderSession> {
        let enc = unsafe { c4db_getSharedFleeceEncoder(self.inner.0.as_ptr()) };
        NonNull::new(enc)
            .ok_or_else(|| {
                Error::LogicError("c4db_getSharedFleeceEncoder return null.into()".into())
            })
            .map(FlEncoderSession::new)
    }

    pub(crate) fn internal_get(&self, doc_id: &str, must_exists: bool) -> Result<C4DocumentOwner> {
        let mut c4err = c4error_init();
        let c4doc = unsafe {
            c4doc_get(
                self.inner.0.as_ptr(),
                doc_id.as_bytes().into(),
                must_exists,
                &mut c4err,
            )
        };
        NonNull::new(c4doc)
            .ok_or_else(|| c4err.into())
            .map(C4DocumentOwner)
    }
}

lazy_static! {
    static ref DB_LOG_HANDLER: () = {
        debug!("init couchbase log to rust log rerouting");
        c4log_to_log_init();
        ()
    };
}
