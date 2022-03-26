use couchbase_lite::*;
use fallible_streaming_iterator::FallibleStreamingIterator;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tempfile::tempdir;

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(tag = "type")]
struct Foo {
    i: i32,
    s: String,
}

#[derive(Deserialize, Debug)]
struct Empty {}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
struct S {
    f: f64,
    s: String,
}

impl PartialEq for S {
    fn eq(&self, o: &S) -> bool {
        (self.f - o.f).abs() < 1e-13 && self.s == o.s
    }
}

#[test]
fn test_write_read() {
    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {}", tmp_dir.path().display());
    let db_path = tmp_dir.path().join("a.cblite2");
    let mut ids_and_data = Vec::<(String, Foo)>::new();
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
        {
            let mut trans = db.transaction().unwrap();
            for i in 17..=180 {
                let foo = Foo {
                    i: i,
                    s: format!("Hello {}", i),
                };
                let enc = trans.shared_encoder_session().unwrap();
                let mut doc = Document::new(&foo, enc).unwrap();
                trans.save(&mut doc).unwrap();
                ids_and_data.push((doc.id().into(), foo));
            }
            trans.commit().unwrap();
        }
        assert_eq!(ids_and_data.len() as u64, db.document_count());
        for (doc_id, foo) in &ids_and_data {
            let doc = db.get_existing(doc_id).unwrap();
            let loaded_foo: Foo = doc.decode_body().unwrap();
            assert_eq!(*foo, loaded_foo);
        }
    }

    println!("Close and reopen");
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
        assert_eq!(ids_and_data.len() as u64, db.document_count());
        for (doc_id, foo) in &ids_and_data {
            let doc = db.get_existing(doc_id).unwrap();
            let loaded_foo: Foo = doc.decode_body().unwrap();
            assert_eq!(*foo, loaded_foo);
        }

        {
            let mut trans = db.transaction().unwrap();
            for (doc_id, foo) in &ids_and_data {
                let mut doc = trans.get_existing(doc_id).unwrap();
                let mut foo_updated = foo.clone();
                foo_updated.i += 1;
                let enc = trans.shared_encoder_session().unwrap();
                doc.update_body(&foo_updated, enc).unwrap();
                trans.save(&mut doc).unwrap();
            }
            trans.commit().unwrap();
        }
        assert_eq!(ids_and_data.len() as u64, db.document_count());
        for (doc_id, foo) in &ids_and_data {
            let doc = db.get_existing(doc_id).unwrap();
            let loaded_foo: Foo = doc.decode_body().unwrap();
            assert_eq!(
                Foo {
                    i: foo.i + 1,
                    s: foo.s.clone()
                },
                loaded_foo
            );
        }
    }

    println!("Close and reopen, enumerate");
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
        assert_eq!(ids_and_data.len() as u64, db.document_count());
        {
            let mut iter = db
                .enumerate_all_docs(DocEnumeratorFlags::default())
                .unwrap();
            ids_and_data.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let mut ids_and_data_iter = ids_and_data.iter();
            while let Some(item) = iter.next().unwrap() {
                let doc = item.get_doc().unwrap();
                let (doc_id, foo) = ids_and_data_iter.next().unwrap();
                assert_eq!(doc_id, doc.id());
                let loaded_foo: Foo = doc.decode_body().unwrap();
                assert_eq!(
                    Foo {
                        i: foo.i + 1,
                        s: foo.s.clone()
                    },
                    loaded_foo
                );
            }
        }

        let n = ids_and_data.len() / 2;

        {
            let mut trans = db.transaction().unwrap();
            for doc_id in ids_and_data.iter().take(n).map(|x| x.0.as_str()) {
                let mut doc = trans.get_existing(doc_id).unwrap();
                trans.delete(&mut doc).unwrap();
            }
            trans.commit().unwrap();
        }
        assert_eq!((ids_and_data.len() - n) as u64, db.document_count());
    }

    tmp_dir.close().expect("Can not close tmp_dir");
}

