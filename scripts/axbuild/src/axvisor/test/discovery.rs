use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

use super::{AXVISOR_NORMAL_GROUP, AXVISOR_TEST_SUITE_OS, AxvisorQemuCase, BoardTestGroup};
use crate::{
    context::resolve_axvisor_arch_and_target,
    test::{board as board_test, qemu as test_qemu, qemu::parse_test_target, suite as test_suite},
};

pub(crate) fn parse_target(
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    parse_test_target(
        arch,
        target,
        "axvisor qemu tests",
        &crate::context::supported_arches(),
        &crate::context::supported_targets(),
        resolve_axvisor_arch_and_target,
    )
}

pub(crate) fn discover_qemu_cases(
    workspace_root: &Path,
    group: &str,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<AxvisorQemuCase>> {
    let test_suite_dir = test_suite_dir(workspace_root, group)?;
    test_qemu::discover_qemu_cases(
        &test_suite_dir,
        arch,
        target,
        selected_case,
        "Axvisor",
        "qemu",
    )?
    .into_iter()
    .map(load_qemu_case)
    .collect()
}

fn load_qemu_case(case: test_qemu::DiscoveredQemuCase) -> anyhow::Result<AxvisorQemuCase> {
    let build_group = case.build_group;
    let build_config_path = case.build_config_path;
    let test_case = test_qemu::load_test_qemu_case_fields(
        case.display_name,
        case.name,
        case.case_dir,
        case.qemu_config_path,
        "Axvisor",
        false,
    )?;
    Ok(AxvisorQemuCase {
        case: test_case,
        build_group,
        build_config_path,
    })
}

pub(crate) fn discover_board_test_groups(
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
    board: Option<&str>,
) -> anyhow::Result<Vec<BoardTestGroup>> {
    let test_suite_dir = test_suite_dir(workspace_root, group)?;
    let groups = collect_board_test_groups(workspace_root, &test_suite_dir)?;
    board_test::filter_board_test_groups(groups, selected_case, board, "axvisor", || {
        format!(
            "no Axvisor board test groups found under {}",
            test_suite_dir.display()
        )
    })
}

fn collect_board_test_groups(
    workspace_root: &Path,
    test_suite_dir: &Path,
) -> anyhow::Result<Vec<BoardTestGroup>> {
    let mut groups = Vec::new();
    for info in board_test::discover_board_case_build_infos(test_suite_dir, "Axvisor")? {
        ensure_board_run_config(&info.board_test_config_path)?;
        let build_config = resolve_workspace_path(workspace_root, info.build_config_path);
        ensure_file_exists(&build_config, "Axvisor board build group config")?;
        groups.push(BoardTestGroup {
            name: info.name,
            board_name: info.board_name,
            build_config,
            board_test_config_path: info.board_test_config_path,
        });
    }

    Ok(groups)
}

pub(super) fn discover_uboot_test_group(
    workspace_root: &Path,
    board: &str,
    guest: &str,
) -> anyhow::Result<BoardTestGroup> {
    let board_name = format!("{board}-{guest}");
    let mut groups = discover_board_test_groups(
        workspace_root,
        AXVISOR_NORMAL_GROUP,
        None,
        Some(&board_name),
    )?;

    if groups.len() == 1 {
        return Ok(groups.remove(0));
    }

    let labels = groups
        .iter()
        .map(|group| format!("{}/{}", group.name, group.board_name))
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "ambiguous axvisor uboot test target board=`{board}` guest=`{guest}`. Matching cases are: \
         {labels}"
    )
}

fn ensure_board_run_config(path: &Path) -> anyhow::Result<()> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str::<ostool::board::config::BoardRunConfig>(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(())
}

fn resolve_workspace_path(workspace_root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

pub(super) fn ensure_file_exists(path: &Path, label: &str) -> anyhow::Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        bail!("{label} maps to missing file `{}`", path.display())
    }
}

pub(super) fn test_suite_dir(workspace_root: &Path, group: &str) -> anyhow::Result<PathBuf> {
    test_suite::require_group_dir(workspace_root, AXVISOR_TEST_SUITE_OS, "Axvisor", group)
}

pub(super) fn test_suite_root(workspace_root: &Path) -> PathBuf {
    test_suite::suite_root(workspace_root, AXVISOR_TEST_SUITE_OS)
}

pub(super) fn discover_test_group_names(workspace_root: &Path) -> anyhow::Result<Vec<String>> {
    test_suite::discover_group_names(workspace_root, AXVISOR_TEST_SUITE_OS)
}

pub(super) fn qemu_list_error_is_ignorable(kind: test_qemu::ListQemuCasesErrorKind) -> bool {
    matches!(
        kind,
        test_qemu::ListQemuCasesErrorKind::EmptyGroup
            | test_qemu::ListQemuCasesErrorKind::UnknownSelectedCase
    )
}
