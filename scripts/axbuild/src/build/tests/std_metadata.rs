use super::*;

#[test]
fn std_build_uses_package_axstd_metadata_for_ax_std_features() {
    let workspace = temp_workspace("std-app", "").unwrap();
    let app_manifest = workspace.join("app/Cargo.toml");
    fs::write(
        &app_manifest,
        "[package]\nname = \"std-app\"\nversion = \"0.1.0\"\nedition = \
         \"2024\"\n\n[package.metadata.axstd]\nfeatures = [\"multitask\", \"net\", \
         \"log-level-debug\"]\n",
    )
    .unwrap();

    let metadata = metadata_for_manifest(&workspace.join("Cargo.toml"));
    let mut info = BuildInfo {
        features: vec!["dns".to_string()],
        ..BuildInfo::default()
    };

    info.resolve_std_features_with_metadata("std-app", "x86_64-unknown-none", &metadata);
    let mut envs = HashMap::new();
    pass_std_build_nested_features(
        &mut envs,
        &mut info.features,
        &[],
        &[
            "dns".to_string(),
            "multitask".to_string(),
            "net".to_string(),
            "std-compat".to_string(),
        ],
    );

    assert_eq!(
        info.features,
        vec![
            "ax-std/dns".to_string(),
            "ax-std/multitask".to_string(),
            "ax-std/net".to_string(),
            "ax-std/std-compat".to_string(),
        ]
    );
    assert!(envs.is_empty());
}

#[test]
fn std_build_auto_enables_app_arceos_feature_when_declared() {
    let metadata = repo_metadata();
    let cargo = BuildInfo {
        features: Vec::new(),
        ..BuildInfo::default()
    }
    .into_prepared_base_cargo_config_with_metadata(
        "arceos-helloworld",
        "x86_64-unknown-none",
        &metadata,
    )
    .unwrap();

    assert!(cargo.features.contains(&"arceos".to_string()));
}

#[test]
fn std_build_does_not_inject_arceos_feature_when_app_lacks_it() {
    let mut features = vec!["dns".to_string()];

    inject_arceos_feature_for_std_build(&mut features, &["dns".to_string()]);

    assert_eq!(features, vec!["dns".to_string()]);
}

#[test]
fn std_build_uses_dynamic_platform_features_without_static_hal_platform() {
    let metadata = repo_metadata();
    let cargo = BuildInfo {
        features: vec![
            "ax-std".to_string(),
            "ax-driver/virtio-net".to_string(),
            "net".to_string(),
        ],
        ..BuildInfo::default()
    }
    .into_prepared_base_cargo_config_with_metadata(
        "arceos-httpclient",
        "aarch64-unknown-none-softfloat",
        &metadata,
    )
    .unwrap();

    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/aarch64-unknown-linux-musl.json")
    );
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(cargo.features.contains(&"ax-std/smp".to_string()));
    assert!(cargo.features.contains(&"ax-std/std-compat".to_string()));
    assert!(cargo.features.contains(&"ax-std/virtio-net".to_string()));
    assert!(cargo.features.contains(&"ax-std/net".to_string()));
    assert!(cargo.to_bin);
    assert_eq!(
        cargo.env.get("AX_TARGET"),
        Some(&"aarch64-unknown-none-softfloat".to_string())
    );
    assert!(
        cargo
            .features
            .iter()
            .all(|feature| !feature.starts_with("ax-std/aarch64-"))
    );
}

#[test]
fn std_build_aarch64_defaults_to_dynamic_platform() {
    let metadata = repo_metadata();
    let cargo = BuildInfo {
        ..BuildInfo::default()
    }
    .into_prepared_base_cargo_config_with_metadata(
        "arceos-helloworld",
        "aarch64-unknown-none-softfloat",
        &metadata,
    )
    .unwrap();

    assert!(
        cargo
            .target
            .ends_with("scripts/targets/std/pie/aarch64-unknown-linux-musl.json")
    );
    assert!(!cargo.env.contains_key("AX_CONFIG_PATH"));
    assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
    assert!(cargo.features.contains(&"ax-std/smp".to_string()));
    assert!(cargo.features.contains(&"ax-std/std-compat".to_string()));
    assert!(
        cargo
            .features
            .iter()
            .all(|feature| !feature.starts_with("ax-std/aarch64-"))
    );
    let config = std::fs::read_to_string(cargo.extra_config.unwrap()).unwrap();
    assert!(!config.contains("--cfg"));
    assert!(!config.contains("--check-cfg"));
    assert!(!config.contains("relocation-model"));
    assert!(!config.contains("code-model"));
}

#[test]
fn std_build_config_preserves_backtrace_rustflags_from_env() {
    let metadata = repo_metadata();
    let mut info = BuildInfo::default();
    info.env.insert("DWARF".to_string(), "y".to_string());

    let cargo = info
        .into_prepared_base_cargo_config_with_metadata(
            "arceos-helloworld",
            "x86_64-unknown-none",
            &metadata,
        )
        .unwrap();

    let config = std::fs::read_to_string(cargo.extra_config.unwrap()).unwrap();
    assert!(config.contains(r#""-Cdebuginfo=2""#));
    assert!(config.contains(r#""-Cstrip=none""#));
    assert!(config.contains(r#""-Cforce-frame-pointers=yes""#));
}

#[test]
fn std_build_config_enables_stack_protector_from_feature() {
    let metadata = repo_metadata();
    let info = BuildInfo {
        features: vec!["stack-protector".to_string()],
        ..BuildInfo::default()
    };

    let cargo = info
        .into_prepared_base_cargo_config_with_metadata(
            "arceos-helloworld",
            "x86_64-unknown-none",
            &metadata,
        )
        .unwrap();

    assert!(
        cargo
            .features
            .contains(&"ax-std/stack-protector".to_string())
    );
    let config = std::fs::read_to_string(cargo.extra_config.unwrap()).unwrap();
    assert!(config.contains(r#""-Zstack-protector=strong""#));
}