#[test]
fn test_observed_changes() {
    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {}", tmp_dir.path().display());
    let db_path = tmp_dir.path().join("a.cblite2");
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
        db.register_observer(|| println!("something changed"))
            .unwrap();
        let changes: Vec<_> = db.observed_changes().collect();
        assert!(changes.is_empty());
        let doc_id: String = {
            let mut trans = db.transaction().unwrap();
            let foo = Foo {
                i: 17,
                s: "hello".into(),
            };
            let enc = trans.shared_encoder_session().unwrap();
            let mut doc = Document::new(&foo, enc).unwrap();
            trans.save(&mut doc).unwrap();
            trans.commit().unwrap();
            doc.id().into()
        };
        let changes: Vec<_> = db.observed_changes().collect();
        println!("changes: {:?}", changes);
        assert_eq!(1, changes.len());
        assert_eq!(doc_id, changes[0].doc_id().unwrap());
        assert!(!changes[0].revision_id().unwrap().is_empty());
        assert!(!changes[0].external());
        assert!(changes[0].body_size() > 2);

        let changes: Vec<_> = db.observed_changes().collect();
        assert!(changes.is_empty());

        {
            let mut trans = db.transaction().unwrap();
            let mut doc = trans.get_existing(&doc_id).unwrap();
            trans.delete(&mut doc).unwrap();
            trans.commit().unwrap();
        }
        let changes: Vec<_> = db.observed_changes().collect();
        println!("changes: {:?}", changes);
        assert_eq!(1, changes.len());
        assert_eq!(doc_id, changes[0].doc_id().unwrap());
        assert!(!changes[0].revision_id().unwrap().is_empty());
        assert!(!changes[0].external());
        assert_eq!(2, changes[0].body_size());

        let doc = db.get_existing(&doc_id).unwrap();
        println!("doc {:?}", doc);
        doc.decode_body::<Empty>().unwrap();
    }
    tmp_dir.close().expect("Can not close tmp_dir");
}

#[test]
fn test_save_float() {
    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {}", tmp_dir.path().display());
    let db_path = tmp_dir.path().join("a.cblite2");
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
        let mut trans = db.transaction().unwrap();
        let s = S {
            f: 17.48,
            s: "ABCD".into(),
        };
        let enc = trans.shared_encoder_session().unwrap();
        let mut doc = Document::new(&s, enc).unwrap();
        trans.save(&mut doc).unwrap();
        trans.commit().unwrap();
        let doc_id: String = doc.id().into();
        drop(doc);

        let doc = db.get_existing(&doc_id).unwrap();
        let loaded_s: S = doc.decode_body().unwrap();
        assert_eq!(s, loaded_s);
    }
    tmp_dir.close().expect("Can not close tmp_dir");
}

#[test]
fn test_save_several_times() {
    fn create_s(i: i32) -> S {
        S {
            f: f64::from(i) / 3.6,
            s: format!("Hello {}", i),
        }
    }
    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {}", tmp_dir.path().display());
    let db_path = tmp_dir.path().join("a.cblite2");
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
        let s = create_s(500);
        let mut trans = db.transaction().unwrap();
        let mut doc = Document::new(&s, trans.shared_encoder_session().unwrap()).unwrap();
        trans.save(&mut doc).unwrap();
        trans.commit().unwrap();
        let doc_id: String = doc.id().into();
        drop(doc);
        assert_eq!(1, db.document_count());

        let doc = db.get_existing(&doc_id).unwrap();
        assert_eq!(s, doc.decode_body::<S>().unwrap());

        let s = create_s(501);
        let mut doc =
            Document::new_with_id(doc_id.as_str(), &s, db.shared_encoder_session().unwrap())
                .unwrap();
        let mut trans = db.transaction().unwrap();
        trans.save(&mut doc).unwrap();
        trans.commit().unwrap();
        drop(doc);
        assert_eq!(1, db.document_count());

        let doc = db.get_existing(&doc_id).unwrap();
        assert_eq!(s, doc.decode_body::<S>().unwrap());

        let s = create_s(400);
        let fleece_data =
            serde_fleece::to_fl_slice_result_with_encoder(&s, db.shared_encoder_session().unwrap())
                .unwrap();
        let mut doc = Document::new_with_id_fleece(&doc_id, fleece_data);
        let mut trans = db.transaction().unwrap();
        trans.save(&mut doc).unwrap();
        trans.commit().unwrap();
        drop(doc);
        assert_eq!(1, db.document_count());

        let doc = db.get_existing(&doc_id).unwrap();
        assert_eq!(s, doc.decode_body::<S>().unwrap());
    }
    tmp_dir.close().expect("Can not close tmp_dir");
}

