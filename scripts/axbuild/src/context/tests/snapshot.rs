use super::{common::*, *};

#[test]
fn snapshot_persistence_can_be_disabled_by_env() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _env = TempEnvVar::set(NO_SNAPSHOT_ENV, "1");

    assert!(!SnapshotPersistence::Store.should_store());
    assert!(!SnapshotPersistence::Discard.should_store());
}
