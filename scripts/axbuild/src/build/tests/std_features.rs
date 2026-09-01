use super::*;

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

    assert!(features.contains(&"ax-std/dns".to_string()));
    assert!(features.contains(&"ax-std/nvme".to_string()));
    assert!(features.contains(&"ax-std/virtio-net".to_string()));
    assert!(features.contains(&"dns".to_string()));
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

    assert!(info.features.contains(&"ax-std/dns".to_string()));
    assert!(info.features.contains(&"dns".to_string()));
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
    assert!(cargo.features.iter().any(|feature| feature == "ax-std/dns"));
    assert!(cargo.features.iter().any(|feature| feature == "ax-std/fs"));
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
    let prebuild = fs::read_to_string(
        cargo
            .pre_build_cmds
            .first()
            .expect("dynamic std build should prepare a pre-build archive script"),
    )
    .unwrap();
    assert!(prebuild.contains("target_name='x86_64-unknown-linux-musl'"));
    assert!(!prebuild.contains("cargo}\" build -p ax-std"));
    assert!(!prebuild.contains("libax_std.a"));
    assert!(prebuild.contains("libc.a"));
    assert!(prebuild.contains("archive_tool()"));
    assert!(prebuild.contains("$(rustc --print sysroot)"));
    assert!(prebuild.contains("create_empty_archive \"$fake_dir/libc.a\""));
    assert!(prebuild.contains("create_empty_archive \"$fake_dir/libunwind.a\""));
}