#[test]
fn test_indices() {
    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {}", tmp_dir.path().display());
    let db_path = tmp_dir.path().join("a.cblite2");
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();

        fn get_index_list(db: &Database) -> Vec<String> {
            let mut ret = vec![];
            let mut index_name_it = db.get_indexes().unwrap();
            while let Some(value) = index_name_it.next().unwrap() {
                let name = value.name_as_str().unwrap();
                println!("index name: {}", name);
                ret.push(name.into());
            }
            ret
        }

        println!("before index creation:");
        assert!(get_index_list(&db).is_empty());

        db.create_index("Foo_s", "[[\".s\"]]", IndexType::ValueIndex, None)
            .unwrap();
        println!("after index creation:");
        assert_eq!(vec!["Foo_s".to_string()], get_index_list(&db));

        {
            let mut trans = db.transaction().unwrap();
            for i in 0..10_000 {
                let foo = Foo {
                    i: i,
                    s: format!("Hello {}", i),
                };
                let enc = trans.shared_encoder_session().unwrap();
                let mut doc = Document::new(&foo, enc).unwrap();
                trans.save(&mut doc).unwrap();
            }
            trans.commit().unwrap();
        }

        let work_time = Instant::now();
        let query = db
            .query(
                r#"
{
 "WHAT": ["._id"],
 "WHERE": ["AND", ["=", [".type"], "Foo"], ["=", [".s"], "Hello 500"]]
}
"#,
            )
            .unwrap();
        let mut iter = query.run().unwrap();
        while let Some(item) = iter.next().unwrap() {
            // work with item
            let id = item.get_raw_checked(0).unwrap();
            let id = id.as_str().unwrap();
            println!("iteration id {}", id);
            let doc = db.get_existing(id).unwrap();
            println!("doc id {}", doc.id());

            let foo: Foo = doc.decode_body().unwrap();
            println!("foo: {:?}", foo);
            assert_eq!(500, foo.i);
        }
        println!("work time: {:?}", work_time.elapsed());
    }
    tmp_dir.close().expect("Can not close tmp_dir");
}

