use crate::{
    document::{C4DocumentOwner, Document},
    error::{c4error_init, Error},
    ffi::{
        c4db_beginTransaction, c4db_endTransaction, c4doc_create, c4doc_update, kC4ErrorConflict,
        kC4ErrorNotFound, kRevDeleted, C4Document, C4RevisionFlags, LiteCoreDomain,
    },
    fl_slice::{AsFlSlice, FlSliceOwner},
    Database, Result,
};
use std::{ops::Deref, ptr::NonNull};

pub struct Transaction<'db> {
    db: &'db Database,
    finished: bool,
}

impl Transaction<'_> {
    pub(crate) fn new(db: &mut Database) -> Result<Transaction> {
        let mut c4err = c4error_init();
        if unsafe { c4db_beginTransaction(db.inner.0.as_ptr(), &mut c4err) } {
            Ok(Transaction {
                db,
                finished: false,
            })
        } else {
            Err(c4err.into())
        }
    }

    pub fn commit(mut self) -> Result<()> {
        self.end_transaction(true)
    }

    fn end_transaction(&mut self, commit: bool) -> Result<()> {
        self.finished = true;
        let mut c4err = c4error_init();
        if unsafe { c4db_endTransaction(self.db.inner.0.as_ptr(), commit, &mut c4err) } {
            Ok(())
        } else {
            Err(c4err.into())
        }
    }

    pub fn save(&mut self, doc: &mut Document) -> Result<()> {
        self.main_save(doc, false)
    }

    pub fn delete(&mut self, doc: &mut Document) -> Result<()> {
        self.main_save(doc, true)
    }

    fn main_save(&mut self, doc: &mut Document, deletion: bool) -> Result<()> {
        if deletion && !doc.exists() {
            return Err(Error::LogicError(format!(
                "Cannot delete a document that has not yet been saved, doc_id {}",
                doc.id()
            )));
        }
        let mut new_doc = match self.internal_save(doc, None, deletion) {
            Ok(x) => Some(x),
            Err(Error::DbError(c4err))
                if c4err.domain == LiteCoreDomain && c4err.code == (kC4ErrorConflict as i32) =>
            {
                None
            }
            Err(err) => {
                return Err(err);
            }
        };
        if new_doc.is_none() {
            let cur_doc = match self.db.internal_get(doc.id(), true) {
                Ok(x) => x,
                Err(Error::DbError(c4err))
                    if deletion
                        && c4err.domain == LiteCoreDomain
                        && c4err.code == (kC4ErrorNotFound as i32) =>
                {
                    return Ok(());
                }
                Err(err) => return Err(err),
            };
            if deletion && cur_doc.is_deleted() {
                doc.replace_c4doc(Some(cur_doc));
                return Ok(());
            }
            new_doc = Some(self.internal_save(doc, Some(cur_doc), deletion)?);
        }
        doc.replace_c4doc(new_doc);

        Ok(())
    }

    fn internal_save(
        &mut self,
        doc: &mut Document,
        base: Option<C4DocumentOwner>,
        deletion: bool,
    ) -> Result<C4DocumentOwner> {
        let mut body = FlSliceOwner::default();
        let mut rev_flags: C4RevisionFlags = 0;
        if deletion {
            rev_flags = kRevDeleted as C4RevisionFlags;
        }
        if !deletion && !doc.is_empty() {
            body = doc.encode(self.db)?;
        }
        let c4_doc: Option<NonNull<C4Document>> = if let Some(x) = base.as_ref() {
            Some(x.0)
        } else {
            doc.inner.as_ref().map(|x| x.0)
        };
        let mut c4err = c4error_init();
        let new_doc = if let Some(c4_doc) = c4_doc {
            unsafe {
                c4doc_update(
                    c4_doc.as_ptr(),
                    body.as_bytes().as_flslice(),
                    rev_flags,
                    &mut c4err,
                )
            }
        } else {
            unsafe {
                c4doc_create(
                    self.db.inner.0.as_ptr(),
                    doc.id().as_bytes().as_flslice(),
                    body.as_bytes().as_flslice(),
                    rev_flags,
                    &mut c4err,
                )
            }
        };

        NonNull::new(new_doc)
            .map(C4DocumentOwner)
            .ok_or_else(|| c4err.into())
    }
}

impl Deref for Transaction<'_> {
    type Target = Database;

    fn deref(&self) -> &Database {
        self.db
    }
}

impl Drop for Transaction<'_> {
    #[allow(unused_must_use)]
    fn drop(&mut self) {
        if !self.finished {
            self.end_transaction(false);
        }
    }
}
