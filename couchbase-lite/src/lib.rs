//! couchbase-lite is an ergonomic wrapper for using couchbase-lite-core from Rust.
//! ```rust
//! # #[macro_use]
//! # extern crate serde;
//! # use serde::{Serialize, Deserialize};
//! use couchbase_lite::{
//!     Database, Document, DatabaseFlags,
//!     fallible_streaming_iterator::FallibleStreamingIterator
//! };
//!
//! #[derive(Serialize, Deserialize, Debug)]
//! #[serde(tag = "type")]
//! struct Message {
//!     msg: String,
//! }
//!
//! fn main() -> Result<(), couchbase_lite::Error> {
//!     let mut db = Database::open_with_flags(
//!         &std::env::temp_dir().join("a.cblite2"),
//!         DatabaseFlags::CREATE,
//!     )?;
//!     {
//!         let msg = Message { msg: "Test message".into() };
//!         let mut trans = db.transaction()?;
//!         let enc = trans.shared_encoder_session()?;
//!         let mut doc = Document::new(&msg, enc)?;
//!         trans.save(&mut doc)?;
//!         trans.commit()?;
//!     }
//!     println!("we have {} documents in db", db.document_count());
//!     let query = db.n1ql_query("SELECT _id FROM _default WHERE type='Message'")?;
//!     let mut iter = query.run()?;
//!     while let Some(item) = iter.next()? {
//!         let id = item.get_raw_checked(0)?;
//!         let id = id.as_str()?;
//!         let doc = db.get_existing(id)?;
//!         println!("doc id {}", doc.id());
//!         let db_msg: Message = doc.decode_body()?;
//!         println!("db_msg: {:?}", db_msg);
//!         assert_eq!("Test message", db_msg.msg);
//!     }
//!     Ok(())
//! }
//! ```

mod conflict_resolver;
mod database;
mod doc_enumerator;
mod document;
mod error;
mod index;
mod log_reroute;
mod observer;
mod query;
mod replicator;
mod transaction;
mod value;

pub use crate::{
    conflict_resolver::resolve_conflict,
    database::{Database, DatabaseConfig, DatabaseFlags},
    doc_enumerator::DocEnumeratorFlags,
    document::{Document, DocumentFlags},
    error::Error,
    fallible_streaming_iterator::FallibleStreamingIterator,
    index::IndexType,
    replicator::{ReplicatorAuthentication, ReplicatorState},
    value::{ValueRef, ValueRefArray},
};
pub use couchbase_lite_core_sys as ffi;
pub use fallible_streaming_iterator;
pub use ffi::C4QueryLanguage as QueryLanguage;

pub use serde_fleece;
