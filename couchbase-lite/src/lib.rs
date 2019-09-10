//! couchbase-lite is an ergonomic wrapper for using couchbase-lite-core from Rust.
//! ```rust
//! # #[macro_use]
//! # extern crate serde;
//! # use serde::{Serialize, Deserialize};
//! use couchbase_lite::{Database, DatabaseConfig, Document};
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
//!     Ok(())
//! }
//! ```

mod document;
mod error;
mod fl_slice;
mod transaction;

pub use crate::{document::Document, error::Error};
pub use couchbase_lite_core_sys as ffi;

use crate::{
    document::C4DocumentOwner,
    error::{c4error_init, Result},
    ffi::{
        c4db_free, c4db_open, c4doc_get, kC4DB_Create, kC4EncryptionNone, kC4RevisionTrees,
        kC4SQLiteStorageEngine, C4Database, C4DatabaseConfig, C4DatabaseFlags,
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
}
