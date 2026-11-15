use crate::{
    document::C4DocumentOwner,
    error::{c4error_init, Error, Result},
    ffi::{
        c4doc_resolveConflict, c4doc_save, c4doc_selectNextLeafRevision, C4DocContentLevel,
        C4ErrorCode, C4ErrorDomain, C4RevisionFlags, FLSlice, FLSlice_Compare,
    },
    Database,
};
use log::{info, warn};
use std::{os::raw::c_uint, ptr};

/// Resolves a replication conflict in a document
pub fn resolve_conflict(db: &mut Database, doc_id: &str) -> Result<()> {
    resolve_conflict_with(db, doc_id, default_resolve_conflict)
}

fn resolve_conflict_with(
    db: &mut Database,
    doc_id: &str,
    mut resolve: impl FnMut(&mut Database, &str, &C4DocumentOwner) -> Result<()>,
) -> Result<()> {
    const MAX_ATTEMPTS: u8 = 10;
    let mut attempt = 0;
    loop {
        let doc = match db.do_internal_get_opt(doc_id, true, C4DocContentLevel::kDocGetAll)? {
            Some(x) => x,
            None => {
                info!("doc {doc_id} no longer exists, no conflict to resolve");
                return Ok(());
            }
        };

        let ok = select_next_conflicting_revision(&doc)?;
        if !ok {
            info!("conflict in doc {doc_id} already resolved, nothing to do");
            return Ok(());
        }
        match resolve(db, doc_id, &doc) {
            Ok(()) => return Ok(()),
            Err(Error::C4Error(err))
                if err.domain == C4ErrorDomain::LiteCoreDomain
                    && err.code == C4ErrorCode::kC4ErrorConflict.0 =>
            {
                attempt += 1;
                if attempt == MAX_ATTEMPTS {
                    return Err(Error::C4Error(err));
                }
                warn!(
                    "conflict resolution of doc '{}' conflicted with newer saved, retry {}",
                    doc_id, attempt
                );
            }
            Err(err) => return Err(err),
        }
    }
}

fn default_resolve_conflict(
    db: &mut Database,
    doc_id: &str,
    conflict: &C4DocumentOwner,
) -> Result<()> {
    let remote_doc = if (conflict.selected_revision().flags & C4RevisionFlags::kRevDeleted)
        != C4RevisionFlags(0)
    {
        None
    } else {
        Some(conflict)
    };
    let local_doc = db
        .do_internal_get_opt(doc_id, true, C4DocContentLevel::kDocGetAll)?
        .map(|doc| {
            if (doc.selected_revision().flags & C4RevisionFlags::kRevDeleted) != C4RevisionFlags(0)
            {
                None
            } else {
                Some(doc)
            }
        })
        .unwrap_or(None);

    let resolved = default_conflict_resolver(local_doc.as_ref(), remote_doc);
    let resolution = if resolved.map(|x| x.0.as_ptr()).unwrap_or(ptr::null_mut())
        == remote_doc.map(|x| x.0.as_ptr()).unwrap_or(ptr::null_mut())
    {
        Resolution::UseRemote
    } else {
        Resolution::UseLocal
    };
    do_resolve_conflict(db, conflict, resolution, resolved)
}

fn default_conflict_resolver<'b>(
    local_doc: Option<&'b C4DocumentOwner>,
    remote_doc: Option<&'b C4DocumentOwner>,
) -> Option<&'b C4DocumentOwner> {
    fn selected_rev_gen(doc: &C4DocumentOwner) -> (&[u8], c_uint) {
        let rev_id: &[u8] = unsafe {
            doc.selected_revision()
                .revID
                .as_fl_slice()
                .as_bytes_unchecked()
        };
        (rev_id, C4DocumentOwner::generation(rev_id))
    }
    match (local_doc, remote_doc) {
        (None, None) | (None, Some(_)) | (Some(_), None) => None,
        (Some(local_doc), Some(remote_doc)) => {
            let (remote_rev, remote_gen) = selected_rev_gen(remote_doc);
            let (local_rev, local_gen) = selected_rev_gen(local_doc);
            if remote_gen > local_gen {
                Some(remote_doc)
            } else if remote_gen < local_gen {
                Some(local_doc)
            } else if unsafe { FLSlice_Compare(local_rev.into(), remote_rev.into()) } > 0 {
                Some(local_doc)
            } else {
                Some(remote_doc)
            }
        }
    }
}

