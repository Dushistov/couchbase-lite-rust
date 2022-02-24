use couchbase_lite::{
    fallible_streaming_iterator::FallibleStreamingIterator, ffi::kRevIsConflict, resolve_conflict,
    Database, DatabaseFlags, DocEnumeratorFlags, Document, ReplicatorState,
};
use log::{error, trace};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, env, path::Path, str, sync::mpsc, time::Duration};
use tokio::io::AsyncBufReadExt;

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
struct Message {
    msg: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let runtime = tokio::runtime::Runtime::new()?;
    Database::init_socket_impl(runtime.handle().clone());

    let db_path = env::args().nth(1).expect("No path to db file");
    let db_path = Path::new(&db_path);
    let sync_url = env::args()
        .nth(2)
        .unwrap_or_else(|| "ws://192.168.1.32:4984/demo/".to_string());
    let token: Option<String> = env::args().nth(3);

    let (db_thread, db_exec) = run_db_thread(db_path);
    let db_exec_repl = db_exec.clone();
    let db_exec2 = db_exec.clone();
    db_exec.spawn(move |db| {
        if let Some(db) = db.as_mut() {
            fix_conflicts(db).expect("fix conflict failed");
            db.start_replicator(
                &sync_url,
                token.as_ref().map(String::as_str),
                move |repl_state| {
                    println!("replicator state changed: {:?}", repl_state);
                    match repl_state {
                        ReplicatorState::Stopped(_) | ReplicatorState::Offline => {
                            db_exec_repl.spawn(|db| {
                                if let Some(db) = db.as_mut() {
                                    println!("restarting replicator");
                                    std::thread::sleep(std::time::Duration::from_secs(5));
                                    db.restart_replicator().expect("restart_replicator failed");
                                } else {
                                    eprintln!("db is NOT open");
                                }
                            });
                        }
                        _ => {}
                    }
                },
                move |pushing, doc_it| {
                    for doc in doc_it {
                        if !pushing && (doc.flags & kRevIsConflict) != 0 {
                            let doc_id: &str = doc.docID.as_fl_slice().try_into().unwrap();
                            let doc_id = doc_id.to_string();
                            let rev_id = <&[u8]>::from(doc.revID.as_fl_slice()).to_vec();
                            db_exec2.spawn(move |db| {
                                println!("there is conflict for ({}, {:?}) during replication, trying resolve", doc_id, str::from_utf8(&rev_id));
                                if let Some(db) = db.as_mut() {
                                    resolve_conflict(db, &doc_id, Some(rev_id.into())).expect("resolve conflict failed");
                                }
                            });
                        }
                    }
                },
            )
            .expect("replicator start failed");
        } else {
            eprintln!("db is NOT open");
        }
    });

    let db_exec_repl = db_exec.clone();
    db_exec.spawn(move |db| {
        if let Some(db) = db.as_mut() {
            db.register_observer(move || {
                db_exec_repl
                    .spawn(|db| print_external_changes(db).expect("read external changes failed"));
            })
            .expect("register observer failed");
        } else {
            eprintln!("db is NOT open");
        }
    });

    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());

    let db_exec_repl = db_exec.clone();
    runtime.block_on(async move {
        let mut buf = String::new();
        let mut edit_id = None;
        loop {
            stdin
                .read_line(&mut buf)
                .await
                .expect("reading from stdin fail");
            let msg = &buf;
            let msg = msg.trim_end();
            if !msg.is_empty() {
                if msg.starts_with("!quit") {
                    println!("Time to quit");
                    break;
                } else if let Some(id) = msg.strip_prefix("!edit ") {
                    edit_id = Some(id.to_string());
                    println!("ready to edit message {:?}", edit_id);
                } else if msg.starts_with("!list") {
                    db_exec_repl.spawn(move |db| {
                        if let Some(db) = db.as_mut() {
                            print_all_messages(db).expect("read from db failed");
                        }
                    });
                } else {
                    println!("Your message is '{}'", msg);

                    {
                        let msg = msg.to_string();
                        let edit_id = edit_id.take();
                        db_exec_repl.spawn(move |db| {
                            if let Some(mut db) = db.as_mut() {
                                save_msg(&mut db, &msg, edit_id.as_ref().map(String::as_str))
                                    .expect("save to db failed");
                            } else {
                                eprintln!("db is NOT open");
                            }
                        });
                    }
                }
            }
            buf.clear();
        }
    });
    println!("tokio runtime block_on done");
    db_exec.spawn(|db| {
        if let Some(db) = db.as_mut() {
            db.clear_observers();
            db.stop_replicator();
        } else {
            eprintln!("db is NOT open");
        }
    });
    drop(db_exec);
    db_thread.join().unwrap();
    println!("process exit time I/O");

    runtime.block_on(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    println!("exiting");
    Ok(())
}

