mod enumer;

use couchbase_lite::{Enumerator, FallibleStreamingIterator};
use enumer::EnumeratorDeserializer;
use serde::Deserialize;
use serde_fleece::{NonNullConst};

pub use serde_fleece::Error;

pub fn from_query<'a, T>(mut result: Enumerator) -> Result<Vec<T>, Error> where T: Deserialize<'a> {
    let mut vec = Vec::<T>::new();

    while let Some(item) = match result.next() {
        Ok(result) => result,
        Err(_) => None,
    } {
        let mut column_deserializer = EnumeratorDeserializer::new(item.c4_enumerator().into());

        let deserialized_result = T::deserialize(&mut column_deserializer)?;

        vec.push(deserialized_result);
    }

    Ok(vec)
}
