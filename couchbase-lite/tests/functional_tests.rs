use couchbase_lite::*;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, str, time::Instant};
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
    let uuids_at_start;
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
        uuids_at_start = db.uuids().unwrap();
        {
            let mut trans = db.transaction().unwrap();
            for i in 17..=180 {
                let foo = Foo {
                    i: i,
                    s: format!("Hello {i}"),
                };
                let mut doc = Document::new(&foo).unwrap();
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
        assert_eq!(
            uuids_at_start,
            db.uuids().unwrap(),
            "uuids not the same after write/read"
        );
    }

    println!("Close and reopen");
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
        assert_eq!(
            uuids_at_start,
            db.uuids().unwrap(),
            "uuids not the same after reopen"
        );
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
                doc.update_body(&foo_updated).unwrap();
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
        assert_eq!(
            uuids_at_start,
            db.uuids().unwrap(),
            "uuids not the same after second reopen"
        );
        assert_eq!(ids_and_data.len() as u64, db.document_count());
        {
            let mut iter = db
                .enumerate_all_docs(DocEnumeratorFlags::default())
                .unwrap();
            ids_and_data.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let mut ids_and_data_iter = ids_and_data.iter();
            while let Some(item) = iter.next().unwrap() {
                let doc_info = item.get_doc_info().unwrap().unwrap();
                let doc = item.get_doc().unwrap();
                let (doc_id, foo) = ids_and_data_iter.next().unwrap();
                assert_eq!(doc_id, doc.id());
                assert_eq!(doc_id, doc_info.doc_id());
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
        let rest = ids_and_data.len() - n;
        {
            println!("count result after delete");
            assert_eq!(rest as u64, db.document_count());
            let query = db.n1ql_query("SELECT COUNT(*) FROM _default").unwrap();
            let mut iter = query.run().unwrap();
            let count: usize = iter.next().unwrap().unwrap().get_checked(0).unwrap();
            assert_eq!(rest, count);
            assert_eq!(
                ids_and_data.len(),
                db.enumerate_all_docs(
                    DocEnumeratorFlags::INCLUDE_DELETED
                        | DocEnumeratorFlags::INCLUDE_NON_CONFLICTED
                )
                .unwrap()
                .count()
                .unwrap()
            );
        }
        {
            let mut trans = db.transaction().unwrap();
            for doc_id in ids_and_data.iter().take(n).map(|x| x.0.as_str()) {
                trans.purge_by_id(doc_id).unwrap();
            }
            trans.commit().unwrap();
        }
        {
            println!("count result after purge");
            assert_eq!(rest as u64, db.document_count());
            let query = db.n1ql_query("SELECT COUNT(*) FROM _default").unwrap();
            let mut iter = query.run().unwrap();
            let count: usize = iter.next().unwrap().unwrap().get_checked(0).unwrap();
            assert_eq!(rest, count);
            assert_eq!(
                rest,
                db.enumerate_all_docs(
                    DocEnumeratorFlags::INCLUDE_DELETED
                        | DocEnumeratorFlags::INCLUDE_NON_CONFLICTED
                )
                .unwrap()
                .count()
                .unwrap()
            );
        }
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
            let mut doc = Document::new(&foo).unwrap();
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
        let mut doc = Document::new(&s).unwrap();
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
        let mut doc = Document::new(&s).unwrap();
        trans.save(&mut doc).unwrap();
        trans.commit().unwrap();
        let doc_id: String = doc.id().into();
        drop(doc);
        assert_eq!(1, db.document_count());

        let doc = db.get_existing(&doc_id).unwrap();
        assert_eq!(s, doc.decode_body::<S>().unwrap());

        let s = create_s(501);
        let mut doc = Document::new_with_id(doc_id.as_str(), &s).unwrap();
        let mut trans = db.transaction().unwrap();
        trans.save(&mut doc).unwrap();
        trans.commit().unwrap();
        drop(doc);
        assert_eq!(1, db.document_count());

        let doc = db.get_existing(&doc_id).unwrap();
        assert_eq!(s, doc.decode_body::<S>().unwrap());

        let s = create_s(400);
        let mut doc = db.get_existing(&doc_id).unwrap();
        doc.update_body(&s).unwrap();
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
fn test_save_unchanged_loaded_document_preserves_body() {
    let tmp_dir = tempdir().unwrap();
    let db_path = tmp_dir.path().join("a.cblite2");
    let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();
    let original = HashMap::from([("value".to_string(), "original".to_string())]);

    {
        let mut transaction = db.transaction().unwrap();
        let mut doc = Document::new_with_id("saved-doc", &original).unwrap();
        transaction.save(&mut doc).unwrap();
        transaction.commit().unwrap();
    }

    let mut doc = db.get_existing("saved-doc").unwrap();
    {
        let mut transaction = db.transaction().unwrap();
        transaction.save(&mut doc).unwrap();
        transaction.commit().unwrap();
    }
    let body: HashMap<String, String> =
        db.get_existing("saved-doc").unwrap().decode_body().unwrap();
    assert_eq!(body, original);

    drop(doc);
    drop(db);
    let db = Database::open_with_flags(&db_path, DatabaseFlags::empty()).unwrap();
    let body: HashMap<String, String> =
        db.get_existing("saved-doc").unwrap().decode_body().unwrap();
    assert_eq!(body, original);
}

#[test]
fn test_save_blob_reference_updates_attachment_flag() {
    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
    struct BlobReference {
        #[serde(rename = "@type")]
        kind: String,
        digest: String,
        length: u64,
    }

    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
    struct Body {
        blobs: Vec<BlobReference>,
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("blobs.cblite2");
    let body = Body {
        blobs: vec![BlobReference {
            kind: "blob".into(),
            digest: "sha1-2jmj7l5rSw0yVb/vlWAYkK/YBwk=".into(),
            length: 0,
        }],
    };

    let mut db = Database::open_with_flags(&path, DatabaseFlags::CREATE).unwrap();
    let mut doc = Document::new_with_id("blob-doc", &body).unwrap();
    {
        let mut transaction = db.transaction().unwrap();
        transaction.save(&mut doc).unwrap();
        transaction.commit().unwrap();
    }
    assert!(doc
        .flags()
        .unwrap()
        .contains(DocumentFlags::HAS_ATTACHMENTS));
    drop(doc);
    drop(db);

    let mut db = Database::open_with_flags(&path, DatabaseFlags::empty()).unwrap();
    db.register_observer(|| {}).unwrap();
    let doc = db.get_existing("blob-doc").unwrap();
    assert!(doc
        .flags()
        .unwrap()
        .contains(DocumentFlags::HAS_ATTACHMENTS));
    assert_eq!(doc.decode_body::<Body>().unwrap(), body);

    let mut doc = db.get_existing("blob-doc").unwrap();
    {
        let mut transaction = db.transaction().unwrap();
        transaction.save(&mut doc).unwrap();
        transaction.commit().unwrap();
    }
    assert!(doc
        .flags()
        .unwrap()
        .contains(DocumentFlags::HAS_ATTACHMENTS));
    assert_ne!(
        db.observed_changes().next().unwrap().revision_flags()
            & ffi::C4RevisionFlags::kRevHasAttachments,
        ffi::C4RevisionFlags(0)
    );

    doc.update_body(&Body { blobs: vec![] }).unwrap();
    {
        let mut transaction = db.transaction().unwrap();
        transaction.save(&mut doc).unwrap();
        transaction.commit().unwrap();
    }
    assert_eq!(
        db.observed_changes().next().unwrap().revision_flags()
            & ffi::C4RevisionFlags::kRevHasAttachments,
        ffi::C4RevisionFlags(0)
    );

    doc.update_body(&body).unwrap();
    {
        let mut transaction = db.transaction().unwrap();
        transaction.save(&mut doc).unwrap();
        transaction.commit().unwrap();
    }
    assert!(doc
        .flags()
        .unwrap()
        .contains(DocumentFlags::HAS_ATTACHMENTS));
    assert_ne!(
        db.observed_changes().next().unwrap().revision_flags()
            & ffi::C4RevisionFlags::kRevHasAttachments,
        ffi::C4RevisionFlags(0)
    );
    drop(doc);
    drop(db);

    let db = Database::open_with_flags(&path, DatabaseFlags::empty()).unwrap();
    let doc = db.get_existing("blob-doc").unwrap();
    assert!(doc
        .flags()
        .unwrap()
        .contains(DocumentFlags::HAS_ATTACHMENTS));
    assert_eq!(doc.decode_body::<Body>().unwrap(), body);
}

#[test]
fn test_pending_document_uses_destination_shared_keys() {
    let dir = tempdir().unwrap();
    let mut destination = Database::open_with_flags(
        &dir.path().join("destination.cblite2"),
        DatabaseFlags::CREATE,
    )
    .unwrap();

    let mut transaction = destination.transaction().unwrap();
    let mut seed = Document::new_with_id("seed", &HashMap::from([("bravo", "seed")])).unwrap();
    transaction.save(&mut seed).unwrap();
    transaction.commit().unwrap();

    let expected = HashMap::from([("alpha".to_owned(), "payload".to_owned())]);
    let mut doc = Document::new_with_id("pending", &expected).unwrap();
    doc.update_body(&expected).unwrap();
    let mut transaction = destination.transaction().unwrap();
    transaction.save(&mut doc).unwrap();
    transaction.commit().unwrap();

    let actual: HashMap<String, String> = destination
        .get_existing("pending")
        .unwrap()
        .decode_body()
        .unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn test_pending_document_preserves_keys_after_encoder_transaction_rollback() {
    let dir = tempdir().unwrap();
    let mut db =
        Database::open_with_flags(&dir.path().join("a.cblite2"), DatabaseFlags::CREATE).unwrap();
    let expected = HashMap::from([("alpha".to_owned(), "payload".to_owned())]);

    let mut pending = {
        let mut transaction = db.transaction().unwrap();
        let _body_with_rolled_back_key = serde_fleece::to_fl_slice_result_with_encoder(
            &expected,
            transaction.shared_encoder_session().unwrap(),
        )
        .unwrap();
        Document::new_with_id("pending", &expected).unwrap()
        // Dropping the transaction also rolls back its shared-key additions.
    };

    {
        let mut transaction = db.transaction().unwrap();
        let mut other =
            Document::new_with_id("other", &HashMap::from([("bravo", "seed")])).unwrap();
        transaction.save(&mut other).unwrap();
        transaction.commit().unwrap();
    }

    let mut transaction = db.transaction().unwrap();
    transaction.save(&mut pending).unwrap();
    transaction.commit().unwrap();

    let actual: HashMap<String, String> =
        db.get_existing("pending").unwrap().decode_body().unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn test_loaded_document_cannot_be_saved_or_deleted_from_another_database() {
    let dir = tempdir().unwrap();
    let mut source =
        Database::open_with_flags(&dir.path().join("source.cblite2"), DatabaseFlags::CREATE)
            .unwrap();
    let mut destination = Database::open_with_flags(
        &dir.path().join("destination.cblite2"),
        DatabaseFlags::CREATE,
    )
    .unwrap();
    let original = HashMap::from([("value".to_owned(), "original".to_owned())]);
    let mut newly_saved = Document::new_with_id("newly-saved", &original).unwrap();

    {
        let mut transaction = source.transaction().unwrap();
        for id in ["to-update", "to-delete"] {
            let mut doc = Document::new_with_id(id, &original).unwrap();
            transaction.save(&mut doc).unwrap();
        }
        transaction.save(&mut newly_saved).unwrap();
        transaction.commit().unwrap();
    }

    let mut to_update = source.get_existing("to-update").unwrap();
    let mut to_delete = source.get_existing("to-delete").unwrap();
    to_update
        .update_body(&HashMap::from([("value", "changed")]))
        .unwrap();

    // The source transaction makes a wrongly directed native update possible
    let source_transaction = source.transaction().unwrap();
    let mut destination_transaction = destination.transaction().unwrap();
    assert!(destination_transaction.save(&mut to_update).is_err());
    assert!(destination_transaction.delete(&mut to_delete).is_err());
    assert!(destination_transaction.save(&mut newly_saved).is_err());
    drop(destination_transaction);
    source_transaction.commit().unwrap();

    for id in ["to-update", "to-delete", "newly-saved"] {
        let doc = source.get_existing(id).unwrap();
        assert!(!doc.flags().unwrap().contains(DocumentFlags::DELETED));
        let body: HashMap<String, String> = doc.decode_body().unwrap();
        assert_eq!(body, original);
    }
    assert_eq!(destination.document_count(), 0);

    drop(source);
    let body: HashMap<String, String> = to_delete.decode_body().unwrap();
    assert_eq!(body, original);

    let mut reopened_source =
        Database::open_with_flags(&dir.path().join("source.cblite2"), DatabaseFlags::empty())
            .unwrap();
    let mut transaction = reopened_source.transaction().unwrap();
    assert!(transaction.save(&mut newly_saved).is_err());
}

#[test]
fn test_failed_save_preserves_document_for_retry() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("failed-save.cblite2");
    let mut db = Database::open_with_flags(&path, DatabaseFlags::CREATE).unwrap();
    let original = HashMap::from([("value", "original")]);

    {
        let mut transaction = db.transaction().unwrap();
        let mut doc = Document::new_with_id("doc", &original).unwrap();
        transaction.save(&mut doc).unwrap();
        transaction.commit().unwrap();
    }

    let mut doc = db.get_existing("doc").unwrap();
    let original_revision = doc.revision_id().unwrap().to_owned();
    let original_sequence = doc.sequence();
    doc.update_body(&42).unwrap(); // corrupt document, body should be dictionary, not number

    let mut transaction = db.transaction().unwrap();
    let err = transaction.save(&mut doc).unwrap_err();
    assert!(matches!(
        err,
        Error::C4Error(c4err)
            if c4err.domain == ffi::C4ErrorDomain::LiteCoreDomain
                && c4err.code == ffi::C4ErrorCode::kC4ErrorCorruptRevisionData.0
    ));
    assert!(doc.exists());
    assert_eq!(doc.revision_id(), Some(original_revision.as_str()));
    assert_eq!(doc.sequence(), original_sequence);

    let updated = HashMap::from([("value".to_owned(), "updated".to_owned())]);
    doc.update_body(&updated).unwrap();
    transaction.save(&mut doc).unwrap();
    transaction.commit().unwrap();

    let actual: HashMap<String, String> = db.get_existing("doc").unwrap().decode_body().unwrap();
    assert_eq!(actual, updated);
}

#[test]
fn test_stale_document_save_retries_with_latest_revision() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("stale-save.cblite2");
    let mut db = Database::open_with_flags(&path, DatabaseFlags::CREATE).unwrap();

    let mut original =
        Document::new_with_id("doc", &HashMap::from([("value", "original")])).unwrap();
    {
        let mut transaction = db.transaction().unwrap();
        transaction.save(&mut original).unwrap();
        transaction.commit().unwrap();
    }

    let mut stale = db.get_existing("doc").unwrap();
    let mut current = db.get_existing("doc").unwrap();
    current
        .update_body(&HashMap::from([("value", "intermediate")]))
        .unwrap();
    {
        let mut transaction = db.transaction().unwrap();
        transaction.save(&mut current).unwrap();
        transaction.commit().unwrap();
    }

    let expected = HashMap::from([("value".to_owned(), "final".to_owned())]);
    stale.update_body(&expected).unwrap();
    let mut transaction = db.transaction().unwrap();
    transaction.save(&mut stale).unwrap();
    transaction.commit().unwrap();

    let actual: HashMap<String, String> = db.get_existing("doc").unwrap().decode_body().unwrap();
    assert_eq!(actual, expected);
    assert_eq!(
        stale.revision_id(),
        db.get_existing("doc").unwrap().revision_id()
    );
}

#[test]
fn test_unicode_collation_uses_icu() {
    let dir = tempdir().unwrap();
    let mut db =
        Database::open_with_flags(&dir.path().join("unicode.cblite2"), DatabaseFlags::CREATE)
            .unwrap();
    {
        let mut transaction = db.transaction().unwrap();
        let mut doc = Document::new_with_id(
            "cafe",
            &Foo {
                i: 0,
                s: "Café".into(),
            },
        )
        .unwrap();
        transaction.save(&mut doc).unwrap();
        transaction.commit().unwrap();
    }

    let query = db
        .query(
            r#"{"WHAT":["._id"],"WHERE":["COLLATE",{"unicode":true,"case":false,"diac":false},["=",[".s"],"cafe"]]}"#,
        )
        .unwrap();
    let mut rows = query.run().unwrap();
    assert_eq!(
        rows.next()
            .unwrap()
            .unwrap()
            .get_raw_checked(0)
            .unwrap()
            .as_str()
            .unwrap(),
        "cafe"
    );
    assert!(rows.next().unwrap().is_none());
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
                assert_eq!(value.type_(), ffi::C4IndexType::kC4ValueIndex);
                assert_eq!(value.expr_as_str().unwrap(), "[[\".s\"]]");
                println!("index name: {name}");
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
                    s: format!("Hello {i}"),
                };
                let mut doc = Document::new(&foo).unwrap();
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
            println!("iteration id {id}");
            let doc = db.get_existing(id).unwrap();
            println!("doc id {}", doc.id());

            let foo: Foo = doc.decode_body().unwrap();
            println!("foo: {foo:?}");
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
                s: format!("Hello {i}"),
            };
            let mut doc = Document::new(&foo).unwrap();
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
                field1: format!("_common_prefix_{i}"),
                field2: format!("{}", i + 1),
            };
            let mut doc = Document::new(&d).unwrap();
            trans.save(&mut doc).unwrap();
        }
        trans.commit().unwrap();

        db.create_index("field1", "[[\".field1\"]]", IndexType::ValueIndex, None)
            .unwrap();
        db.create_index("field2", "[[\".field2\"]]", IndexType::ValueIndex, None)
            .unwrap();

        for i in 0..N {
            let pat = format!("{i}");
            let query = db
                .query(&format!(
                    r#"{{
"WHAT": [["count()"]],
 "WHERE": ["OR", ["LIKE", [".field1"], "%{pat}%"],
                 ["LIKE", [".field2"], "%{pat}%"]]}}"#,
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
                s: format!("Hello {i}"),
            };
            let mut doc = Document::new(&foo).unwrap();
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
                s: format!("Hello {i}"),
            };
            let mut doc = Document::new(&foo).unwrap();
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

