mod de;
mod error;
mod ser;

pub use couchbase_lite_core_sys as ffi;
pub use de::from_slice;
pub use error::Error;
pub use ser::to_fl_slice_result;
