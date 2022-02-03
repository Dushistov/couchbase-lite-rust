use couchbase_lite::{kC4DB_Create, Database, DocEnumeratorFlags, Document};
use fallible_streaming_iterator::FallibleStreamingIterator;
use serde::{Deserialize, Serialize};
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
