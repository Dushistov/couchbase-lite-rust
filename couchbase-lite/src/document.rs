use crate::{
    error::{c4error_init, Error, Result},
    ffi::{
        c4doc_getRevisionBody, c4doc_loadRevisionBody, c4doc_release, c4rev_getGeneration,
        kDocConflicted, kDocDeleted, kDocExists, kDocHasAttachments, C4Document, C4DocumentFlags,
        C4Revision, FLSliceResult,
    },
};
use bitflags::bitflags;
use serde::{de::DeserializeOwned, Serialize};
use serde_fleece::{to_fl_slice_result_with_encoder, FlEncoderSession};
use std::{os::raw::c_uint, ptr::NonNull, str};
use uuid::Uuid;

#[derive(Debug)]
pub struct Document {
    id: String,
    pub(crate) unsaved_body: Option<FLSliceResult>,
    pub(crate) inner: Option<C4DocumentOwner>,
}

bitflags! {
    pub struct DocumentFlags: u32 {
        /// The document's current revision is deleted.
        const DELETED         = kDocDeleted;
        /// The document is in conflict.
        const CONFLICTED      = kDocConflicted;
        /// The document's current revision has attachments.
        const HAS_ATTACHMENTS = kDocHasAttachments;
        /// The document exists (i.e. has revisions.)
        const EXISTS = kDocExists;
    }
}

impl Document {
    #[inline]
    pub fn new<T>(data: &T, enc: FlEncoderSession) -> Result<Self>
    where
        T: Serialize,
    {
        let unsaved_body = Some(to_fl_slice_result_with_encoder(data, enc)?);
        Ok(Self {
            inner: None,
            unsaved_body,
            id: Uuid::new_v4().hyphenated().to_string(),
        })
    }
    #[inline]
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
    #[inline]
    pub fn new_with_id_fleece<S: Into<String>>(doc_id: S, fleece_data: FLSliceResult) -> Self {
        Self {
            inner: None,
            id: doc_id.into(),
            unsaved_body: Some(fleece_data),
        }
    }
    /// return the document's ID
    #[inline]
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
        let body = inner.load_body()?;
        let x: T = serde_fleece::from_slice(body)?;
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

    /// Returns a document's current sequence in the local database.
    /// This number increases every time the document is saved, and a more recently saved document
    /// will have a greater sequence number than one saved earlier, so sequences may be used as an
    /// abstract 'clock' to tell relative modification times
    #[inline]
    pub fn sequence(&self) -> Option<u64> {
        self.inner
            .as_ref()
            .map(|p| unsafe { p.0.as_ref() }.sequence)
    }

    /// Returns a document's revision ID, which is a short opaque string that's guaranteed to be
    /// unique to every change made to the document.
    #[inline]
    pub fn revision_id(&self) -> Option<&str> {
        self.inner
            .as_ref()
            .map(|p| str::from_utf8(p.revision_id()).ok())
            .unwrap_or(None)
    }

    #[inline]
    pub fn flags(&self) -> Option<DocumentFlags> {
        self.inner
            .as_ref()
            .map(|p| DocumentFlags::from_bits_truncate(p.flags()))
    }

    #[inline]
    pub fn generation(&self) -> c_uint {
        self.inner
            .as_ref()
            .map(|d| C4DocumentOwner::generation(d.revision_id()))
            .unwrap_or(0)
    }

    /// Just check `Document::flags` to see if document exists
    #[inline]
    pub fn exists(&self) -> bool {
        self.inner.as_ref().map(|x| x.exists()).unwrap_or(false)
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
    pub(crate) fn exists(&self) -> bool {
        (self.flags() & kDocExists) == kDocExists
    }
    fn flags(&self) -> C4DocumentFlags {
        unsafe { self.0.as_ref().flags }
    }
    pub(crate) fn selected_revision(&self) -> &C4Revision {
        &unsafe { self.0.as_ref() }.selectedRev
    }
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
    pub(crate) fn revision_id(&self) -> &[u8] {
        unsafe { self.0.as_ref() }.revID.as_fl_slice().into()
    }
    pub(crate) fn generation(rev_id: &[u8]) -> c_uint {
        unsafe { c4rev_getGeneration(rev_id.into()) }
    }
    pub(crate) fn load_body(&self) -> Result<&[u8]> {
        let mut c4err = c4error_init();
        if unsafe { c4doc_loadRevisionBody(self.0.as_ptr(), &mut c4err) } {
            Ok(unsafe { c4doc_getRevisionBody(self.0.as_ptr()) }.into())
        } else {
            Err(c4err.into())
        }
    }
}
