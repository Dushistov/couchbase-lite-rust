use couchbase_lite::{kC4DB_Create, Database, DocEnumeratorFlags, Document, IndexType};
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
        let mut db = Database::open_with_flags(&db_path, kC4DB_Create).unwrap();
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
        let mut db = Database::open_with_flags(&db_path, kC4DB_Create).unwrap();
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
        let mut db = Database::open_with_flags(&db_path, kC4DB_Create).unwrap();
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
        let mut db = Database::open_with_flags(&db_path, kC4DB_Create).unwrap();
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
        let mut db = Database::open_with_flags(&db_path, kC4DB_Create).unwrap();
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
        let mut db = Database::open_with_flags(&db_path, kC4DB_Create).unwrap();
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
        let mut doc = Document::new_with_id_fleece(&doc_id, fleece_data).unwrap();
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
        let mut db = Database::open_with_flags(&db_path, kC4DB_Create).unwrap();

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