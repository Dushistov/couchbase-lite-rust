use crate::{
    document::{C4DocumentOwner, Document},
    error::{c4error_init, Error, Result},
    ffi::{
        c4db_beginTransaction, c4db_endTransaction, c4db_getSharedFleeceEncoder, c4db_purgeDoc,
        c4doc_put, c4doc_update, kRevDeleted, C4DocPutRequest, C4ErrorCode, C4ErrorDomain, FLSlice,
        FLSliceResult,
    },
    Database,
};
use log::error;
use serde_fleece::FlEncoderSession;
use std::{
    ops::Deref,
    ptr::{self, NonNull},
};

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
    /// Removes all trace of a document and its revisions from the database.
    pub fn purge_by_id(&mut self, doc_id: &str) -> Result<()> {
        let mut c4err = c4error_init();
        if unsafe { c4db_purgeDoc(self.db.inner.0.as_ptr(), doc_id.into(), &mut c4err) } {
            Ok(())
        } else {
            Err(c4err.into())
        }
    }

    /// Get shared "fleece" encoder, `&mut self` to make possible
    /// exists only one session
    pub fn shared_encoder_session(&mut self) -> Result<FlEncoderSession> {
        let enc = unsafe { c4db_getSharedFleeceEncoder(self.db.inner.0.as_ptr()) };
        NonNull::new(enc)
            .ok_or_else(|| {
                Error::LogicError("c4db_getSharedFleeceEncoder return null.into()".into())
            })
            .map(FlEncoderSession::new)
    }

    fn main_save(&mut self, doc: &mut Document, deletion: bool) -> Result<()> {
        let mut retrying = false;
        let mut saving_doc = None;
        loop {
            if !retrying {
                if deletion && !doc.exists() {
                    return Err(Error::LogicError(format!(
                        "Cannot delete a document that has not yet been saved, doc_id {}",
                        doc.id()
                    )));
                }
                saving_doc = doc.inner.take();
            }
            let (rev_flags, body) = if !deletion {
                (
                    0,
                    doc.unsaved_body
                        .as_ref()
                        .map(FLSliceResult::as_fl_slice)
                        .unwrap_or_default(),
                )
            } else {
                (kRevDeleted, FLSlice::default())
            };
            retrying = false;
            let mut c4err = c4error_init();
            let new_doc = if let Some(doc) = saving_doc.as_mut() {
                unsafe { c4doc_update(doc.0.as_ptr(), body, rev_flags, &mut c4err) }
            } else {
                let rq = C4DocPutRequest {
                    body,
                    docID: doc.id().into(),
                    revFlags: rev_flags,
                    existingRevision: false,
                    allowConflict: false,
                    history: ptr::null(),
                    historyCount: 0,
                    save: true,
                    maxRevTreeDepth: 0,
                    remoteDBID: 0,
                    allocedBody: FLSliceResult::default(),
                    deltaCB: None,
                    deltaCBContext: ptr::null_mut(),
                    deltaSourceRevID: FLSlice::default(),
                };
                unsafe { c4doc_put(self.db.inner.0.as_ptr(), &rq, ptr::null_mut(), &mut c4err) }
            };
            if new_doc.is_null()
                && !(c4err.domain == C4ErrorDomain::LiteCoreDomain
                    && c4err.code == C4ErrorCode::kC4ErrorConflict.0)
            {
                return Err(c4err.into());
            }
            if let Some(new_doc) = NonNull::new(new_doc) {
                doc.replace_c4doc(Some(C4DocumentOwner(new_doc)));
            } else {
                saving_doc = match self.db.internal_get(doc.id(), true) {
                    Ok(x) => Some(x),
                    Err(Error::C4Error(c4err))
                        if deletion
                            && c4err.domain == C4ErrorDomain::LiteCoreDomain
                            && c4err.code == C4ErrorCode::kC4ErrorNotFound.0 =>
                    {
                        return Ok(());
                    }
                    Err(err) => return Err(err),
                };
                retrying = true;
            }

            if !retrying {
                break;
            }
        }
        Ok(())
    }
}

impl Deref for Transaction<'_> {
    type Target = Database;

    fn deref(&self) -> &Database {
        self.db
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.finished {
            if let Err(err) = self.end_transaction(false) {
                error!("end_transaction failed: {}", err);
            }
        }
    }
}
