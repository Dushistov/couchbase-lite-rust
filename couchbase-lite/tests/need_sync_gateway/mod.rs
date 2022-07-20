//! To run this tests you need couchbase + sync_gateway
//! so ignored by default

use couchbase_lite::*;
use std::str;
use tempfile::tempdir;

// details https://github.com/Dushistov/couchbase-lite-rust/issues/54
#[test]
#[ignore]
fn test_double_replicator_restart() {
    use tokio::runtime;

    let _ = env_logger::try_init();
    let url = "ws://127.0.0.1:4984/demo/";
    let auth = ReplicatorAuthentication::None;

    let runtime = runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap();

    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {}", tmp_dir.path().display());
    let db_path = tmp_dir.path().join("a.cblite2");
    Database::init_socket_impl(runtime.handle().clone());
    let db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();

    let (sync_tx, sync_rx) = std::sync::mpsc::channel::<()>();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    let mut repl = {
        let sync_tx = sync_tx.clone();
        let handle = runtime.handle().clone();
        let mut repl = Replicator::new(
            &db,
            url,
            &auth,
            |coll_name: C4String, doc_id: C4String, rev_id: C4String, rev_flags, _body| {
                let coll_name: &str = unsafe { str::from_utf8_unchecked(coll_name.into()) };
                let doc_id: &str = unsafe { str::from_utf8_unchecked(doc_id.into()) };
                let rev_id: &str = unsafe { str::from_utf8_unchecked(rev_id.into()) };
                println!("Pull filter: {coll_name}, {doc_id}, {rev_id}, {rev_flags:?}");
                true
            },
            move |repl_state| {
                println!("repl_state changed: {repl_state:?}");
                if let ReplicatorState::Idle = repl_state {
                    sync_tx.send(()).unwrap();
                    let tx = tx.clone();
                    handle.spawn(async move {
                        tx.send(()).await.unwrap();
                    });
                }
            },
            move |pushing: bool, doc_iter: &mut dyn Iterator<Item = &C4DocumentEnded>| {
                let docs: Vec<String> = doc_iter
                    .map(|x| {
                        let doc_id: &str = x.docID.as_fl_slice().try_into().unwrap();
                        doc_id.to_string()
                    })
                    .collect();
                println!("pushing {pushing}, docs {docs:?}");
            },
        )
        .unwrap();
        repl.start(false).unwrap();
        repl
    };

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();

    let thread_join_handle = {
        std::thread::spawn(move || {
            runtime.block_on(async {
                rx.recv().await.unwrap();
                println!("got async event that replicator was idle");
                rx.recv().await.unwrap();
                let _: () = stop_rx.await.unwrap();
                println!("get value from stop_rx, waiting last messages processing");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            });
        })
    };
    sync_rx.recv().unwrap();
    println!("got SYNC event that replicator was idle");
    for _ in 0..10 {
        repl = repl.restart(&db, url, &auth, false).unwrap();
    }
    println!("multi restart done");
    std::thread::sleep(std::time::Duration::from_secs(2));
    repl.stop();
    stop_tx.send(()).unwrap();
    thread_join_handle.join().unwrap();

    println!("tokio done");
}
