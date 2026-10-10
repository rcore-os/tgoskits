use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

use super::{AXVISOR_NORMAL_GROUP, AXVISOR_TEST_SUITE_OS, AxvisorQemuCase, BoardTestGroup};
use crate::{
    context::resolve_axvisor_arch_and_target,
    test::{board as board_test, qemu as test_qemu, qemu::parse_test_target, suite as test_suite},
};

/// AxVisor cases are split across two suite trees: the regular
/// `test-suit/axvisor` tree and the `apps/axvisor` tree that holds the nightly
/// cases migrated out of `test-suit`. Discovery merges both so functional and
/// nightly cases stay selectable through the same filters while keeping the
/// directory split.
const AXVISOR_MIGRATED_SUITE_ROOT: &str = "apps/axvisor";

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
    let roots = group_dirs(workspace_root, group)?;
    let mut cases = Vec::new();
    let mut missing_selected_case = None;
    for root in &roots {
        match test_qemu::discover_qemu_cases_allow_empty(
            root,
            arch,
            target,
            selected_case,
            "Axvisor",
            "qemu",
        ) {
            Ok(found) => {
                for case in found {
                    cases.push(load_qemu_case(case)?);
                }
            }
            Err(error) => {
                // `allow_empty` already reports a root without cases for this
                // arch/target as `Ok` when no case is selected, so an error here
                // is a genuine config/scan failure that must propagate. With a
                // selection the error may instead mean this root simply lacks
                // the case, which a sibling root may still provide: re-scanning
                // without a selection reproduces config/scan failures (they do
                // not depend on the selection) and stays `Ok` for a root that
                // only misses the case.
                let missing_from_root = selected_case.is_some()
                    && test_qemu::discover_qemu_cases_allow_empty(
                        root, arch, target, None, "Axvisor", "qemu",
                    )
                    .is_ok();
                if !missing_from_root {
                    return Err(error);
                }
                missing_selected_case.get_or_insert(error);
            }
        }
    }

    // A root without the requested case reports an empty selection; surface the
    // retained error only when no root contributed the case, so a case missing
    // everywhere still fails instead of silently shrinking coverage.
    if cases.is_empty()
        && let Some(error) = missing_selected_case
    {
        return Err(error);
    }
    Ok(cases)
}

