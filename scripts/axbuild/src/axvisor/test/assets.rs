use std::path::{Component, Path, PathBuf};

use anyhow::ensure;

use crate::test::case as test_case;

pub(super) fn axvisor_case_asset_config() -> test_case::CaseAssetConfig {
    test_case::CaseAssetConfig {
        grouped_execution: test_case::GroupedCaseExecution::External,
        script_env: test_case::CaseScriptEnvConfig {
            staging_root: "AXVISOR_TEST_STAGING_ROOT".to_string(),
            case_dir: "AXVISOR_TEST_CASE_DIR".to_string(),
            case_c_dir: "AXVISOR_TEST_CASE_C_DIR".to_string(),
            case_work_dir: "AXVISOR_TEST_CASE_WORK_DIR".to_string(),
            case_build_dir: "AXVISOR_TEST_CASE_BUILD_DIR".to_string(),
            case_overlay_dir: "AXVISOR_TEST_CASE_OVERLAY_DIR".to_string(),
        },
        cache_env_vars: Vec::new(),
        prepare_staging_root: |_| Ok(()),
        prepare_guest_package_env: None,
    }
}

pub(super) fn resolve_workspace_path(
    workspace_root: &Path,
    configured_path: &str,
    variable: &str,
) -> anyhow::Result<PathBuf> {
    let configured_path = Path::new(configured_path);
    ensure!(
        !configured_path.is_absolute()
            && configured_path
                .components()
                .all(|component| matches!(component, Component::CurDir | Component::Normal(_))),
        "{variable} must be a workspace-relative path without parent traversal"
    );
    Ok(workspace_root.join(configured_path))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn configured_asset_path_must_stay_inside_workspace() {
        let root = tempdir().unwrap();

        assert_eq!(
            resolve_workspace_path(root.path(), "tmp/asset", "TEST_ASSET").unwrap(),
            root.path().join("tmp/asset")
        );
        assert!(resolve_workspace_path(root.path(), "../outside", "TEST_ASSET").is_err());
        assert!(resolve_workspace_path(root.path(), "/tmp/outside", "TEST_ASSET").is_err());
    }
}
