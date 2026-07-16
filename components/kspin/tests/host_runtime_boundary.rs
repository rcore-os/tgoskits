use std::{fs, path::PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("ax-kspin must live under components/")
        .to_path_buf()
}

#[test]
fn host_runtime_is_a_dev_fixture_not_a_unified_ax_kspin_feature() {
    let root = workspace_root();
    let kspin = fs::read_to_string(root.join("components/kspin/Cargo.toml")).unwrap();
    assert!(!kspin.contains("host-test"));

    let fixture =
        fs::read_to_string(root.join("components/kspin/test-runtime/Cargo.toml")).unwrap();
    assert!(fixture.contains("publish = false"));

    for manifest in [
        "components/rsext4/Cargo.toml",
        "components/starry-process/Cargo.toml",
        "components/starry-signal/Cargo.toml",
        "drivers/ax-driver/Cargo.toml",
    ] {
        let manifest = fs::read_to_string(root.join(manifest)).unwrap();
        let dev_dependencies = manifest
            .split_once("[dev-dependencies]")
            .map(|(_, section)| section)
            .expect("consumer must select its host runtime only for tests");
        assert!(dev_dependencies.contains("ax-kspin-test-runtime"));
    }
}
