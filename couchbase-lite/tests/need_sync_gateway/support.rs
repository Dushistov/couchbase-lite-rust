use couchbase_lite::*;
use couchbase_lite_core_sys::{c4_dumpInstances, c4_getObjectCount};
use std::{
    env, thread,
    time::{Duration, Instant},
};
use tempfile::{tempdir, TempDir};

pub(super) fn wait_for_litecore_cleanup(initial_objects: i32) {
    // Stopped is delivered before LiteCore releases its internal references.
    // Keep the socket runtime alive until the native objects are destroyed.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let objects = unsafe { c4_getObjectCount() };
        if objects == initial_objects {
            return;
        }
        if Instant::now() >= deadline {
            unsafe { c4_dumpInstances() };
            panic!(
                "LiteCore cleanup timed out: {objects} live objects, expected {initial_objects}"
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn stop_and_wait(
    repl: &mut Replicator,
    state_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ReplicatorState>,
) {
    // Restart can leave status events from earlier replicators in the queue.
    while state_rx.try_recv().is_ok() {}
    assert!(
        !matches!(repl.state(), ReplicatorState::Stopped(_)),
        "replicator stopped before stop() was called"
    );

    repl.stop();
    loop {
        match state_rx.blocking_recv() {
            Some(ReplicatorState::Stopped(err)) => {
                match err {
                    Error::C4Error(c4err) => assert_eq!(c4err.code, 0, "{c4err:?}"),
                    other => panic!("replication stopped with an error: {other:?}"),
                }
                break;
            }
            Some(state) => println!("Get state after stop: {state:?}"),
            None => panic!("replicator state channel closed before it stopped"),
        }
    }
}

fn get_env_var(name: &str) -> Result<String, String> {
    match env::var(name) {
        Err(err) => Err(format!("Can not get {name}: {err}")),
        Ok(val) if val.trim().is_empty() => Err(format!("{name} is setted but empty")),
        Ok(val) => Ok(val),
    }
}

pub(super) fn init_test_env() -> Option<(String, ReplicatorAuthentication, TempDir)> {
    let _ = env_logger::try_init();
    let tmp_dir = tempdir().expect("Can not create tmp directory");
    println!("we create tempdir at {:?}", tmp_dir.path());
    let Ok(url) = get_env_var("SG_URL") else {
        return None;
    };
    let auth = if env::var("SG_USER").is_ok() {
        let username = get_env_var("SG_USER").unwrap().into();
        let password = get_env_var("SG_PASS").unwrap().into();
        ReplicatorAuthentication::Basic { username, password }
    } else if env::var("SG_TOKEN").is_ok() {
        let token = get_env_var("SG_TOKEN").unwrap().into();
        ReplicatorAuthentication::SessionToken(token)
    } else {
        ReplicatorAuthentication::None
    };
    Some((url, auth, tmp_dir))
}
