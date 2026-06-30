use std::path::{Path, PathBuf};

use anyhow::bail;

use super::{
    ARCEOS_C_TEST_GROUP, ARCEOS_RUST_TEST_BUILD_GROUP, ARCEOS_RUST_TEST_GROUP,
    ARCEOS_RUST_TEST_PACKAGE, ARCEOS_TEST_SUITE_OS,
    args::ArgsTestQemu,
    assets::{
        arceos_rust_test_dir, build_config_path, qemu_config_path, read_manifest_package_name,
    },
    rust_qemu::rust_qemu_features_for_run,
    types::{ArceosRustQemuCase, QemuTestFlow},
};
use crate::{
    arceos::ArceOS,
    test::{case::TestQemuCase, qemu as qemu_test, suite as test_suite},
};

pub(super) fn discover_rust_qemu_cases(
    arceos: &ArceOS,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<Vec<ArceosRustQemuCase>> {
    let root = arceos_rust_test_dir(arceos);
    rust_qemu_features_for_run(selected_case, allow_missing_selected_case)?
        .into_iter()
        .map(|feature| load_arceos_test_suit_qemu_case(&root, arch, target, feature))
        .collect()
}

pub(super) fn discover_qemu_cases_in_dir(
    dir: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    group: &str,
) -> anyhow::Result<Vec<ArceosRustQemuCase>> {
    qemu_test::discover_qemu_cases(dir, arch, target, selected_case, "ArceOS", group)?
        .into_iter()
        .map(load_rust_qemu_case)
        .collect::<anyhow::Result<Vec<_>>>()
}

pub(super) fn discover_qemu_cases_in_dir_allow_empty(
    dir: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    group: &str,
) -> anyhow::Result<Vec<ArceosRustQemuCase>> {
    qemu_test::discover_qemu_cases_allow_empty(dir, arch, target, selected_case, "ArceOS", group)?
        .into_iter()
        .map(load_rust_qemu_case)
        .collect::<anyhow::Result<Vec<_>>>()
}

fn load_rust_qemu_case(case: qemu_test::DiscoveredQemuCase) -> anyhow::Result<ArceosRustQemuCase> {
    let package = read_manifest_package_name(&case.case_dir.join("Cargo.toml"))?;
    let host_http_server = qemu_test::load_qemu_case_host_http_server(&case.qemu_config_path)?;
    Ok(ArceosRustQemuCase {
        case: TestQemuCase {
            name: case.name,
            display_name: case.display_name,
            case_dir: case.case_dir,
            qemu_config_path: case.qemu_config_path,
            test_commands: Vec::new(),
            host_symbolize_success_regex: Vec::new(),
            host_http_server,
            subcases: Vec::new(),
            grouped_subcase_filter: None,
        },
        build_group: case.build_group,
        build_config_path: case.build_config_path,
        package,
        feature: None,
    })
}

pub(super) fn load_arceos_test_suit_qemu_case(
    root: &Path,
    arch: &str,
    target: &str,
    feature: &str,
) -> anyhow::Result<ArceosRustQemuCase> {
    let build_config_path = arceos_test_suit_build_config_path(root, target)?;
    let qemu_config_path = arceos_test_suit_qemu_config_path(root, arch)?;
    let host_http_server = qemu_test::load_qemu_case_host_http_server(&qemu_config_path)?;
    Ok(ArceosRustQemuCase {
        case: TestQemuCase {
            name: feature.to_string(),
            display_name: feature.to_string(),
            case_dir: root.to_path_buf(),
            qemu_config_path,
            test_commands: Vec::new(),
            host_symbolize_success_regex: Vec::new(),
            host_http_server,
            subcases: Vec::new(),
            grouped_subcase_filter: None,
        },
        build_group: ARCEOS_RUST_TEST_BUILD_GROUP.to_string(),
        build_config_path,
        package: ARCEOS_RUST_TEST_PACKAGE.to_string(),
        feature: Some(feature.to_string()),
    })
}

pub(super) fn arceos_test_suit_build_config_path(
    root: &Path,
    target: &str,
) -> anyhow::Result<PathBuf> {
    build_config_path(root, target, "ArceOS rust test suite")
}

pub(super) fn arceos_test_suit_qemu_config_path(
    root: &Path,
    arch: &str,
) -> anyhow::Result<PathBuf> {
    qemu_config_path(root, arch, "ArceOS rust test suite")
}

pub(super) fn selected_qemu_test_groups(
    workspace_root: &Path,
    args: &ArgsTestQemu,
) -> anyhow::Result<Vec<QemuTestFlow>> {
    if args.only_c {
        return Ok(vec![QemuTestFlow::C]);
    }
    if args.only_rust {
        return Ok(vec![QemuTestFlow::Rust]);
    }

    match args.test_group.as_deref() {
        None => {
            let mut flows = vec![QemuTestFlow::Rust, QemuTestFlow::C];
            for group in test_suite::discover_group_names(workspace_root, ARCEOS_TEST_SUITE_OS)? {
                if group != ARCEOS_RUST_TEST_GROUP && group != ARCEOS_C_TEST_GROUP {
                    flows.push(QemuTestFlow::Generic(group));
                }
            }
            Ok(flows)
        }
        Some(ARCEOS_RUST_TEST_GROUP) => Ok(vec![QemuTestFlow::Rust]),
        Some(ARCEOS_C_TEST_GROUP) => Ok(vec![QemuTestFlow::C]),
        Some(group) => {
            let dir = test_suite::group_dir(workspace_root, ARCEOS_TEST_SUITE_OS, group);
            if dir.is_dir() {
                Ok(vec![QemuTestFlow::Generic(group.to_string())])
            } else {
                bail!(
                    "unsupported ArceOS qemu test group `{group}`; supported groups are: {}",
                    test_suite::supported_group_names(workspace_root, ARCEOS_TEST_SUITE_OS)
                        .unwrap_or_else(|_| {
                            format!("{ARCEOS_RUST_TEST_GROUP}, {ARCEOS_C_TEST_GROUP}")
                        })
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn qemu_args(only_rust: bool, only_c: bool, package: Vec<String>) -> ArgsTestQemu {
        ArgsTestQemu {
            arch: None,
            target: Some("x86_64-unknown-none".to_string()),
            test_group: None,
            test_case: None,
            list: false,
            package,
            only_rust,
            only_c,
            no_symbolize: false,
            keep_qemu_log: false,
        }
    }

    #[test]
    fn selected_qemu_test_groups_default_runs_rust_then_c() {
        let dir = tempdir().unwrap();
        let flows =
            selected_qemu_test_groups(dir.path(), &qemu_args(false, false, Vec::new())).unwrap();

        assert_eq!(flows, &[QemuTestFlow::Rust, QemuTestFlow::C]);
    }

    #[test]
    fn selected_qemu_test_groups_only_rust_skips_c() {
        let dir = tempdir().unwrap();
        let flows =
            selected_qemu_test_groups(dir.path(), &qemu_args(true, false, Vec::new())).unwrap();

        assert_eq!(flows, &[QemuTestFlow::Rust]);
    }

    #[test]
    fn selected_qemu_test_groups_only_c_skips_rust() {
        let dir = tempdir().unwrap();
        let flows =
            selected_qemu_test_groups(dir.path(), &qemu_args(false, true, Vec::new())).unwrap();

        assert_eq!(flows, &[QemuTestFlow::C]);
    }

    #[test]
    fn selected_qemu_test_groups_package_filter_no_longer_changes_groups() {
        let dir = tempdir().unwrap();
        let flows = selected_qemu_test_groups(
            dir.path(),
            &qemu_args(false, false, vec!["arceos-test-suit".to_string()]),
        )
        .unwrap();

        assert_eq!(flows, &[QemuTestFlow::Rust, QemuTestFlow::C]);
    }
}
