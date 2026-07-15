use super::*;

#[test]
fn std_build_nested_features_are_passed_through_not_enabled_on_app() {
    let mut envs = HashMap::new();
    let mut features = vec![
        "ax-driver/virtio-blk".to_string(),
        "ax-driver/virtio-net".to_string(),
        "dns".to_string(),
    ];

    pass_std_build_nested_features(
        &mut envs,
        &mut features,
        &["dns".to_string()],
        &[
            "dns".to_string(),
            "plat-dyn".to_string(),
            "std-compat".to_string(),
            "virtio-blk".to_string(),
            "virtio-net".to_string(),
        ],
    );

    assert_eq!(
        features,
        vec![
            "ax-std/dns".to_string(),
            "ax-std/virtio-blk".to_string(),
            "ax-std/virtio-net".to_string(),
            "dns".to_string(),
        ]
    );
    assert!(envs.is_empty());
}

#[test]
fn std_build_runtime_features_are_passed_through_after_normalization() {
    let mut info = BuildInfo {
        features: vec!["dns".to_string()],
        ..BuildInfo::default()
    };

    info.resolve_std_features();
    let mut envs = HashMap::new();
    pass_std_build_nested_features(
        &mut envs,
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
    assert!(envs.is_empty());
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
    assert!(cargo.to_bin);
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