#[test]
fn test_like_offset_limit() {
    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {}", tmp_dir.path().display());
    let db_path = tmp_dir.path().join("a.cblite2");
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
        let mut trans = db.transaction().unwrap();
        for i in 0..10_000 {
            let foo = Foo {
                i: i,
                s: format!("Hello {}", i),
            };
            let enc = trans.shared_encoder_session().unwrap();
            let mut doc = Document::new(&foo, enc).unwrap();
            trans.save(&mut doc).unwrap();
        }
        trans.commit().unwrap();

        assert_eq!(
            vec![
                "Hello 1555",
                "Hello 2555",
                "Hello 3555",
                "Hello 4555",
                "Hello 555",
                "Hello 5555",
                "Hello 6555",
                "Hello 7555",
                "Hello 8555",
                "Hello 9555",
            ],
            query_data(
                &db,
                r#"
{
 "WHAT": [".s"],
 "WHERE": ["LIKE", [".s"], "%555"]
}
"#,
            )
            .unwrap()
        );

        assert_eq!(
            vec!["Hello 0", "Hello 1"],
            query_data(
                &db,
                r#"
{
 "WHAT": [".s"],
 "LIMIT": 2, "OFFSET": 0
}
"#,
            )
            .unwrap()
        );

        assert_eq!(
            vec!["Hello 1", "Hello 2"],
            query_data(
                &db,
                r#"
{
 "WHAT": [".s"],
 "LIMIT": 2, "OFFSET": 1
}
"#,
            )
            .unwrap()
        );

        assert_eq!(
            vec!["Hello 2555", "Hello 3555",],
            query_data(
                &db,
                r#"
{
 "WHAT": [".s"],
 "WHERE": ["LIKE", [".s"], "%555"],
 "ORDER_BY": [".s"],
 "LIMIT": 2, "OFFSET": 1
}
"#,
            )
            .unwrap()
        );
    }
    tmp_dir.close().expect("Can not close tmp_dir");

    fn query_data(db: &Database, query: &str) -> Result<Vec<String>, couchbase_lite::Error> {
        let query = db.query(query)?;
        let mut iter = query.run()?;
        let mut query_ret = Vec::with_capacity(10);
        while let Some(item) = iter.next()? {
            let val = item.get_raw_checked(0)?;
            let val = val.as_str()?;
            query_ret.push(val.to_string());
        }
        query_ret.sort();
        Ok(query_ret)
    }
}

#[test]
fn test_like_performance() {
    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {}", tmp_dir.path().display());
    let db_path = tmp_dir.path().join("a.cblite2");
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
        #[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
        #[serde(tag = "type")]
        struct Data {
            field1: String,
            field2: String,
        }

        const N: usize = 3_000;
        let mut trans = db.transaction().unwrap();
        for i in 0..N {
            let d = Data {
                field1: format!("_common_prefix_{}", i),
                field2: format!("{}", i + 1),
            };
            let enc = trans.shared_encoder_session().unwrap();
            let mut doc = Document::new(&d, enc).unwrap();
            trans.save(&mut doc).unwrap();
        }
        trans.commit().unwrap();

        db.create_index("field1", "[[\".field1\"]]", IndexType::ValueIndex, None)
            .unwrap();
        db.create_index("field2", "[[\".field2\"]]", IndexType::ValueIndex, None)
            .unwrap();

        for i in 0..N {
            let pat = format!("{}", i);
            let query = db
                .query(&format!(
                    r#"{{
"WHAT": [["count()"]],
 "WHERE": ["OR", ["LIKE", [".field1"], "%{pat}%"],
                 ["LIKE", [".field2"], "%{pat}%"]]}}"#,
                    pat = pat,
                ))
                .unwrap();
            let mut iter = query.run().unwrap();
            let mut query_ret = Vec::with_capacity(10);
            while let Some(item) = iter.next().unwrap() {
                let val = item.get_raw_checked(0).unwrap();
                let val = val.as_u64().unwrap();
                query_ret.push(val);
            }
            assert_eq!(1, query_ret.len());
            assert!(query_ret[0] > 1);
        }
    }
    tmp_dir.close().expect("Can not close tmp_dir");
}

#[test]
fn test_n1ql_query() {
    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {}", tmp_dir.path().display());
    let db_path = tmp_dir.path().join("a.cblite2");
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
        let mut trans = db.transaction().unwrap();
        for i in 0..10_000 {
            let foo = Foo {
                i,
                s: format!("Hello {}", i),
            };
            let enc = trans.shared_encoder_session().unwrap();
            let mut doc = Document::new(&foo, enc).unwrap();
            trans.save(&mut doc).unwrap();
        }
        trans.commit().unwrap();

        let expected = vec![
            "Hello 1555",
            "Hello 2555",
            "Hello 3555",
            "Hello 4555",
            "Hello 555",
            "Hello 5555",
            "Hello 6555",
            "Hello 7555",
            "Hello 8555",
            "Hello 9555",
        ];

        {
            let query = db
                .n1ql_query("SELECT s FROM a WHERE s LIKE '%555'")
                .unwrap();

            let mut iter = query.run().unwrap();
            let mut query_ret = Vec::with_capacity(10);
            while let Some(item) = iter.next().unwrap() {
                let val = item.get_raw_checked(0).unwrap();
                let val = val.as_str().unwrap();
                query_ret.push(val.to_string());
            }
            query_ret.sort();

            assert_eq!(expected, query_ret);
        }

        {
            let query = db
                .n1ql_query("SELECT s FROM a WHERE s LIKE '%555'")
                .unwrap();

            let mut iter = query.run().unwrap();
            let mut query_ret = Vec::with_capacity(10);
            while let Some(item) = iter.next().unwrap() {
                let val: &str = item.get_checked_serde(0).unwrap();
                query_ret.push(val.to_string());
            }
            query_ret.sort();

            assert_eq!(expected, query_ret);
        }
    }
    tmp_dir.close().expect("Can not close tmp_dir");
}

