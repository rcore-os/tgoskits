use super::*;

#[test]
fn arceos_io_test_selects_a_concrete_fat_filesystem() {
    let workspace = crate::context::workspace_root_path().unwrap();
    let manifest_path = workspace.join("apps/arceos/io_test/Cargo.toml");
    let manifest: toml::Value =
        toml::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let arceos_features = manifest["features"]["arceos"]
        .as_array()
        .expect("arceos-io_test must declare its ArceOS feature set");

    assert!(
        arceos_features
            .iter()
            .filter_map(toml::Value::as_str)
            .any(|feature| feature == "ax-std/fatfs"),
        "{} must select FAT for its generated FAT32 NVMe rootfs",
        manifest_path.display()
    );
}

#[test]
fn arceos_io_test_x86_uses_uefi_handoff() {
    let workspace = crate::context::workspace_root_path().unwrap();
    let config_path = workspace.join("apps/arceos/io_test/qemu-x86_64.toml");
    let config: toml::Value = toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();

    assert_eq!(
        config.get("uefi").and_then(toml::Value::as_bool),
        Some(true),
        "{} must use the supported x86_64 UEFI handoff",
        config_path.display()
    );
    assert_eq!(
        config.get("to_bin").and_then(toml::Value::as_bool),
        Some(true),
        "{} must retain the UEFI runner's explicit BIN artifact contract",
        config_path.display()
    );
}

#[test]
fn axfs_vfs_enables_sleepable_mutexes() {
    let workspace = crate::context::workspace_root_path().unwrap();
    let manifest_path = workspace.join("fs/ax-fs-ng/Cargo.toml");
    let manifest: toml::Value =
        toml::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let vfs_features = manifest["features"]["vfs"]
        .as_array()
        .expect("ax-fs-ng must declare its VFS feature set");

    assert!(
        vfs_features
            .iter()
            .filter_map(toml::Value::as_str)
            .any(|feature| feature == "ax-sync/sleep"),
        "{} must keep filesystem I/O locks sleepable for channel-backed block completion",
        manifest_path.display()
    );
}

#[test]
fn axfs_ng_host_tests_use_host_sync_without_axtest() {
    let workspace = crate::context::workspace_root_path().unwrap();
    let manifest_path = workspace.join("fs/ax-fs-ng/Cargo.toml");
    let manifest: toml::Value =
        toml::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let features = manifest["features"]
        .as_table()
        .expect("ax-fs-ng must declare a feature table");
    let host_test_features = features["host-test"]
        .as_array()
        .expect("ax-fs-ng must expose an explicit host-test feature");

    assert!(
        host_test_features
            .iter()
            .filter_map(toml::Value::as_str)
            .any(|feature| feature == "ax-sync/host-test"),
        "{} must enable the host synchronization backend only through host-test",
        manifest_path.display()
    );
    assert!(
        !features.contains_key("axtest"),
        "{} must not expose an axtest feature",
        manifest_path.display()
    );

    let dev_ax_sync_features = manifest["target"]["cfg(not(target_os = \"none\"))"]
        ["dev-dependencies"]["ax-sync"]["features"]
        .as_array()
        .expect("host tests must enable the host synchronization backend");
    assert!(
        dev_ax_sync_features
            .iter()
            .filter_map(toml::Value::as_str)
            .any(|feature| feature == "host-test"),
        "{} must let ordinary host cargo tests link ax-sync",
        manifest_path.display()
    );
    assert!(
        manifest
            .get("dev-dependencies")
            .and_then(toml::Value::as_table)
            .and_then(|dependencies| dependencies.get("ax-sync"))
            .and_then(toml::Value::as_table)
            .and_then(|dependency| dependency.get("features"))
            .and_then(toml::Value::as_array)
            .is_none_or(|features| {
                features
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .all(|feature| feature != "host-test")
            }),
        "{} must keep the host backend target-specific",
        manifest_path.display()
    );
}

#[test]
fn std_build_nested_features_are_passed_through_not_enabled_on_app() {
    let mut features = vec![
        "ax-driver/nvme".to_string(),
        "ax-driver/virtio-net".to_string(),
        "dns".to_string(),
    ];

    pass_std_build_nested_features(
        &mut features,
        &["dns".to_string()],
        &[
            "dns".to_string(),
            "plat-dyn".to_string(),
            "std-compat".to_string(),
            "nvme".to_string(),
            "virtio-net".to_string(),
        ],
    );

    assert_eq!(
        features,
        vec![
            "ax-std/dns".to_string(),
            "ax-std/nvme".to_string(),
            "ax-std/virtio-net".to_string(),
            "dns".to_string(),
        ]
    );
}

#[test]
fn std_build_runtime_features_are_passed_through_after_normalization() {
    let mut info = BuildInfo {
        features: vec!["dns".to_string()],
        ..BuildInfo::default()
    };

    info.resolve_std_features();
    pass_std_build_nested_features(
        &mut info.features,
        &["dns".to_string()],
        &[
            "dns".to_string(),
            "plat-dyn".to_string(),
            "std-compat".to_string(),
        ],
    );

    assert_eq!(
        info.features,
        vec!["ax-std/dns".to_string(), "dns".to_string()]
    );
}

#[test]
fn std_build_cargo_config_builds_fake_lib_before_app() {
    let metadata = repo_metadata();
    let cargo = BuildInfo {
        features: vec!["ax-std".to_string(), "fs".to_string(), "dns".to_string()],
        ..BuildInfo::default()
    }
    .into_prepared_base_cargo_config_with_metadata(
        "arceos-helloworld",
        "x86_64-unknown-none",
        &metadata,
    )
    .unwrap();

    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/x86_64-unknown-linux-musl.json")
    );
    assert!(
        cargo
            .args
            .windows(2)
            .any(|pair| pair == ["-Z", "json-target-spec"])
    );
    assert_eq!(
        cargo.features,
        vec!["ax-std/dns".to_string(), "ax-std/fs".to_string(),]
    );
    assert!(!cargo.to_bin);
    assert_eq!(
        cargo.env.get("CARGO_UNSTABLE_JSON_TARGET_SPEC"),
        Some(&"true".to_string())
    );
    assert!(!cargo.env.contains_key("AXSTD_STD_DEFAULT_FEATURES"));
    assert_eq!(
        cargo.env.get("AX_TARGET"),
        Some(&"x86_64-unknown-none".to_string())
    );
    assert!(
        cargo
            .extra_config
            .as_ref()
            .is_some_and(|path| path.ends_with("config-x86_64-unknown-linux-musl-dynamic.toml"))
    );
    assert_eq!(cargo.pre_build_cmds.len(), 1);
    let prebuild = fs::read_to_string(&cargo.pre_build_cmds[0]).unwrap();
    assert!(prebuild.contains("target_name='x86_64-unknown-linux-musl'"));
    assert!(!prebuild.contains("cargo}\" build -p ax-std"));
    assert!(!prebuild.contains("libax_std.a"));
    assert!(prebuild.contains("libc.a"));
    assert!(prebuild.contains("archive_tool()"));
    assert!(prebuild.contains("$(rustc --print sysroot)"));
    assert!(prebuild.contains("create_empty_archive \"$fake_dir/libc.a\""));
    assert!(prebuild.contains("create_empty_archive \"$fake_dir/libunwind.a\""));
}
