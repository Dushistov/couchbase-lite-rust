mod database;
mod error;
mod log_reroute;

pub use crate::database::{Database, DatabaseConfig};
pub use couchbase_lite_core_sys as ffi;
pub use ffi::{kC4DB_Create, kC4DB_NoUpgrade, kC4DB_NonObservable, kC4DB_ReadOnly};