#[test]
fn test_n1ql_query_with_parameter() {
    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {}", tmp_dir.path().display());
    let db_path = tmp_dir.path().join("a.cblite2");
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
        let mut trans = db.transaction().unwrap();
        for i in 0..10_000 {
            let foo = Foo {
                i,
                s: format!("Hello {}", i),
            };
            let enc = trans.shared_encoder_session().unwrap();
            let mut doc = Document::new(&foo, enc).unwrap();
            trans.save(&mut doc).unwrap();
        }
        trans.commit().unwrap();

        let query = db
            .n1ql_query("SELECT s FROM a WHERE s LIKE $pattern ORDER BY s LIMIT 2 OFFSET 1")
            .unwrap();
        query
            .set_parameters_fleece(serde_fleece::fleece!({
                "pattern": "%555"
            }))
            .unwrap();
        let expected = vec!["Hello 2555", "Hello 3555"];

        let mut iter = query.run().unwrap();
        let mut query_ret = Vec::with_capacity(10);
        while let Some(item) = iter.next().unwrap() {
            let val = item.get_raw_checked(0).unwrap();
            let val = val.as_str().unwrap();
            query_ret.push(val.to_string());
        }
        query_ret.sort();

        assert_eq!(expected, query_ret);
    }
    tmp_dir.close().expect("Can not close tmp_dir");
}

// to reproduce bug you need server, so ignored by default
// details https://github.com/Dushistov/couchbase-lite-rust/issues/54
#[test]
#[ignore]
#[cfg(feature = "use-tokio-websocket")]
fn test_double_replicator_restart() {
    use tokio::runtime;

    let _ = env_logger::try_init();

    let runtime = runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap();

    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {}", tmp_dir.path().display());
    let db_path = tmp_dir.path().join("a.cblite2");
    Database::init_socket_impl(runtime.handle().clone());
    let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();

    let (sync_tx, sync_rx) = std::sync::mpsc::channel::<()>();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    {
        let sync_tx = sync_tx.clone();
        let handle = runtime.handle().clone();
        db.start_replicator(
            "ws://127.0.0.1:4984/demo/",
            ReplicatorAuthentication::None,
            move |repl_state| {
                println!("repl_state changed: {:?}", repl_state);
                if let ReplicatorState::Idle = repl_state {
                    sync_tx.send(()).unwrap();
                    let tx = tx.clone();
                    handle.spawn(async move {
                        tx.send(()).await.unwrap();
                    });
                }
            },
            move |pushing, doc_iter| {
                let docs: Vec<String> = doc_iter
                    .map(|x| {
                        let doc_id: &str = x.docID.as_fl_slice().try_into().unwrap();
                        doc_id.to_string()
                    })
                    .collect();
                println!("pushing {}, docs {:?}", pushing, docs);
            },
        )
        .unwrap();
    }

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
        db.restart_replicator().unwrap();
    }
    println!("multi restart done");
    std::thread::sleep(std::time::Duration::from_secs(2));
    db.stop_replicator();
    stop_tx.send(()).unwrap();
    thread_join_handle.join().unwrap();

    println!("tokio done");
}
