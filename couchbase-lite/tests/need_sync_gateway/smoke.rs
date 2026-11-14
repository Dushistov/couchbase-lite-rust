//! Basic push and pull replication against Sync Gateway

#[allow(dead_code)] // This shared module also contains helpers for other test binaries.
mod support;

use couchbase_lite::{ffi::C4ReplicatorMode, *};
use couchbase_lite_core_sys::c4_getObjectCount;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    sync::mpsc,
    time::{Duration, Instant},
};
use support::{init_test_env, wait_for_litecore_cleanup};
use tokio::runtime;
use uuid::Uuid;

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SmokeDocument {
    title: String,
    numbers: Vec<i32>,
}

#[ignore]
#[test]
fn test_replication_round_trip() {
    let Some((url, auth, tmp_dir)) = init_test_env() else {
        println!("SKIPPING test_replication_round_trip, Sync Gateway is not configured");
        return;
    };

    let initial_objects = unsafe { c4_getObjectCount() };
    let runtime = runtime::Runtime::new().unwrap();
    Database::init_socket_impl(runtime.handle().clone());

    let db_path = tmp_dir.path().join("smoke.cblite2");
    let docs = [
        SmokeDocument {
            title: "first".into(),
            numbers: vec![1, 2, 3],
        },
        SmokeDocument {
            title: "second".into(),
            numbers: vec![],
        },
        SmokeDocument {
            title: "third".into(),
            numbers: vec![-1, 0, 42],
        },
    ];
    let prefix = Uuid::new_v4();
    let ids: Vec<_> = (0..docs.len())
        .map(|index| format!("smoke-{prefix}-{index}"))
        .collect();

    let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
    {
        let mut transaction = db.transaction().unwrap();
        for (id, body) in ids.iter().zip(&docs) {
            let encoder = transaction.shared_encoder_session().unwrap();
            let mut doc = Document::new_with_id(id.as_str(), body, encoder).unwrap();
            transaction.save(&mut doc).unwrap();
        }
        transaction.commit().unwrap();
    }
    assert_eq!(db.document_count(), docs.len() as u64);
    replicate_once(
        &db,
        &url,
        auth.clone(),
        C4ReplicatorMode::kC4OneShot,
        C4ReplicatorMode::kC4Disabled,
    );
    drop(db);
    wait_for_litecore_cleanup(initial_objects);

    fs::remove_dir_all(&db_path).unwrap();
    let db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
    assert_eq!(db.document_count(), 0);
    replicate_once(
        &db,
        &url,
        auth,
        C4ReplicatorMode::kC4Disabled,
        C4ReplicatorMode::kC4OneShot,
    );

    for (id, expected) in ids.iter().zip(&docs) {
        let actual: SmokeDocument = db.get_existing(id).unwrap().decode_body().unwrap();
        assert_eq!(&actual, expected, "document {id}");
    }
    drop(db);
    wait_for_litecore_cleanup(initial_objects);
}

fn replicate_once(
    db: &Database,
    url: &str,
    auth: ReplicatorAuthentication,
    push: C4ReplicatorMode,
    pull: C4ReplicatorMode,
) {
    let (state_tx, state_rx) = mpsc::channel();
    let params = ReplicatorParameters::default()
        .with_auth(auth)
        .with_push_mode(push)
        .with_pull_mode(pull)
        .with_validation_func(|_: ffi::C4CollectionSpec, _, _, _, _| true)
        .with_state_changed_callback(move |state| {
            let _ = state_tx.send(state);
        });
    let mut replicator = Replicator::new(db, url, params).unwrap();
    replicator.start(false).unwrap();

    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match state_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("replication did not stop")
        {
            ReplicatorState::Stopped(Error::C4Error(error)) if error.code == 0 => break,
            ReplicatorState::Stopped(error) => panic!("replication failed: {error:?}"),
            _ => {}
        }
    }
}