fn select_next_conflicting_revision(doc: &C4DocumentOwner) -> Result<bool> {
    let mut c4err = c4error_init();
    while unsafe { c4doc_selectNextLeafRevision(doc.0.as_ptr(), true, true, &mut c4err) } {
        if (doc.selected_revision().flags & C4RevisionFlags::kRevIsConflict) != C4RevisionFlags(0) {
            return Ok(true);
        }
    }
    if c4err.code == 0 {
        Ok(false)
    } else {
        Err(Error::C4Error(c4err))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Resolution {
    UseRemote,
    UseLocal,
}

fn do_resolve_conflict(
    db: &mut Database,
    conflict_doc: &C4DocumentOwner,
    resolution: Resolution,
    resolved_doc: Option<&C4DocumentOwner>,
) -> Result<()> {
    let tx = db.transaction()?;
    // Remote Revision always win so that the resolved revision will not conflict with the remote:
    let winner = unsafe { conflict_doc.0.as_ref() }
        .selectedRev
        .revID
        .as_fl_slice();
    let loser = conflict_doc.revision_id();
    let mut merge_flags = C4RevisionFlags(0);
    let mut merge_body = FLSlice::default();
    // When useLocal (local wins) or useMerge is true, the new revision will be created
    // under the remote branch which is the winning branch. When useRemote (remote wins)
    // is true, the remote revision will be kept as is and the losing branch will be pruned.
    if resolution != Resolution::UseRemote {
        if let Some(resolved_doc) = resolved_doc {
            let body = resolved_doc.load_body()?;
            merge_body = body.into();
            merge_flags =
                resolved_doc.selected_revision().flags & C4RevisionFlags::kRevHasAttachments;
        } else {
            merge_flags = C4RevisionFlags::kRevDeleted;
            // A null body tells LiteCore to keep the winning revision. A non-null,
            // empty body makes it create the deletion revision instead.
            static EMPTY_MERGE_BODY: u8 = 0;
            merge_body = FLSlice {
                buf: (&EMPTY_MERGE_BODY as *const u8).cast(),
                size: 0,
            };
        }
    }
    let mut c4err = c4error_init();
    if !unsafe {
        c4doc_resolveConflict(
            conflict_doc.0.as_ptr(),
            winner,
            loser.into(),
            merge_body,
            merge_flags,
            &mut c4err,
        )
    } {
        return Err(c4err.into());
    }

    if !unsafe { c4doc_save(conflict_doc.0.as_ptr(), 0, &mut c4err) } {
        return Err(c4err.into());
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ffi::*, DatabaseFlags, DocumentFlags};

    fn encode_body(db: *mut C4Database, json: &str) -> FLSliceResult {
        unsafe {
            let encoder = c4db_getSharedFleeceEncoder(db);
            FLEncoder_Reset(encoder);
            assert!(FLEncoder_ConvertJSON(encoder, json.into()));
            let mut error = FLError::kFLNoError;
            let body = FLEncoder_Finish(encoder, &mut error);
            assert_eq!(error, FLError::kFLNoError);
            FLEncoder_Reset(encoder);
            body
        }
    }

    fn put_revision(
        db: *mut C4Database,
        history: &[C4String],
        body: FLSlice,
        flags: C4RevisionFlags,
        remote: bool,
    ) {
        let request = C4DocPutRequest {
            body,
            docID: "conflict".into(),
            revFlags: flags,
            existingRevision: true,
            allowConflict: true,
            history: history.as_ptr(),
            historyCount: history.len(),
            save: true,
            maxRevTreeDepth: 0,
            remoteDBID: if remote { 1 } else { 0 },
            allocedBody: FLSliceResult::default(),
            deltaCB: None,
            deltaCBContext: ptr::null_mut(),
            deltaSourceRevID: FLSlice::default(),
        };
        let mut error = c4error_init();
        let doc = unsafe { c4doc_put(db, &request, ptr::null_mut(), &mut error) };
        assert!(!doc.is_null(), "put failed: {:?}", Error::C4Error(error));
        unsafe { c4doc_release(doc) };
    }

    fn live_conflict() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conflict.cblite2");
        let mut db = Database::open_with_flags(&path, DatabaseFlags::CREATE).unwrap();
        {
            let transaction = db.transaction().unwrap();
            let raw_db = transaction.inner.0.as_ptr();
            let local_body = encode_body(raw_db, r#"{"value":"local"}"#);
            let remote_body = encode_body(raw_db, r#"{"value":"remote"}"#);
            put_revision(
                raw_db,
                &["3-cccc".into(), "2-aaaa".into(), "1-aaaa".into()],
                local_body.as_fl_slice(),
                C4RevisionFlags(0),
                false,
            );
            put_revision(
                raw_db,
                &["2-bbbb".into(), "1-aaaa".into()],
                remote_body.as_fl_slice(),
                C4RevisionFlags::kRevKeepBody,
                true,
            );
            transaction.commit().unwrap();
        }
        assert!(db
            .get_existing("conflict")
            .unwrap()
            .flags()
            .unwrap()
            .contains(DocumentFlags::CONFLICTED));
        (dir, db)
    }

    fn conflict_error() -> Error {
        Error::C4Error(C4Error {
            domain: C4ErrorDomain::LiteCoreDomain,
            code: C4ErrorCode::kC4ErrorConflict.0,
            internal_info: 0,
        })
    }

    #[test]
    fn test_retry_after_conflict_error() {
        let (_dir, mut db) = live_conflict();
        let mut attempts = 0;
        resolve_conflict_with(&mut db, "conflict", |db, doc_id, conflict| {
            attempts += 1;
            if attempts == 1 {
                Err(conflict_error())
            } else {
                default_resolve_conflict(db, doc_id, conflict)
            }
        })
        .unwrap();

        assert_eq!(attempts, 2);
        assert!(!db
            .get_existing("conflict")
            .unwrap()
            .flags()
            .unwrap()
            .contains(DocumentFlags::CONFLICTED));
    }

    #[test]
    fn test_conflict_retry_limit_returns_error() {
        let (_dir, mut db) = live_conflict();
        let mut attempts = 0;
        let error = resolve_conflict_with(&mut db, "conflict", |_, _, _| {
            attempts += 1;
            Err(conflict_error())
        })
        .unwrap_err();

        assert_eq!(attempts, 10);
        match error {
            Error::C4Error(err) => {
                assert_eq!(err.domain, C4ErrorDomain::LiteCoreDomain);
                assert_eq!(err.code, C4ErrorCode::kC4ErrorConflict.0);
            }
            other => panic!("expected LiteCore conflict, got {other:?}"),
        }
        assert!(db
            .get_existing("conflict")
            .unwrap()
            .flags()
            .unwrap()
            .contains(DocumentFlags::CONFLICTED));
    }

    #[test]
    fn test_local_deletion_wins_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conflict.cblite2");
        let mut db = Database::open_with_flags(&path, DatabaseFlags::CREATE).unwrap();

        {
            let transaction = db.transaction().unwrap();
            let raw_db = transaction.inner.0.as_ptr();
            let deleted_body = encode_body(raw_db, "{}");
            let remote_body = encode_body(raw_db, r#"{"value":"remote"}"#);
            put_revision(
                raw_db,
                &["3-cccc".into(), "2-aaaa".into(), "1-aaaa".into()],
                deleted_body.as_fl_slice(),
                C4RevisionFlags::kRevDeleted,
                false,
            );
            put_revision(
                raw_db,
                &["2-bbbb".into(), "1-aaaa".into()],
                remote_body.as_fl_slice(),
                C4RevisionFlags::kRevKeepBody,
                true,
            );
            transaction.commit().unwrap();
        }

        let before = db.get_existing("conflict").unwrap();
        assert_eq!(before.revision_id(), Some("3-cccc"));
        let before_flags = before.flags().unwrap();
        assert!(before_flags.contains(DocumentFlags::DELETED));
        assert!(before_flags.contains(DocumentFlags::CONFLICTED));
        drop(before);

        resolve_conflict(&mut db, "conflict").unwrap();

        let resolved = db.get_existing("conflict").unwrap();
        let flags = resolved.flags().unwrap();
        assert!(flags.contains(DocumentFlags::DELETED));
        assert!(!flags.contains(DocumentFlags::CONFLICTED));
        assert_eq!(db.document_count(), 0);

        drop(resolved);
        drop(db);
        let reopened = Database::open_with_flags(&path, DatabaseFlags::empty()).unwrap();
        assert!(reopened
            .get_existing("conflict")
            .unwrap()
            .flags()
            .unwrap()
            .contains(DocumentFlags::DELETED));
    }

    #[test]
    fn test_local_attachment_flag_survives_conflict_resolution() {
        #[derive(serde::Deserialize)]
        struct Blob {
            digest: String,
        }

        #[derive(serde::Deserialize)]
        struct Body {
            value: String,
            blob: Blob,
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conflict.cblite2");
        let mut db = Database::open_with_flags(&path, DatabaseFlags::CREATE).unwrap();

        {
            let transaction = db.transaction().unwrap();
            let raw_db = transaction.inner.0.as_ptr();
            let local_body = encode_body(
                raw_db,
                r#"{"value":"local","blob":{"@type":"blob","digest":"sha1-2jmj7l5rSw0yVb/vlWAYkK/YBwk=","length":0}}"#,
            );
            let remote_body = encode_body(raw_db, r#"{"value":"remote"}"#);
            put_revision(
                raw_db,
                &["3-cccc".into(), "2-aaaa".into(), "1-aaaa".into()],
                local_body.as_fl_slice(),
                C4RevisionFlags::kRevHasAttachments,
                false,
            );
            put_revision(
                raw_db,
                &["2-bbbb".into(), "1-aaaa".into()],
                remote_body.as_fl_slice(),
                C4RevisionFlags::kRevKeepBody,
                true,
            );
            transaction.commit().unwrap();
        }

        let before = db.get_existing("conflict").unwrap();
        assert_eq!(before.revision_id(), Some("3-cccc"));
        let before_flags = before.flags().unwrap();
        assert!(before_flags.contains(DocumentFlags::CONFLICTED));
        assert!(before_flags.contains(DocumentFlags::HAS_ATTACHMENTS));
        drop(before);

        resolve_conflict(&mut db, "conflict").unwrap();

        let resolved = db.get_existing("conflict").unwrap();
        let flags = resolved.flags().unwrap();
        assert!(!flags.contains(DocumentFlags::CONFLICTED));
        assert!(flags.contains(DocumentFlags::HAS_ATTACHMENTS));
        drop(resolved);

        let body: Body = db.get_existing("conflict").unwrap().decode_body().unwrap();
        assert_eq!(body.value, "local");
        assert_eq!(body.blob.digest, "sha1-2jmj7l5rSw0yVb/vlWAYkK/YBwk=");

        drop(db);
        let reopened = Database::open_with_flags(&path, DatabaseFlags::empty()).unwrap();
        assert!(reopened
            .get_existing("conflict")
            .unwrap()
            .flags()
            .unwrap()
            .contains(DocumentFlags::HAS_ATTACHMENTS));
    }
}
