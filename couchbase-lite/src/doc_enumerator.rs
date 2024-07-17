use crate::{
    document::{C4DocumentOwner, Document},
    error::{c4error_init, Error, Result},
    ffi::{
        c4db_enumerateAllDocs, c4enum_free, c4enum_getDocument, c4enum_getDocumentInfo,
        c4enum_next, C4DocEnumerator, C4DocumentInfo, C4EnumeratorFlags, C4EnumeratorOptions,
    },
    Database,
};
use bitflags::bitflags;
use fallible_streaming_iterator::FallibleStreamingIterator;
use std::{marker::PhantomData, mem::MaybeUninit, ptr::NonNull, str};

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

pub struct DocumentInfo<'a, 'b> {
    inner: C4DocumentInfo,
    phantom: PhantomData<&'a DocEnumerator<'b>>,
}

impl DocumentInfo<'_, '_> {
    pub(crate) fn new(inner: C4DocumentInfo) -> Self {
        Self {
            inner,
            phantom: PhantomData,
        }
    }
    #[inline]
    pub fn doc_id(&self) -> &str {
        unsafe { str::from_utf8_unchecked(self.inner.docID.into()) }
    }
}

impl<'a> DocEnumerator<'a> {
    pub(crate) fn enumerate_all_docs(
        db: &'a Database,
        flags: DocEnumeratorFlags,
    ) -> Result<DocEnumerator<'a>> {
        let mut c4err = c4error_init();
        let opts = C4EnumeratorOptions {
            flags: C4EnumeratorFlags(flags.bits()),
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

    #[inline]
    pub fn get_doc_info(&self) -> Result<Option<DocumentInfo>> {
        let mut di = MaybeUninit::<C4DocumentInfo>::uninit();
        if !unsafe { c4enum_getDocumentInfo(self.inner.as_ptr(), di.as_mut_ptr()) } {
            return Ok(None);
        }
        let di = unsafe { di.assume_init() };

        Ok(Some(DocumentInfo::new(di)))
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
    #[derive(Debug)]
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
