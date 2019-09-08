//! couchbase-lite is an ergonomic wrapper for using couchbase-lite-core from Rust.
//! ```rust
//! use couchbase_lite::{Database, DatabaseConfig};
//! use std::path::Path;
//!
//! fn main() -> Result<(), couchbase_lite::Error> {
//!     let db = Database::open(Path::new("a.cblite2"), DatabaseConfig::default())?;
//!     Ok(())
//! }
//! ```

mod error;
mod fl_slice;

pub use crate::error::Error;
pub use couchbase_lite_core_sys as ffi;

use crate::{
    error::{c4error_init, Result},
    ffi::{
        c4db_free, c4db_open, kC4DB_Create, kC4EncryptionNone, kC4RevisionTrees,
        kC4SQLiteStorageEngine, C4Database, C4DatabaseConfig, C4DatabaseFlags,
        C4DocumentVersioning, C4EncryptionAlgorithm, C4EncryptionKey, C4String,
    },
    fl_slice::AsFlSlice,
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
}
