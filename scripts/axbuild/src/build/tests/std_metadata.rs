use super::*;

#[test]
fn std_build_does_not_auto_enable_app_arceos_feature() {
    let metadata = repo_metadata();
    let common = CommonConfig {
        features: Vec::new(),
        ..CommonConfig::default()
    };
    let cargo = into_prepared_base_cargo_config_with_metadata(
        common,
        "arceos-helloworld",
        "x86_64-unknown-none",
        &metadata,
        &repo_axbuild_dir(),
        default_runtime(),
    )
    .unwrap();

    assert!(!cargo.features.contains(&"arceos".to_string()));
}
