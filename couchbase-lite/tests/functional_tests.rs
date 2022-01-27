use couchbase_lite::{kC4DB_Create, Database};
use tempfile::tempdir;

#[test]
fn test_write_read() {
    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {}", tmp_dir.path().display());
    let db_path = tmp_dir.path().join("a.cblite2");
    {
        let mut db = Database::open_with_flags(&db_path, kC4DB_Create).unwrap();
    }

    tmp_dir.close().expect("Can not close tmp_dir");
}
