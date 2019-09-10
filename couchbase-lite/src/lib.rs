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
mod query;
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
        c4db_free, c4db_getDocumentCount, c4db_open, c4doc_get, kC4DB_Create, kC4EncryptionNone,
        kC4RevisionTrees, kC4SQLiteStorageEngine, C4Database, C4DatabaseConfig, C4DatabaseFlags,
        C4DocumentVersioning, C4EncryptionAlgorithm, C4EncryptionKey, C4String,
    },
    fl_slice::AsFlSlice,
    transaction::Transaction,
};
use std::{path::Path, ptr::NonNull};

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

/// A connection to a couchbase-lite database.
pub struct Database {
    inner: NonNull<C4Database>,
}

impl Drop for Database {
    fn drop(&mut self) {
        unsafe { c4db_free(self.inner.as_ptr()) };
    }
}

impl Database {
    pub fn open(path: &Path, cfg: DatabaseConfig) -> Result<Database> {
        let mut error = c4error_init();
        let os_path_utf8 = path.to_str().ok_or(Error::Utf8)?;
        let os_path_utf8: C4String = os_path_utf8.as_flslice();
        let db_ptr = unsafe { c4db_open(os_path_utf8, &cfg.inner, &mut error) };
        NonNull::new(db_ptr)
            .map(|inner| Database { inner })
            .ok_or_else(|| error.into())
    }

    pub(crate) fn internal_get(&self, doc_id: &str, must_exists: bool) -> Result<C4DocumentOwner> {
        let mut c4err = c4error_init();
        let doc_ptr = unsafe {
            c4doc_get(
                self.inner.as_ptr(),
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
        unsafe { c4db_getDocumentCount(self.inner.as_ptr()) }
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
}
