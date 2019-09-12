use couchbase_lite::{
    fallible_streaming_iterator::FallibleStreamingIterator, use_c4_civet_web_socket_factory,
    Database, DatabaseConfig, Document,
};
use futures::{
    future::{lazy, Future},
    stream::Stream,
};
use log::{error, trace};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    env,
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
struct Message {
    msg: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let mut runtime = tokio::runtime::Runtime::new()?;
    let task_executor = runtime.executor();

    let db_path = env::args().nth(1).expect("No path to db file");
    let db_path = Path::new(&db_path);
    let sync_url = env::args()
        .nth(2)
        .unwrap_or_else(|| "ws://192.168.1.132:4984/demo/".to_string());
    let token: Option<String> = env::args().nth(3);

    use_c4_civet_web_socket_factory();
    let (db_thread, db_exec) = run_db_thread(db_path);
    db_exec.spawn(move |db| {
        if let Some(db) = db.as_mut() {
            db.start_replicator(&sync_url, token.as_ref().map(String::as_str))
                .expect("replicator start failed");
        } else {
            eprintln!("db is NOT open");
        }
    });

    let db_exec2 = db_exec.clone();
    db_exec.spawn(move |db| {
        if let Some(db) = db.as_mut() {
            db.register_observer(move || {
                db_exec2
                    .spawn(|db| print_external_changes(db).expect("read external changes failed"));
            });
        } else {
            eprintln!("db is NOT open");
        }
    });
    /*
       let db = Arc::new(Mutex::new());


       {
           let db2 = db.clone();
           let mut ldb = db.lock().expect("db lock failed");

           print_all_messages(&ldb)?;

           ldb.register_observer(move || {
               eprintln!("databaseobserver: Something changed in db");
               let db3 = db2.clone();

               task_executor.spawn(lazy(move || {
                   println!("Inside tokio thread");
                   let mut db = db3.lock().expect("db lock failed");




                   Ok(())
               }));
           })?;

           ldb.start_replicator(&sync_url, token.as_ref().map(String::as_str))?;
       }


    */
    let db_exec3 = db_exec.clone();
    let stdin = tokio::io::stdin();
    let framed_read = tokio_codec::FramedRead::new(stdin, tokio::codec::BytesCodec::new())
        .map_err(|e| {
            println!("error = {:?}", e);
        })
        .for_each(move |bytes| {
            if let Ok(msg) = std::str::from_utf8(&bytes) {
                let msg = msg.trim_end();
                if !msg.is_empty() {
                    println!("Your message is '{}'", msg);

                    {
                        let msg = msg.to_string();
                        db_exec.spawn(move |db| {
                            if let Some(mut db) = db.as_mut() {
                                save_msg(&mut db, &msg).expect("save to db failed");
                            } else {
                                eprintln!("db is NOT open");
                            }
                        });
                    }
                }
            } else {
                eprintln!("you enter strange bytes: {:?}", bytes);
            }
            Ok(())
        });

    runtime.spawn(framed_read);
    runtime.shutdown_on_idle().wait().unwrap();
    db_exec3.spawn(|db| {
        if let Some(db) = db.as_mut() {
            db.clear_observers();
        } else {
            eprintln!("db is NOT open");
        }
    });
    drop(db_exec3);
    db_thread.join().unwrap();
    println!("exiting");
    Ok(())
}

type Job<T> = Box<dyn FnOnce(&mut Option<T>) + Send>;

#[derive(Clone)]
struct DbQueryExecutor {
    inner: std::sync::mpsc::Sender<Job<Database>>,
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
        let mut db = match Database::open(&db_path, DatabaseConfig::default()) {
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

fn save_msg(db: &mut Database, data: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut trans = db.transaction()?;
    let msg = Message { msg: data.into() };
    let mut doc = Document::new(&msg)?;
    println!("creat new doc, id {}", doc.id());
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
        println!("iteration id {}", id);
        let doc = db.get_existsing(id)?;
        println!("doc id {}", doc.id());

        let db_msg: Message = doc.decode_data()?;
        println!("db_msg: {:?}", db_msg);
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
            change.doc_id(),
            change.external()
        );
        if change.external() {
            doc_ids.insert(change.doc_id().into());
        }
    }
    for doc_id in &doc_ids {
        let doc = match db.get_existsing(doc_id.as_str()) {
            Ok(x) => x,
            Err(err) => {
                eprintln!("Can not get {}: {}", doc_id, err);
                continue;
            }
        };
        let db_msg: Message = match doc.decode_data() {
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
