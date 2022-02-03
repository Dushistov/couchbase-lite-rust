mod de;
mod error;
mod ser;

pub use couchbase_lite_core_sys as ffi;
pub use de::{from_fl_dict, from_slice, NonNullConst};
pub use error::Error;
pub use ser::{to_fl_slice_result, to_fl_slice_result_with_encoder, FlEncoderSession};
