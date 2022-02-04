use couchbase_lite::{
    fallible_streaming_iterator::FallibleStreamingIterator, kC4DB_Create, Database, Document,
};
use log::{error, trace};
use serde::{Deserialize, Serialize};
use std::{env, path::Path, sync::mpsc};
use tokio::io::AsyncBufReadExt;

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
struct Message {
    msg: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let runtime = tokio::runtime::Runtime::new()?;

    let db_path = env::args().nth(1).expect("No path to db file");
    let db_path = Path::new(&db_path);
    let sync_url = env::args()
        .nth(2)
        .unwrap_or_else(|| "ws://192.168.1.132:4984/demo/".to_string());
    let token: Option<String> = env::args().nth(3);
    let (db_thread, db_exec) = run_db_thread(db_path);

    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let db_exec_repl = db_exec.clone();
    runtime.block_on(async move {
        static EDIT_PREFIX: &'static str = "edit ";
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
                if msg == "quit" {
                    println!("Time to quit");
                    break;
                } else if msg.starts_with(EDIT_PREFIX) {
                    edit_id = Some((&msg[EDIT_PREFIX.len()..]).to_string());
                    println!("ready to edit message {:?}", edit_id);
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

    db_exec.spawn(|db| {
        if let Some(db) = db.as_mut() {
            db.clear_observers();
        //            db.stop_replicator();
        } else {
            eprintln!("db is NOT open");
        }
    });
    drop(db_exec);
    db_thread.join().unwrap();
    println!("exiting");
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
        let mut db = match Database::open_with_flags(&db_path, kC4DB_Create) {
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
    let enc = trans.shared_encoder_session()?;
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
        println!("iteration id {}", id);
        let doc = db.get_existing(id)?;
        println!("doc id {}", doc.id());

        let db_msg: Message = doc.decode_body()?;
        println!("db_msg: {:?}", db_msg);
    }
    Ok(())
}