#[test]
fn test_all_types_in_query() {
    #[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
    #[serde(tag = "type")]
    struct Boo {
        b: bool,
        int: i32,
        long: i64,
        ulong: u64,
        float: f32,
        double: f64,
        s: String,
        arr: [String; 3],
        m: HashMap<String, i32>,
    }
    let boo = Boo {
        b: true,
        int: i32::MIN,
        long: i64::MIN,
        ulong: u64::MAX,
        float: f32::MAX,
        double: f64::MIN,
        s: "Boo::s".into(),
        arr: ["a".into(), "b".into(), "c".into()],
        m: {
            let mut m = HashMap::new();
            m.insert("a".into(), 17);
            m.insert("b".into(), 18);
            m
        },
    };

    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    let tmp_path = tmp_dir.keep();
    println!("we create tempdir at {tmp_path:?}");
    let db_path = tmp_path.join("a.cblite2");
    {
        let mut db = Database::open_with_flags(&db_path, DatabaseFlags::CREATE).unwrap();

        let mut trans = db.transaction().unwrap();
        let mut doc = Document::new(&boo).unwrap();
        trans.save(&mut doc).unwrap();
        trans.commit().unwrap();

        let query = db
            .n1ql_query(
                "SELECT b,int,long,ulong,float,double,s,arr,m FROM _default WHERE _id=$doc_id",
            )
            .unwrap();
        query
            .set_parameters_fleece(serde_fleece::fleece!({"doc_id": doc.id() }))
            .unwrap();
        let mut iter = query.run().unwrap();
        let item = iter.next().unwrap().unwrap();
        let b = item.get_raw_checked(0).unwrap().as_bool().unwrap();
        assert_eq!(b, boo.b);
        let int = item.get_raw_checked(1).unwrap().as_i32().unwrap();
        assert_eq!(int, boo.int);
        let long = item.get_raw_checked(2).unwrap().as_i64().unwrap();
        assert_eq!(long, boo.long);
        let ulong = item.get_raw_checked(3).unwrap().as_u64().unwrap();
        assert_eq!(ulong, boo.ulong);

        let float = item.get_raw_checked(4).unwrap().as_f32().unwrap();
        assert_eq!(float, boo.float);
        let double = item.get_raw_checked(5).unwrap().as_f64().unwrap();
        assert_eq!(double, boo.double);

        assert_eq!(item.get_raw_checked(6).unwrap().as_str().unwrap(), boo.s);

        let arr_value = item.get_raw_checked(7).unwrap();
        let ValueRef::Array(arr) = arr_value else {
            panic!("Expect array type, got {arr_value:?}");
        };
        assert_eq!(3, arr.len());
        let arr: Vec<String> = (0..arr.len())
            .map(|i| arr.get(i).as_str().unwrap().to_string())
            .collect();
        assert_eq!(arr, boo.arr);

        let m_value = item.get_raw_checked(8).unwrap();
        let ValueRef::Dict(m) = m_value else {
            panic!("Expect dict type, got {m_value:?}");
        };
        assert_eq!(2, m.len());
        assert_eq!(17, m.get("a").as_i32().unwrap());
        assert_eq!(18, m.get("b").as_i32().unwrap());

        drop(iter);
        drop(query);

        println!("Check types, when float/double can be converted to integer");
        let boo = Boo {
            float: 17.0,
            double: 18.0,
            ..boo
        };
        let mut trans = db.transaction().unwrap();
        let mut doc = Document::new(&boo).unwrap();
        trans.save(&mut doc).unwrap();
        trans.commit().unwrap();

        let query = db
            .n1ql_query("SELECT float,double FROM _default WHERE _id=$doc_id")
            .unwrap();
        query
            .set_parameters_fleece(serde_fleece::fleece!({"doc_id": doc.id() }))
            .unwrap();
        let mut iter = query.run().unwrap();
        let item = iter.next().unwrap().unwrap();
        let float = item.get_raw_checked(0).unwrap().as_f32().unwrap();
        assert_eq!(float, boo.float);
        let double = item.get_raw_checked(1).unwrap().as_f64().unwrap();
        assert_eq!(double, boo.double);
    }
    fs::remove_dir_all(tmp_path).expect("Can not remove tmp_dir");
}
