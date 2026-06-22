use super::{
    config::{
        checked_in_build_config_roots, checked_in_toml_files, declares_default_dynamic_platform,
        declares_non_dynamic_platform, declares_static_platform,
    },
    *,
};

#[test]
fn static_platform_configs_declare_non_dynamic_builds() {
    let workspace = crate::context::workspace_root_path().unwrap();
    let mut offenders = Vec::new();

    for path in checked_in_toml_files(checked_in_build_config_roots(&workspace)) {
        let content = fs::read_to_string(&path).unwrap();
        if declares_static_platform(&content) && !declares_non_dynamic_platform(&content) {
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
        "static platform configs must set `plat_dyn = false`: {offenders:#?}"
    );
}

#[test]
fn checked_in_build_configs_do_not_declare_default_dynamic_builds() {
    let workspace = crate::context::workspace_root_path().unwrap();
    let mut offenders = Vec::new();

    for path in checked_in_toml_files(checked_in_build_config_roots(&workspace)) {
        let content = fs::read_to_string(&path).unwrap();
        if declares_default_dynamic_platform(&content) {
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
        "default dynamic configs should omit `plat_dyn = true`: {offenders:#?}"
    );
}
