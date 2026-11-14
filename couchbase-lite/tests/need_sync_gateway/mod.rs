//! Sync Gateway restart regression test (should run in separate process)

mod support;

use couchbase_lite::*;
use couchbase_lite_core_sys::c4_getObjectCount;
use std::{num::NonZero, str, thread, time::Duration};
use support::{init_test_env, stop_and_wait, wait_for_litecore_cleanup};
use tokio::runtime;

/// See https://github.com/Dushistov/couchbase-lite-rust/issues/54
#[ignore]
#[test]
fn test_double_replicator_restart() {
    let Some((url, auth, tmp_dir)) = init_test_env() else {
        println!("SKIPING test_double_replicator_restart, env variables not setted");
        return;
    };

    let initial_objects = unsafe { c4_getObjectCount() };
    let runtime = runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap();

    let db_path = tmp_dir.path().join("a.cblite2");
    Database::init_socket_impl(runtime.handle().clone());
    let db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();

    let (sync_tx, sync_rx) = std::sync::mpsc::channel::<()>();
    let (state_tx, mut state_rx) = tokio::sync::mpsc::unbounded_channel();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    let mut repl = {
        let sync_tx = sync_tx.clone();
        let handle = runtime.handle().clone();
        let params = ReplicatorParameters::default()
            .with_auth(auth.clone())
            .with_heartbeat_interval(NonZero::new(1).unwrap())
            .with_validation_func(
                |coll_spec: C4CollectionSpec,
                 doc_id: C4String,
                 rev_id: C4String,
                 rev_flags,
                 _body| {
                    let coll_name: &str =
                        unsafe { str::from_utf8_unchecked(coll_spec.name.as_bytes_unchecked()) };
                    let doc_id: &str =
                        unsafe { str::from_utf8_unchecked(doc_id.as_bytes_unchecked()) };
                    let rev_id: &str =
                        unsafe { str::from_utf8_unchecked(rev_id.as_bytes_unchecked()) };
                    println!("Pull filter: {coll_name}, {doc_id}, {rev_id}, {rev_flags:?}");
                    true
                },
            )
            .with_state_changed_callback(move |repl_state| {
                println!("repl_state changed: {repl_state:?}");
                if matches!(&repl_state, ReplicatorState::Idle) {
                    sync_tx.send(()).unwrap();
                    let tx = tx.clone();
                    handle.spawn(async move {
                        tx.send(()).await.unwrap();
                    });
                }
                state_tx.send(repl_state).unwrap();
            })
            .with_documents_ended_callback(
                move |pushing: bool, doc_iter: &mut dyn Iterator<Item = &C4DocumentEnded>| {
                    let docs: Vec<String> = doc_iter
                        .map(|x| {
                            let doc_id: &str =
                                unsafe { x.docID.as_fl_slice().as_str_unchecked() }.unwrap();
                            doc_id.to_string()
                        })
                        .collect();
                    println!("pushing {pushing}, docs {docs:?}");
                },
            );
        let mut repl = Replicator::new(&db, &url, params).unwrap();
        repl.start(false).unwrap();
        repl
    };

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();

    let thread_join_handle = {
        thread::spawn(move || {
            runtime.block_on(async {
                rx.recv().await.unwrap();
                println!("got async event that replicator was idle");
                rx.recv().await.unwrap();
                let _: () = stop_rx.await.unwrap();
            });
        })
    };
    sync_rx.recv().unwrap();
    println!("got SYNC event that replicator was idle");
    for _ in 0..10 {
        repl = repl.restart(&db, &url, &auth, false).unwrap();
    }
    println!("multi restart done");
    thread::sleep(Duration::from_secs(3));
    stop_and_wait(&mut repl, &mut state_rx);
    drop(repl);
    drop(db);
    wait_for_litecore_cleanup(initial_objects);
    stop_tx.send(()).unwrap();
    thread_join_handle.join().unwrap();
    println!("tokio done");
}
