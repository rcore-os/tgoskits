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
        &repo_axbuild_dir(),
    )
    .unwrap();

    assert!(!cargo.features.contains(&"arceos".to_string()));
}
