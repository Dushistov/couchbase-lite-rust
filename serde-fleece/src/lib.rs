mod de;
mod dict;
mod error;
mod ser;

pub use couchbase_lite_core_sys as ffi;
pub use de::{from_fl_dict, from_fl_value, from_slice, NonNullConst};
pub use dict::{Dict, MutableDict};
pub use error::Error;
pub use ser::{
    json_to_fleece_with_encoder, to_fl_slice_result, to_fl_slice_result_with_encoder, EncodeValue,
    FlEncoderSession,
};
