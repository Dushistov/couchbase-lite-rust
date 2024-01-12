//! To run this tests you need couchbase + sync_gateway
//! so ignored by default

use couchbase_lite::*;
use serde::{Deserialize, Serialize};
use std::{path::Path, str};
use tempfile::{tempdir, TempDir};
use tokio::runtime;

// See https://github.com/Dushistov/couchbase-lite-rust/issues/54
#[test]
#[ignore]
fn test_double_replicator_restart() {
    let (url, auth, tmp_dir) = init_env();

    let runtime = runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap();

    let db_path = tmp_dir.path().join("a.cblite2");
    Database::init_socket_impl(runtime.handle().clone());
    let db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();

    let (sync_tx, sync_rx) = std::sync::mpsc::channel::<()>();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    let mut repl = {
        let sync_tx = sync_tx.clone();
        let handle = runtime.handle().clone();
        let params = ReplicatorParameters::default()
            .with_auth(auth.clone())
            .with_validation_func(
                |coll_spec: C4CollectionSpec,
                 doc_id: C4String,
                 rev_id: C4String,
                 rev_flags,
                 _body| {
                    let coll_name: &str =
                        unsafe { str::from_utf8_unchecked(coll_spec.name.into()) };
                    let doc_id: &str = unsafe { str::from_utf8_unchecked(doc_id.into()) };
                    let rev_id: &str = unsafe { str::from_utf8_unchecked(rev_id.into()) };
                    println!("Pull filter: {coll_name}, {doc_id}, {rev_id}, {rev_flags:?}");
                    true
                },
            )
            .with_state_changed_callback(move |repl_state| {
                println!("repl_state changed: {repl_state:?}");
                if let ReplicatorState::Idle = repl_state {
                    sync_tx.send(()).unwrap();
                    let tx = tx.clone();
                    handle.spawn(async move {
                        tx.send(()).await.unwrap();
                    });
                }
            })
            .with_documents_ended_callback(
                move |pushing: bool, doc_iter: &mut dyn Iterator<Item = &C4DocumentEnded>| {
                    let docs: Vec<String> = doc_iter
                        .map(|x| {
                            let doc_id: &str = x.docID.as_fl_slice().try_into().unwrap();
                            doc_id.to_string()
                        })
                        .collect();
                    println!("pushing {pushing}, docs {docs:?}");
                },
            );
        let mut repl = Replicator::new(&db, url, params).unwrap();
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

// See https://github.com/Dushistov/couchbase-lite-rust/issues/94
#[ignore]
#[test]
fn test_wrong_sync_packets_order() {
    let (url, auth, tmp_dir) = init_env();
    let runtime = runtime::Runtime::new().unwrap();
    Database::init_socket_impl(runtime.handle().clone());

    start_repl_and_save_documents(tmp_dir.path(), "a", 10_000, url, auth).unwrap();
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(tag = "type")]
struct MyDocument {
    text: String,
    numbers: Vec<i32>,
}

fn start_repl_and_save_documents(
    dir: &Path,
    name: &str,
    n: usize,
    url: &str,
    auth: ReplicatorAuthentication,
) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = dir.join(format!("{name}.cblite2"));
    let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE)?;
    let (state_tx, mut state_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut repl = {
        let params = ReplicatorParameters::default()
            .with_auth(auth)
            .with_validation_func(
                |coll_spec: C4CollectionSpec,
                 doc_id: C4String,
                 rev_id: C4String,
                 rev_flags,
                 _body| {
                    let coll_name: &str =
                        unsafe { str::from_utf8_unchecked(coll_spec.name.into()) };
                    let doc_id: &str = unsafe { str::from_utf8_unchecked(doc_id.into()) };
                    let rev_id: &str = unsafe { str::from_utf8_unchecked(rev_id.into()) };
                    println!("Pull filter: {coll_name}, {doc_id}, {rev_id}, {rev_flags:?}");
                    true
                },
            )
            .with_state_changed_callback(move |repl_state| {
                println!("repl_state changed: {repl_state:?}");
                if let Err(err) = state_tx.send(repl_state) {
                    eprintln!("state_tx send failed: {err}");
                }
            })
            .with_documents_ended_callback(
                move |pushing: bool, doc_iter: &mut dyn Iterator<Item = &C4DocumentEnded>| {
                    let docs: Vec<String> = doc_iter
                        .map(|x| {
                            let doc_id: &str = x.docID.as_fl_slice().try_into().unwrap();
                            doc_id.to_string()
                        })
                        .collect();
                    println!("pushing {pushing}, docs {docs:?}");
                },
            );
        let mut repl = Replicator::new(&db, url, params)?;
        repl.start(true)?;
        repl
    };

    for i in 0..n {
        let data = MyDocument {
            text: format!("{i} from {name}"),
            numbers: (0..(i as i32)).collect(),
        };
        let mut trans = db.transaction()?;
        let enc = trans.shared_encoder_session()?;
        let mut doc = Document::new_with_id(format!("{i}"), &data, enc)?;
        trans.save(&mut doc)?;
        trans.commit()?;
    }

    println!("saving {n} documents done");
    let mut was_busy = false;
    while let Some(state) = state_rx.blocking_recv() {
        println!("Get state: {state:?}");
        match state {
            ReplicatorState::Stopped(_) => {
                panic!("replication stopped");
            }
            ReplicatorState::Offline => {
                panic!("replication becomes offline");
            }
            ReplicatorState::Connecting => {
                println!("connecting done");
            }
            ReplicatorState::Idle => {
                if was_busy {
                    println!("state changed from busy to idle, exiting");
                    break;
                }
            }
            ReplicatorState::Busy(_) => was_busy = true,
        }
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    repl.stop();
    Ok(())
}

fn init_env() -> (&'static str, ReplicatorAuthentication, TempDir) {
    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {:?}", tmp_dir.path());
    let url = "ws://127.0.0.1:4984/demo/";
    let auth = ReplicatorAuthentication::None;
    (url, auth, tmp_dir)
}
