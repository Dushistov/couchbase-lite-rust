use std::ptr::NonNull;

use crate::{
    error::{c4error_init, Error, Result},
    ffi::{c4doc_release, kDocExists, C4Document, C4DocumentFlags},
};
use couchbase_lite_core_sys::{c4doc_getRevisionBody, c4doc_loadRevisionBody, FLSliceResult};
use serde::{de::DeserializeOwned, Serialize};
use serde_fleece::{to_fl_slice_result_with_encoder, FlEncoderSession};
use uuid::Uuid;

#[derive(Debug)]
pub struct Document {
    id: String,
    pub(crate) unsaved_body: Option<FLSliceResult>,
    pub(crate) inner: Option<C4DocumentOwner>,
}

impl Document {
    pub fn new<T>(data: &T, enc: FlEncoderSession) -> Result<Self>
    where
        T: Serialize,
    {
        let unsaved_body = Some(to_fl_slice_result_with_encoder(data, enc)?);
        Ok(Self {
            inner: None,
            unsaved_body,
            id: Uuid::new_v4().to_hyphenated().to_string(),
        })
    }
    pub fn new_with_id<S, T>(doc_id: S, data: &T, enc: FlEncoderSession) -> Result<Self>
    where
        S: Into<String>,
        T: Serialize,
    {
        let unsaved_body = Some(to_fl_slice_result_with_encoder(data, enc)?);
        Ok(Self {
            inner: None,
            id: doc_id.into(),
            unsaved_body,
        })
    }
    pub fn new_with_id_fleece<S: Into<String>>(
        doc_id: S,
        fleece_data: FLSliceResult,
    ) -> Result<Self> {
        Ok(Self {
            inner: None,
            id: doc_id.into(),
            unsaved_body: Some(fleece_data),
        })
    }
    /// return the document's ID
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Decode body of document
    pub fn decode_body<T: DeserializeOwned>(self) -> Result<T> {
        if let Some(slice) = self.unsaved_body.as_ref().map(FLSliceResult::as_fl_slice) {
            let x: T = serde_fleece::from_slice(slice.into())?;
            return Ok(x);
        }
        let inner: &C4DocumentOwner = self.inner.as_ref().ok_or_else(|| {
            Error::LogicError(format!(
                "Document {} have no underlying C4Document",
                self.id
            ))
        })?;
        load_body(inner.0)?;
        let body = unsafe { c4doc_getRevisionBody(inner.0.as_ptr()) };
        let x: T = serde_fleece::from_slice(body.into())?;
        Ok(x)
    }
    /// Update internal buffer with data, you need save document
    /// to database to make this change permanent
    pub fn update_body<T>(&mut self, data: &T, enc: FlEncoderSession) -> Result<()>
    where
        T: Serialize,
    {
        let body = to_fl_slice_result_with_encoder(data, enc)?;
        self.unsaved_body = Some(body);
        Ok(())
    }

    pub(crate) fn new_internal<S>(inner: C4DocumentOwner, doc_id: S) -> Self
    where
        S: Into<String>,
    {
        Self {
            inner: Some(inner),
            id: doc_id.into(),
            unsaved_body: None,
        }
    }
    pub(crate) fn replace_c4doc(&mut self, doc: Option<C4DocumentOwner>) {
        self.inner = doc;
    }
    pub(crate) fn exists(&self) -> bool {
        self.inner.as_ref().map(|x| x.exists()).unwrap_or(false)
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub(crate) struct C4DocumentOwner(pub(crate) NonNull<C4Document>);

impl Drop for C4DocumentOwner {
    fn drop(&mut self) {
        unsafe { c4doc_release(self.0.as_ptr()) };
    }
}

impl C4DocumentOwner {
    #[inline]
    pub(crate) fn exists(&self) -> bool {
        (self.flags() & kDocExists) == kDocExists
    }
    #[inline]
    fn flags(&self) -> C4DocumentFlags {
        unsafe { self.0.as_ref().flags }
    }
    #[inline]
    pub(crate) fn id(&self) -> Result<&str> {
        unsafe {
            self.0
                .as_ref()
                .docID
                .as_fl_slice()
                .try_into()
                .map_err(|_| Error::InvalidUtf8)
        }
    }
}

fn load_body(inner: NonNull<C4Document>) -> Result<()> {
    let mut c4err = c4error_init();
    if unsafe { c4doc_loadRevisionBody(inner.as_ptr(), &mut c4err) } {
        Ok(())
    } else {
        Err(c4err.into())
    }
}
