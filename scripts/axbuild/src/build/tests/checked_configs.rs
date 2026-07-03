use super::{
    config::{
        checked_in_build_config_roots, checked_in_toml_files, declares_removed_plat_dyn_field,
        declares_removed_platform_feature,
    },
    *,
};

#[test]
fn checked_in_build_configs_do_not_declare_removed_platform_features() {
    let workspace = crate::context::workspace_root_path().unwrap();
    let mut offenders = Vec::new();

    for path in checked_in_toml_files(checked_in_build_config_roots(&workspace)) {
        let content = fs::read_to_string(&path).unwrap();
        if declares_removed_platform_feature(&content) {
            offenders.push(
                path.strip_prefix(&workspace)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
        }
    }

    assert!(
        offenders.is_empty(),
        "removed platform features are no longer supported: {offenders:#?}"
    );
}

#[test]
fn checked_in_build_configs_do_not_declare_removed_plat_dyn_field() {
    let workspace = crate::context::workspace_root_path().unwrap();
    let mut offenders = Vec::new();

    for path in checked_in_toml_files(checked_in_build_config_roots(&workspace)) {
        let content = fs::read_to_string(&path).unwrap();
        if declares_removed_plat_dyn_field(&content) {
            offenders.push(
                path.strip_prefix(&workspace)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
        }
    }

    assert!(
        offenders.is_empty(),
        "build configs must remove the `plat_dyn` field: {offenders:#?}"
    );
}
