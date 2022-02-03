mod database;
mod document;
mod error;
mod log_reroute;
mod transaction;

pub use crate::{
    database::{Database, DatabaseConfig},
    document::Document,
};
pub use couchbase_lite_core_sys as ffi;
pub use ffi::{kC4DB_Create, kC4DB_NoUpgrade, kC4DB_NonObservable, kC4DB_ReadOnly};
