use crate::{
    error::{c4error_init, Error},
    ffi::{
        c4db_encodeJSON, c4doc_bodyAsJSON, c4doc_free, c4doc_loadRevisionBody, kDocExists,
        kRevDeleted, C4Document, C4DocumentFlags, C4RevisionFlags,
    },
    fl_slice::{fl_slice_to_str_unchecked, AsFlSlice, FlSliceOwner},
    Database, Result,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{fmt, fmt::Debug, ptr::NonNull};
use uuid::Uuid;

#[derive(Debug)]
pub struct Document {
    id: String,
    unsaved_json5_body: Option<String>,
    pub(crate) inner: Option<C4DocumentOwner>,
}

impl Document {
    pub(crate) fn new_internal<S>(inner: C4DocumentOwner, doc_id: S) -> Self
    where
        S: Into<String>,
    {
        Self {
            inner: Some(inner),
            id: doc_id.into(),
            unsaved_json5_body: None,
        }
    }

    /// return the document's ID
    pub fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.unsaved_json5_body.is_none()
    }

    pub(crate) fn encode(&self, db: &Database) -> Result<FlSliceOwner> {
        if let Some(json5) = self.unsaved_json5_body.as_ref() {
            let mut c4err = c4error_init();
            let encoded = unsafe {
                c4db_encodeJSON(
                    db.inner.0.as_ptr(),
                    json5.as_bytes().as_flslice(),
                    &mut c4err,
                )
            };
            if !encoded.buf.is_null() {
                Ok(encoded.into())
            } else {
                return Err(c4err.into());
            }
        } else {
            Ok(FlSliceOwner::default())
        }
    }

    pub(crate) fn replace_c4doc(&mut self, doc: Option<C4DocumentOwner>) {
        self.inner = doc;
    }

    pub fn decode_data<T: DeserializeOwned>(self) -> Result<T> {
        if let Some(ref json) = self.unsaved_json5_body {
            let x: T = json5::from_str(&json)?;
            return Ok(x);
        }
        let inner: &C4DocumentOwner = self.inner.as_ref().ok_or_else(|| {
            Error::LogicError(format!(
                "Document {} have no underlying C4Document",
                self.id
            ))
        })?;
        load_body(inner.0)?;
        let mut c4err = c4error_init();
        let body = unsafe { c4doc_bodyAsJSON(inner.0.as_ptr(), true, &mut c4err) };
        if body.buf.is_null() {
            return Err(c4err.into());
        }
        let body: FlSliceOwner = body.into();
        let x: T = serde_json::from_slice(body.as_bytes())?;
        Ok(x)
    }

    pub fn new_with_id<S, T>(doc_id: S, data: &T) -> Result<Self>
    where
        S: Into<String>,
        T: Serialize,
    {
        Ok(Self {
            inner: None,
            id: doc_id.into(),
            unsaved_json5_body: Some(json5::to_string(data)?),
        })
    }

    pub fn new<T>(data: &T) -> Result<Self>
    where
        T: Serialize,
    {
        Ok(Self {
            inner: None,
            id: Uuid::new_v4().to_hyphenated().to_string(),
            unsaved_json5_body: Some(json5::to_string(data)?),
        })
    }

    pub fn new_with_id_json5<S: Into<String>>(doc_id: S, json5_str: String) -> Result<Self> {
        Ok(Self {
            inner: None,
            id: doc_id.into(),
            unsaved_json5_body: Some(json5_str),
        })
    }

    /// Update internal buffer with data, you need save document
    /// to database to make this change permanent
    pub fn update_data<T>(&mut self, data: &T) -> Result<()>
    where
        T: Serialize,
    {
        let body = json5::to_string(data)?;
        self.unsaved_json5_body = Some(body);
        Ok(())
    }

    pub(crate) fn exists(&self) -> bool {
        self.inner.as_ref().map(|x| x.exists()).unwrap_or(false)
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

#[repr(transparent)]
pub(crate) struct C4DocumentOwner(pub(crate) NonNull<C4Document>);

impl Debug for C4DocumentOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", unsafe { self.0.as_ref() })
    }
}

impl C4DocumentOwner {
    pub(crate) fn exists(&self) -> bool {
        (self.flags() & kDocExists) == kDocExists
    }

    pub(crate) fn is_deleted(&self) -> bool {
        (self.selected_flags() & (kRevDeleted as C4RevisionFlags))
            == (kRevDeleted as C4RevisionFlags)
    }

    fn flags(&self) -> C4DocumentFlags {
        unsafe { self.0.as_ref().flags }
    }

    fn selected_flags(&self) -> C4RevisionFlags {
        unsafe { self.0.as_ref().selectedRev.flags }
    }

    pub(crate) fn id(&self) -> &str {
        unsafe { fl_slice_to_str_unchecked(self.0.as_ref().docID) }
    }
}

impl Drop for C4DocumentOwner {
    fn drop(&mut self) {
        unsafe { c4doc_free(self.0.as_ptr()) };
    }
}
