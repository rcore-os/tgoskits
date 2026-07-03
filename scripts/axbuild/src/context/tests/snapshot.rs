use super::{common::*, *};

#[test]
fn snapshot_load_returns_default_when_missing() {
    let root = tempdir().unwrap();
    let snapshot = ArceosCommandSnapshot::load(root.path()).unwrap();
    assert_eq!(snapshot, ArceosCommandSnapshot::default());
}

#[test]
fn snapshot_persistence_can_be_disabled_by_env() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _env = TempEnvVar::set(NO_SNAPSHOT_ENV, "1");

    assert!(!SnapshotPersistence::Store.should_store());
    assert!(!SnapshotPersistence::Discard.should_store());
}

#[test]
fn snapshot_persistence_treats_zero_env_as_enabled() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _env = TempEnvVar::set(NO_SNAPSHOT_ENV, "0");

    assert!(SnapshotPersistence::Store.should_store());
    assert!(!SnapshotPersistence::Discard.should_store());
}

#[test]
fn axvisor_snapshot_load_returns_default_when_missing() {
    let root = tempdir().unwrap();
    let snapshot = AxvisorCommandSnapshot::load(root.path()).unwrap();
    assert_eq!(snapshot, AxvisorCommandSnapshot::default());
}

#[test]
fn snapshot_store_round_trips() {
    let root = tempdir().unwrap();
    let snapshot = ArceosCommandSnapshot {
        package: Some("arceos-helloworld".into()),
        arch: Some("aarch64".into()),
        target: Some("target".into()),
        smp: None,
        config: Some(PathBuf::from("configs/build.toml")),
        qemu: ArceosQemuSnapshot {
            qemu_config: Some(PathBuf::from("configs/qemu.toml")),
        },
        uboot: ArceosUbootSnapshot {
            uboot_config: Some(PathBuf::from("configs/uboot.toml")),
        },
    };

    let path = snapshot.store(root.path()).unwrap();
    let loaded = ArceosCommandSnapshot::load(root.path()).unwrap();

    assert_eq!(path, snapshot_path(root.path(), ARCEOS_SNAPSHOT_FILE));
    assert_eq!(loaded, snapshot);
}

#[test]
fn axvisor_snapshot_store_round_trips() {
    let root = tempdir().unwrap();
    let snapshot = AxvisorCommandSnapshot {
        arch: Some("aarch64".into()),
        target: Some(DEFAULT_AXVISOR_TARGET.into()),
        smp: None,
        config: Some(PathBuf::from(
            "tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml",
        )),
        vmconfigs: vec![PathBuf::from("tmp/vm1.toml"), PathBuf::from("tmp/vm2.toml")],
        qemu: AxvisorQemuSnapshot {
            qemu_config: Some(PathBuf::from("configs/qemu.toml")),
        },
        uboot: AxvisorUbootSnapshot {
            uboot_config: Some(PathBuf::from("configs/uboot.toml")),
        },
    };

    let path = snapshot.store(root.path()).unwrap();
    let loaded = AxvisorCommandSnapshot::load(root.path()).unwrap();

    assert_eq!(path, snapshot_path(root.path(), AXVISOR_SNAPSHOT_FILE));
    assert_eq!(loaded, snapshot);
}
