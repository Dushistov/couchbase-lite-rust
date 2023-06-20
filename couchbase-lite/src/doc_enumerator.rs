use crate::{
    document::{C4DocumentOwner, Document},
    error::{c4error_init, Error, Result},
    ffi::{
        c4db_enumerateAllDocs, c4enum_free, c4enum_getDocument, c4enum_next, C4DocEnumerator,
        C4EnumeratorFlags, C4EnumeratorOptions,
    },
    Database,
};
use bitflags::bitflags;
use fallible_streaming_iterator::FallibleStreamingIterator;
use std::ptr::NonNull;

pub struct DocEnumerator<'a> {
    _db: &'a Database,
    reach_end: bool,
    inner: NonNull<C4DocEnumerator>,
}

impl Drop for DocEnumerator<'_> {
    #[inline]
    fn drop(&mut self) {
        unsafe { c4enum_free(self.inner.as_ptr()) };
    }
}

impl<'a> DocEnumerator<'a> {
    pub(crate) fn enumerate_all_docs(
        db: &'a Database,
        flags: DocEnumeratorFlags,
    ) -> Result<DocEnumerator<'a>> {
        let mut c4err = c4error_init();
        let opts = C4EnumeratorOptions {
            flags: C4EnumeratorFlags(flags.bits),
        };
        let enum_ptr = unsafe { c4db_enumerateAllDocs(db.inner.0.as_ptr(), &opts, &mut c4err) };
        NonNull::new(enum_ptr)
            .map(|inner| DocEnumerator {
                _db: db,
                inner,
                reach_end: false,
            })
            .ok_or_else(|| c4err.into())
    }

    pub fn get_doc(&self) -> Result<Document> {
        let mut c4err = c4error_init();
        let doc_ptr = unsafe { c4enum_getDocument(self.inner.as_ptr(), &mut c4err) };
        let c4doc: C4DocumentOwner =
            NonNull::new(doc_ptr).map(C4DocumentOwner).ok_or_else(|| {
                let err: Error = c4err.into();
                err
            })?;
        let id: String = c4doc.id()?.into();
        Ok(Document::new_internal(c4doc, id))
    }
}

impl<'en> FallibleStreamingIterator for DocEnumerator<'en> {
    type Error = crate::error::Error;
    type Item = DocEnumerator<'en>;

    fn advance(&mut self) -> Result<()> {
        if self.reach_end {
            return Ok(());
        }
        let mut c4err = c4error_init();
        if unsafe { c4enum_next(self.inner.as_ptr(), &mut c4err) } {
            Ok(())
        } else if c4err.code == 0 {
            self.reach_end = true;
            Ok(())
        } else {
            Err(c4err.into())
        }
    }

    #[inline]
    fn get(&self) -> Option<&DocEnumerator<'en>> {
        if !self.reach_end {
            Some(self)
        } else {
            None
        }
    }
}

bitflags! {
    pub struct DocEnumeratorFlags: u16 {
        /// If true, iteration goes by descending document IDs
        const DESCENDING           = 0x01;
        /// If true, include deleted documents
        const INCLUDE_DELETED       = 0x08;
        /// If false, include _only_ documents in conflict
        const INCLUDE_NON_CONFLICTED = 0x10;
        /// If false, document bodies will not be preloaded, just
        /// metadata (docID, revID, sequence, flags.) This is faster if you
        /// don't need to access the revision tree or revision bodies. You
        /// can still access all the data of the document, but it will
        /// trigger loading the document body from the database. */
        const INCLUDE_BODIES        = 0x20;

    }
}
impl Default for DocEnumeratorFlags {
    #[inline]
    fn default() -> Self {
        DocEnumeratorFlags::INCLUDE_BODIES | DocEnumeratorFlags::INCLUDE_NON_CONFLICTED
    }
}