fn fix_conflicts(db: &mut Database) -> Result<(), Box<dyn std::error::Error>> {
    let mut conflicts = Vec::with_capacity(100);
    {
        let mut it = db.enumerate_all_docs(DocEnumeratorFlags::empty())?;
        while let Some(item) = it.next()? {
            let doc = item.get_doc()?;
            println!("document with conflict {}", doc.id());
            conflicts.push(doc.id().to_string());
        }
    }
    for doc_id in &conflicts {
        resolve_conflict(db, &doc_id, None)?;
    }
    if !conflicts.is_empty() {
        println!("All conflicts was resolved");
    }
    Ok(())
}

type Job<T> = Box<dyn FnOnce(&mut Option<T>) + Send>;

#[derive(Clone)]
struct DbQueryExecutor {
    inner: mpsc::Sender<Job<Database>>,
}

impl DbQueryExecutor {
    pub fn spawn<F: FnOnce(&mut Option<Database>) + Send + 'static>(&self, job: F) {
        self.inner
            .send(Box::new(job))
            .expect("thread_pool::Executor::spawn failed");
    }
}

fn run_db_thread(db_path: &Path) -> (std::thread::JoinHandle<()>, DbQueryExecutor) {
    let (sender, receiver) = std::sync::mpsc::channel::<Job<Database>>();
    let db_path: std::path::PathBuf = db_path.into();
    let join_handle = std::thread::spawn(move || {
        let mut db = match Database::open_with_flags(&db_path, DatabaseFlags::CREATE) {
            Ok(db) => {
                println!("We read all messages after open:");
                print_all_messages(&db).expect("read from db failed");
                println!("read all messages after open done");
                Some(db)
            }
            Err(err) => {
                error!("Initialiazion cause error: {}", err);
                None
            }
        };
        loop {
            match receiver.recv() {
                Ok(x) => x(&mut db),
                Err(err) => {
                    trace!("db_thread: recv error: {}", err);
                    break;
                }
            }
        }
    });
    (join_handle, DbQueryExecutor { inner: sender })
}

fn save_msg(
    db: &mut Database,
    data: &str,
    doc_id: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut trans = db.transaction()?;
    let msg = Message { msg: data.into() };
    let enc = trans.shared_encoder_session().unwrap();
    let mut doc = if let Some(doc_id) = doc_id {
        println!("save_msg: edit message");
        Document::new_with_id(doc_id, &msg, enc)?
    } else {
        Document::new(&msg, enc)?
    };
    println!("save_msg: doc id {}", doc.id());
    trans.save(&mut doc)?;
    trans.commit()?;
    Ok(())
}

fn print_all_messages(db: &Database) -> Result<(), Box<dyn std::error::Error>> {
    let query = db.query(r#"{"WHAT": ["._id"], "WHERE": ["=", [".type"], "Message"]}"#)?;
    let mut iter = query.run()?;
    while let Some(item) = iter.next()? {
        // work with item
        let id = item.get_raw_checked(0)?;
        let id = id.as_str()?;
        let doc = db.get_existing(id)?;
        let doc_id = doc.id().to_string();
        let seq = doc.sequence().ok_or("No sequence")?;
        let rev = doc.revision_id().ok_or("No revision")?.to_string();
        let flags = doc.flags();
        let db_msg: Message = doc.decode_body()?;
        println!(
            "iter id {} doc id {}, seq {}, rev {}, flags {:?}, msg `{}`",
            id, doc_id, seq, rev, flags, db_msg.msg
        );
    }
    Ok(())
}

fn print_external_changes(db: &mut Option<Database>) -> Result<(), Box<dyn std::error::Error>> {
    let db = db
        .as_mut()
        .ok_or_else(|| format!("print_external_changes: db not OPEN"))?;
    let mut doc_ids = HashSet::<String>::new();
    for change in db.observed_changes() {
        println!(
            "observed change: doc id {} was changed, external {}",
            change.doc_id()?,
            change.external()
        );
        if change.external() {
            doc_ids.insert(change.doc_id()?.into());
        }
    }
    for doc_id in &doc_ids {
        let doc = match db.get_existing(doc_id.as_str()) {
            Ok(x) => x,
            Err(err) => {
                eprintln!("Can not get {}: {}", doc_id, err);
                continue;
            }
        };
        let db_msg: Message = match doc.decode_body() {
            Ok(x) => x,
            Err(err) => {
                eprintln!("Can not decode data: {}", err);
                continue;
            }
        };
        println!("external: {}", db_msg.msg);
    }
    Ok(())
}
