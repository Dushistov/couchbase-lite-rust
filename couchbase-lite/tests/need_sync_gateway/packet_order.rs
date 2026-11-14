//! Sync Gateway packet ordering regression test

mod support;

use couchbase_lite::*;
use couchbase_lite_core_sys::c4_getObjectCount;
use serde::{Deserialize, Serialize};
use std::{env, path::Path, str};
use support::{init_test_env, stop_and_wait, wait_for_litecore_cleanup};
use tokio::runtime;

/// See https://github.com/Dushistov/couchbase-lite-rust/issues/94
#[ignore]
#[test]
fn test_wrong_sync_packets_order() {
    let Some((url, auth, tmp_dir)) = init_test_env() else {
        println!("SKIPING test_wrong_sync_packets_order, env variables not setted");
        return;
    };

    let initial_objects = unsafe { c4_getObjectCount() };
    let runtime = runtime::Runtime::new().unwrap();
    Database::init_socket_impl(runtime.handle().clone());

    let document_count = env::var("SG_TEST_DOCUMENT_COUNT")
        .map(|count| {
            count
                .parse()
                .expect("SG_TEST_DOCUMENT_COUNT must be a number")
        })
        .unwrap_or(10_000);
    start_repl_and_save_documents(tmp_dir.path(), "a", document_count, &url, auth).unwrap();
    wait_for_litecore_cleanup(initial_objects);
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
                if let Err(err) = state_tx.send(repl_state) {
                    eprintln!("state_tx send failed: {err}");
                }
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
    stop_and_wait(&mut repl, &mut state_rx);
    Ok(())
}
