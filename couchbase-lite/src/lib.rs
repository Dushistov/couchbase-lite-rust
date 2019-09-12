//! couchbase-lite is an ergonomic wrapper for using couchbase-lite-core from Rust.
//! ```rust
//! # #[macro_use]
//! # extern crate serde;
//! # use serde::{Serialize, Deserialize};
//! use couchbase_lite::{
//!     Database, DatabaseConfig, Document,
//!     fallible_streaming_iterator::FallibleStreamingIterator
//! };
//! use std::path::Path;
//!
//! #[derive(Serialize, Deserialize, Debug)]
//! #[serde(tag = "type")]
//! struct Message {
//!     msg: String,
//! }
//!
//! fn main() -> Result<(), couchbase_lite::Error> {
//!     let mut db = Database::open(Path::new("a.cblite2"), DatabaseConfig::default())?;
//!     {
//!         let msg = Message { msg: "Test message".into() };
//!         let mut trans = db.transaction()?;
//!         let mut doc = Document::new(&msg)?;
//!         trans.save(&mut doc)?;
//!         trans.commit()?;
//!     }
//!     println!("we have {} documents in db", db.document_count());
//!     let query = db.query(r#"{"WHAT": ["._id"], "WHERE": ["=", [".type"], "Message"]}"#)?;
//!     let mut iter = query.run()?;
//!     while let Some(item) = iter.next()? {
//!         let id = item.get_raw_checked(0)?;
//!         let id = id.as_str()?;
//!         let doc = db.get_existsing(id)?;
//!         println!("doc id {}", doc.id());
//!         let db_msg: Message = doc.decode_data()?;
//!         println!("db_msg: {:?}", db_msg);
//!         assert_eq!("Test message", db_msg.msg);
//!     }
//!     Ok(())
//! }
//! ```

mod doc_enumerator;
mod document;
mod error;
mod fl_slice;
mod log_reroute;
mod observer;
mod query;
mod replicator;
mod transaction;
mod value;

pub use crate::{
    doc_enumerator::{DocEnumerator, DocEnumeratorFlags},
    document::Document,
    error::Error,
    query::Query,
};
pub use couchbase_lite_core_sys as ffi;
pub use fallible_streaming_iterator;

use crate::{
    document::C4DocumentOwner,
    error::{c4error_init, Result},
    ffi::{
        c4db_free, c4db_getDocumentCount, c4db_open, c4doc_get, c4socket_registerFactory,
        kC4DB_Create, kC4EncryptionNone, kC4RevisionTrees, kC4SQLiteStorageEngine,
        C4CivetWebSocketFactory, C4Database, C4DatabaseConfig, C4DatabaseFlags,
        C4DocumentVersioning, C4EncryptionAlgorithm, C4EncryptionKey, C4String,
    },
    fl_slice::AsFlSlice,
    log_reroute::DB_LOGGER,
    observer::{DatabaseObserver, DbChange, DbChangesIter},
    replicator::Replicator,
    transaction::Transaction,
};
use log::error;
use once_cell::sync::Lazy;
use std::{
    collections::HashSet,
    path::Path,
    ptr::NonNull,
    sync::{Arc, Mutex},
};

/// Database configuration, used during open
pub struct DatabaseConfig {
    inner: C4DatabaseConfig,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            inner: C4DatabaseConfig {
                flags: kC4DB_Create as C4DatabaseFlags,
                storageEngine: unsafe { kC4SQLiteStorageEngine },
                versioning: kC4RevisionTrees as C4DocumentVersioning,
                encryptionKey: C4EncryptionKey {
                    algorithm: kC4EncryptionNone as C4EncryptionAlgorithm,
                    bytes: [0; 32],
                },
            },
        }
    }
}

/// use embedded web-socket library
pub fn use_c4_civet_web_socket_factory() {
    unsafe { c4socket_registerFactory(C4CivetWebSocketFactory) };
}

/// A connection to a couchbase-lite database.
pub struct Database {
    inner: DbInner,
    db_events: Arc<Mutex<HashSet<usize>>>,
    db_observers: Vec<DatabaseObserver>,
    db_replicator: Option<Replicator>,
}

pub(crate) struct DbInner(NonNull<C4Database>);
/// According to
/// https://github.com/couchbase/couchbase-lite-core/wiki/Thread-Safety
/// it is possible to call from any thread, but not concurrently
unsafe impl Send for DbInner {}

impl Drop for Database {
    fn drop(&mut self) {
        if let Some(repl) = self.db_replicator.take() {
            repl.stop();
        }
        self.db_observers.clear();
        unsafe { c4db_free(self.inner.0.as_ptr()) };
    }
}

impl Database {
    pub fn open(path: &Path, cfg: DatabaseConfig) -> Result<Database> {
        Lazy::force(&DB_LOGGER);
        let mut error = c4error_init();
        let os_path_utf8 = path.to_str().ok_or(Error::Utf8)?;
        let os_path_utf8: C4String = os_path_utf8.as_flslice();
        let db_ptr = unsafe { c4db_open(os_path_utf8, &cfg.inner, &mut error) };
        NonNull::new(db_ptr)
            .map(|inner| Database {
                inner: DbInner(inner),
                db_events: Arc::new(Mutex::new(HashSet::new())),
                db_observers: vec![],
                db_replicator: None,
            })
            .ok_or_else(|| error.into())
    }

    pub(crate) fn internal_get(&self, doc_id: &str, must_exists: bool) -> Result<C4DocumentOwner> {
        let mut c4err = c4error_init();
        let doc_ptr = unsafe {
            c4doc_get(
                self.inner.0.as_ptr(),
                doc_id.as_bytes().as_flslice(),
                must_exists,
                &mut c4err,
            )
        };
        NonNull::new(doc_ptr)
            .map(C4DocumentOwner)
            .ok_or_else(|| c4err.into())
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
    pub fn get_existsing(&self, doc_id: &str) -> Result<Document> {
        self.internal_get(doc_id, true)
            .map(|x| Document::new_internal(x, doc_id))
    }

    /// Compiles a query from an expression given as JSON.
    /// The expression is a predicate that describes which documents should be returned.
    /// A separate, optional sort expression describes the ordering of the results.
    pub fn query(&self, query_json: &str) -> Result<Query> {
        Query::new(self, query_json)
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

    /// starts database replication
    pub fn start_replicator(&mut self, url: &str, token: Option<&str>) -> Result<()> {
        self.db_replicator = Some(Replicator::new(self, url, token)?);
        Ok(())
    }
}

pub struct ObserverdChangesIter<'db> {
    db: &'db Database,
    obs_it: Option<DbChangesIter<'db>>,
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
                let obs_ptr = match db_events.iter().next() {
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
