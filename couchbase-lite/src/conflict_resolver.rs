use crate::{
    document::C4DocumentOwner,
    error::{c4error_init, Error, Result},
    ffi::{
        c4doc_resolveConflict, c4doc_save, c4doc_selectNextLeafRevision, c4doc_selectRevision,
        kRevDeleted, kRevIsConflict, kRevLeaf, C4DocContentLevel, C4RevisionFlags, FLSlice,
        FLSlice_Compare,
    },
    Database,
};
use log::{info, warn};
use std::{borrow::Cow, ptr};

/// Resolves a replication conflict in a document
pub fn resolve_conflict(
    db: &mut Database,
    doc_id: &str,
    mut rev_id: Option<Cow<[u8]>>,
) -> Result<()> {
    let mut in_conflict = false;
    let mut retry_count = 0_u8;
    const MAX_RETRY_COUNT: u8 = 10;
    loop {
        let doc = match db.do_internal_get_opt(doc_id, true, C4DocContentLevel::kDocGetAll)? {
            Some(x) => x,
            None => {
                info!("doc {} no longer exists, no conflict to resolve", doc_id);
                return Ok(());
            }
        };
        let ok = if let Some(rev_id) = rev_id.as_ref() {
            select_revision(&doc, rev_id)?;
            let mask = kRevLeaf | kRevIsConflict;
            (doc.selected_revision().flags & mask) == mask
        } else {
            let ok = select_next_conflicting_revision(&doc)?;
            rev_id = Some(doc.revision_id().to_vec().into());
            ok
        };
        if !ok {
            info!("conflict in doc {} already resolved, nothing to do", doc_id);
            return Ok(());
        }
        let ok = default_resolve_conflict(db, doc_id, &doc)?;
        if !ok {
            retry_count += 1;
            in_conflict = retry_count < MAX_RETRY_COUNT;
            if in_conflict {
                warn!(
                    "conflict resolution of doc '{}' conflicted with newer saved, retry {}",
                    doc_id, retry_count
                );
            }
        }

        if !in_conflict {
            break;
        }
    }
    Ok(())
}

fn default_resolve_conflict(
    db: &mut Database,
    doc_id: &str,
    conflict: &C4DocumentOwner,
) -> Result<bool> {
    let remote_doc = if (conflict.selected_revision().flags & kRevDeleted) != 0 {
        None
    } else {
        Some(conflict)
    };
    let local_doc = db
        .do_internal_get_opt(doc_id, true, C4DocContentLevel::kDocGetAll)?
        .map(|doc| {
            if (doc.selected_revision().flags & kRevDeleted) != 0 {
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
    match (local_doc, remote_doc) {
        (None, None) | (None, Some(_)) | (Some(_), None) => None,
        (Some(local_doc), Some(remote_doc)) => {
            let remote_gen = remote_doc.generation();
            let local_gen = local_doc.generation();
            if remote_gen > local_gen {
                Some(remote_doc)
            } else if remote_gen < local_gen {
                Some(local_doc)
            } else if unsafe {
                FLSlice_Compare(
                    local_doc.revision_id().into(),
                    remote_doc.revision_id().into(),
                )
            } > 0
            {
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
        if (doc.selected_revision().flags & kRevIsConflict) != 0 {
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
) -> Result<bool> {
    let tx = db.transaction()?;
    // Remote Revision always win so that the resolved revision will not conflict with the remote:
    let winner = unsafe { conflict_doc.0.as_ref() }
        .selectedRev
        .revID
        .as_fl_slice();
    let loser = conflict_doc.revision_id();
    let mut merge_flags: C4RevisionFlags = 0;
    let mut merge_body = FLSlice::default();
    // When useLocal (local wins) or useMerge is true, the new revision will be created
    // under the remote branch which is the winning branch. When useRemote (remote wins)
    // is true, the remote revision will be kept as is and the losing branch will be pruned.
    if resolution != Resolution::UseRemote {
        if let Some(resolved_doc) = resolved_doc {
            let body = resolved_doc.load_body()?;
            merge_body = body.into();
        } else {
            merge_flags = kRevDeleted;
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
    Ok(true)
}

fn select_revision(doc: &C4DocumentOwner, rev_id: &[u8]) -> Result<()> {
    let mut c4err = c4error_init();
    if unsafe { c4doc_selectRevision(doc.0.as_ptr(), rev_id.into(), true, &mut c4err) } {
        Ok(())
    } else {
        Err(c4err.into())
    }
}