/// Merge the listed QEMU cases of every suite root for one group.
///
/// Missing-case errors are ignorable per root so the nightly cases under
/// `apps/axvisor` and the functional cases under `test-suit/axvisor` can be
/// listed together; unexpected errors still propagate.
pub(super) fn list_all_qemu_cases_with_archs(
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<test_qemu::ListedQemuCase>> {
    let roots = group_dirs(workspace_root, group)?;
    let mut cases = Vec::new();
    for root in &roots {
        match test_qemu::discover_all_qemu_cases_with_archs(root, selected_case, "Axvisor", group) {
            Ok(found) => cases.extend(found),
            Err(error) if qemu_list_error_is_ignorable(error.kind()) => {}
            Err(error) => return Err(anyhow::Error::new(error)),
        }
    }
    Ok(cases)
}

/// Merge the bare case names of every suite root for one group.
///
/// A root that does not provide the group or the selected case is ignorable so
/// the nightly cases under `apps/axvisor` and the functional cases under
/// `test-suit/axvisor` list together; any other error surfaces instead of being
/// dropped.
pub(super) fn list_all_qemu_cases(
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let roots = group_dirs(workspace_root, group)?;
    let mut cases = Vec::new();
    for root in &roots {
        match test_qemu::discover_all_qemu_cases(root, selected_case, "Axvisor", group) {
            Ok(found) => cases.extend(found),
            Err(error) if qemu_list_error_is_ignorable(error.kind()) => {}
            Err(error) => return Err(anyhow::Error::new(error)),
        }
    }
    Ok(cases)
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
    if !test_case.test_commands.is_empty() {
        bail!(
            "Axvisor QEMU case `{}` does not support `test_commands`; use `shell_check_steps` to \
             execute commands and check their results",
            test_case.qemu_config_path.display()
        );
    }
    Ok(AxvisorQemuCase {
        case: test_case,
        build_group,
        build_config_path,
    })
}

pub(crate) fn discover_board_test_groups(
    workspace_root: &Path,
    group: &str,
    selected_cases: &[String],
    boards: &[String],
) -> anyhow::Result<Vec<BoardTestGroup>> {
    let roots = board_test_group_roots(workspace_root, group)?;
    let mut groups = Vec::new();
    for root in &roots {
        groups.extend(collect_board_test_groups(workspace_root, root)?);
    }
    let searched = roots
        .iter()
        .map(|dir| dir.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    board_test::filter_board_test_groups_by_board_names(
        groups,
        selected_cases,
        boards,
        "axvisor",
        || format!("no Axvisor board test groups found under {searched}"),
    )
}

/// Suite roots that hold AxVisor board cases. Discovery searches each suite
/// tree (`test-suit/axvisor/<group>` and `apps/axvisor/<group>`), and for the
/// default `normal` group also the `benchmarks/axvisor` tree that holds the
/// real board performance cases. All three stay selectable through the same
/// `--board`/`--test-case` filters.
fn board_test_group_roots(workspace_root: &Path, group: &str) -> anyhow::Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for suite_root in suite_roots(workspace_root) {
        let group_dir = suite_root.join(group);
        if group_dir.is_dir() {
            roots.push(group_dir);
        }
    }
    if group == AXVISOR_NORMAL_GROUP {
        let benchmark_suite_dir = benchmark_suite_root(workspace_root);
        if benchmark_suite_dir.is_dir() {
            roots.push(benchmark_suite_dir);
        }
    }
    if roots.is_empty() {
        bail!(
            "unsupported Axvisor test group `{group}`. Supported groups are: {}",
            supported_group_names(workspace_root)?
        );
    }
    Ok(roots)
}

/// Suite trees that hold AxVisor cases, in lookup order.
fn suite_roots(workspace_root: &Path) -> Vec<PathBuf> {
    vec![
        test_suite::suite_root(workspace_root, AXVISOR_TEST_SUITE_OS),
        workspace_root.join(AXVISOR_MIGRATED_SUITE_ROOT),
    ]
}

fn benchmark_suite_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join("benchmarks").join("axvisor")
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
    let selected_boards = vec![board_name];
    let mut groups =
        discover_board_test_groups(workspace_root, AXVISOR_NORMAL_GROUP, &[], &selected_boards)?;

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

/// Existing group directories across every AxVisor suite root.
///
/// At least one root must provide the group; otherwise the group name is
/// unsupported.
pub(super) fn group_dirs(workspace_root: &Path, group: &str) -> anyhow::Result<Vec<PathBuf>> {
    let dirs = suite_roots(workspace_root)
        .into_iter()
        .map(|root| root.join(group))
        .filter(|dir| dir.is_dir())
        .collect::<Vec<_>>();
    if dirs.is_empty() {
        bail!(
            "unsupported Axvisor test group `{group}`. Supported groups are: {}",
            supported_group_names(workspace_root)?
        );
    }
    Ok(dirs)
}

/// Human-readable list of the AxVisor suite roots for error messages.
pub(super) fn suite_roots_label(workspace_root: &Path) -> String {
    suite_roots(workspace_root)
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn discover_test_group_names(workspace_root: &Path) -> anyhow::Result<Vec<String>> {
    let mut groups = BTreeSet::new();
    for root in suite_roots(workspace_root) {
        groups.extend(test_suite::discover_group_names_in_root(&root)?);
    }
    Ok(groups.into_iter().collect())
}

pub(super) fn supported_group_names(workspace_root: &Path) -> anyhow::Result<String> {
    let groups = discover_test_group_names(workspace_root)?;
    Ok(if groups.is_empty() {
        "<none>".to_string()
    } else {
        groups.join(", ")
    })
}

pub(super) fn qemu_list_error_is_ignorable(kind: test_qemu::ListQemuCasesErrorKind) -> bool {
    matches!(
        kind,
        test_qemu::ListQemuCasesErrorKind::EmptyGroup
            | test_qemu::ListQemuCasesErrorKind::UnknownSelectedCase
    )
}
