use super::*;

#[test]
fn std_build_does_not_auto_enable_app_arceos_feature() {
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

    assert!(!cargo.features.contains(&"arceos".to_string()));
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
