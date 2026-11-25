use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

use super::{AXVISOR_TEST_SUITE_OS, AxvisorQemuCase, BoardTestGroup};
use crate::{
    context::resolve_axvisor_arch_and_target,
    execution::{
        board as board_test, qemu as test_qemu, qemu::parse_test_target, suite as test_suite,
    },
};

/// Selects a configuration namespace without changing the execution machinery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaseSource<'a> {
    External(&'a Path),
    ExternalApps(&'a Path),
    Tests,
    Apps,
}

impl CaseSource<'_> {
    fn suite_roots(self, workspace_root: &Path) -> Vec<PathBuf> {
        match self {
            Self::Tests => vec![test_suite::suite_root(
                workspace_root,
                AXVISOR_TEST_SUITE_OS,
            )],
            Self::Apps => vec![workspace_root.join("apps/axvisor")],
            Self::External(root) | Self::ExternalApps(root) => vec![root.to_path_buf()],
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Tests | Self::External(_) => "axvisor",
            Self::Apps | Self::ExternalApps(_) => "axvisor app",
        }
    }

    /// Repository tests use the suite root; external and app namespaces retain
    /// their caller-selected groups.
    pub(crate) fn default_group(self) -> &'static str {
        if self == Self::Tests { "." } else { "normal" }
    }

    fn is_app(self) -> bool {
        matches!(self, Self::Apps | Self::ExternalApps(_))
    }
}

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
    source: CaseSource<'_>,
    workspace_root: &Path,
    group: &str,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<AxvisorQemuCase>> {
    let roots = group_dirs(source, workspace_root, group)?;
    let mut cases = Vec::new();
    for root in &roots {
        let found = test_qemu::discover_qemu_cases_allow_empty(
            root, arch, target, None, "Axvisor", "qemu",
        )?;
        let found = apply_boot_groups(root, found, arch)?;
        for mut case in found {
            if source.is_app() {
                case.name = relative_app_name(root, &case.case_dir)?;
                case.display_name = case.name.clone();
            }
            let plan =
                crate::axvisor::test::checks::runtime_check_references(&case.qemu_config_path)?;
            if !source.is_app() {
                if let Some(id) = &plan.id {
                    case.name = id.clone();
                    case.display_name = id.clone();
                } else if plan.checks.is_some() {
                    case.name = relative_app_name(root, &case.case_dir)?;
                    case.display_name = case.name.clone();
                }
            }
            cases.push(load_qemu_case(case)?);
        }
    }
    if let Some(selected) = selected_case {
        // Preserve the specific wrong-architecture diagnostic for legacy suites.
        if cases.is_empty() && !source.is_app() {
            for root in &roots {
                test_qemu::discover_qemu_cases_allow_empty(
                    root,
                    arch,
                    target,
                    Some(selected),
                    "Axvisor",
                    "qemu",
                )?;
            }
        }
        let groups = cases
            .iter()
            .map(|case| {
                (
                    case.case.name.as_str(),
                    case.case.qemu_config_path.as_path(),
                )
            })
            .collect::<Vec<_>>();
        let selection =
            crate::axvisor::test::checks::select_runtime_members(&groups, &[selected.to_string()])?;
        let mut selected_cases = Vec::new();
        for (index, members) in selection {
            let mut case = cases[index].clone();
            case.selected_checks = members;
            selected_cases.push(case);
        }
        return Ok(selected_cases);
    }
    Ok(cases)
}

/// Substitute explicitly declared groups for their members. No automatic batching.
fn apply_boot_groups(
    root: &Path,
    found: Vec<test_qemu::DiscoveredQemuCase>,
    arch: &str,
) -> anyhow::Result<Vec<test_qemu::DiscoveredQemuCase>> {
    let mut owned = BTreeSet::new();
    let mut groups = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type().is_file() || !super::boot_group::is_manifest(path) {
            continue;
        }
        let group = super::boot_group::read(path)?;
        let directory = path.parent().context("boot group has no directory")?;
        let members = group
            .cases
            .iter()
            .map(|member| directory.join(member).canonicalize())
            .collect::<Result<Vec<_>, _>>()?;
        let mut candidates = Vec::new();
        for case in &found {
            if members.contains(&case.qemu_config_path.canonicalize()?) {
                candidates.push(case);
            }
        }
        // A manifest for another architecture is irrelevant to this discovery pass.
        let Some(first) = candidates.first() else {
            let selected_arch = format!("qemu-{arch}");
            anyhow::ensure!(
                !group
                    .cases
                    .iter()
                    .any(|member| member.file_stem().is_some_and(|name| name
                        == selected_arch.as_str()
                        || name
                            .to_string_lossy()
                            .starts_with(&format!("{selected_arch}-")))),
                "boot group members have no discoverable build wrapper: {}",
                path.display()
            );
            continue;
        };
        anyhow::ensure!(
            candidates.len() == members.len(),
            "boot group has missing or mixed-architecture members: {}",
            path.display()
        );
        super::boot_group::load(path, None)?;
        for member in members {
            anyhow::ensure!(
                owned.insert(member),
                "runtime belongs to multiple boot groups: {}",
                path.display()
            );
        }
        for case in &found {
            let canonical = case.qemu_config_path.canonicalize()?;
            if case.case_dir.starts_with(directory) && !owned.contains(&canonical) {
                let refs = super::checks::runtime_check_references(&case.qemu_config_path)?;
                anyhow::ensure!(
                    refs.standalone,
                    "runtime missing from boot group; declare axbuild.standalone=true if \
                     intentional: {}",
                    case.qemu_config_path.display()
                );
            }
        }
        let mut case = (**first).clone();
        case.name = group.id.clone();
        case.display_name = group.id;
        case.case_dir = directory.to_path_buf();
        case.qemu_config_path = path.to_path_buf();
        groups.push(case);
    }
    for case in found {
        if !owned.contains(&case.qemu_config_path.canonicalize()?) {
            groups.push(case);
        }
    }
    groups.sort_by(|left, right| left.name.cmp(&right.name));
    let mut ids = BTreeSet::new();
    for case in &groups {
        let refs = super::checks::runtime_check_references(&case.qemu_config_path)?;
        anyhow::ensure!(
            ids.insert(refs.id.unwrap_or_else(|| case.name.clone())),
            "duplicate runtime ID under {}",
            root.display()
        );
    }
    Ok(groups)
}

/// Merge the listed QEMU cases of every suite root for one group.
///
/// Missing cases may be ignored while combining roots in one namespace;
/// malformed configurations still propagate.
pub(crate) fn list_all_qemu_cases_with_archs(
    source: CaseSource<'_>,
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<test_qemu::ListedQemuCase>> {
    let mut listed = std::collections::BTreeMap::<String, Vec<String>>::new();
    for arch in crate::context::supported_arches() {
        let (_, target) = resolve_axvisor_arch_and_target(Some(arch.to_string()), None)?;
        for case in discover_qemu_cases(source, workspace_root, group, arch, &target, None)? {
            let names =
                crate::axvisor::test::checks::runtime_check_names(&case.case.qemu_config_path)?;
            let matches = selected_case.is_none_or(|selected| {
                app_matches(&case.case.name, selected)
                    || names.iter().any(|name| {
                        name == selected || format!("{}/{name}", case.case.name) == selected
                    })
            });
            if matches {
                listed
                    .entry(case.case.name.clone())
                    .or_default()
                    .push(arch.to_string());
                for name in names {
                    let qualified = format!("{}/{name}", case.case.name);
                    if selected_case.is_none_or(|selected| {
                        selected == name || app_matches(&qualified, selected)
                    }) {
                        listed.entry(qualified).or_default().push(arch.to_string());
                    }
                }
            }
        }
    }
    if listed.is_empty() && selected_case.is_some() {
        bail!(
            "unknown Axvisor test case or app {:?} under {}",
            selected_case,
            suite_roots_label(source, workspace_root)
        );
    }
    Ok(listed
        .into_iter()
        .map(|(name, archs)| test_qemu::ListedQemuCase { name, archs })
        .collect())
}

pub(crate) fn list_all_qemu_cases(
    source: CaseSource<'_>,
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    Ok(
        list_all_qemu_cases_with_archs(source, workspace_root, group, selected_case)?
            .into_iter()
            .map(|case| case.name)
            .collect(),
    )
}

fn load_qemu_case(case: test_qemu::DiscoveredQemuCase) -> anyhow::Result<AxvisorQemuCase> {
    let build_group = case.build_group;
    let build_config_path = case.build_config_path;
    let runtime_path = if super::boot_group::is_manifest(&case.qemu_config_path) {
        let group = super::boot_group::read(&case.qemu_config_path)?;
        case.case_dir.join(&group.cases[0])
    } else {
        case.qemu_config_path.clone()
    };
    // The manifest owns membership, while the complete member still owns the
    // generic test contract (including the prohibition on persistent writes).
    let mut test_case = test_qemu::load_test_qemu_case_fields(
        case.display_name,
        case.name,
        case.case_dir,
        runtime_path,
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
    test_case.qemu_config_path = case.qemu_config_path;
    Ok(AxvisorQemuCase {
        selected_checks: None,
        case: test_case,
        build_group,
        build_config_path,
    })
}

pub(crate) fn discover_board_test_groups(
    source: CaseSource<'_>,
    workspace_root: &Path,
    group: &str,
    selected_cases: &[String],
    boards: &[String],
) -> anyhow::Result<Vec<BoardTestGroup>> {
    let roots = board_test_group_roots(source, workspace_root, group)?;
    let mut groups = Vec::new();
    for root in &roots {
        let mut found = collect_board_test_groups(workspace_root, root)?;
        if source.is_app() {
            for case in &mut found {
                case.name = relative_app_name(
                    root,
                    case.board_test_config_path
                        .parent()
                        .context("board app has no directory")?,
                )?;
            }
        }
        groups.extend(found);
    }
    let searched = roots
        .iter()
        .map(|dir| dir.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    // Restrict the execution environment before resolving a short member name.
    let groups = board_test::filter_board_test_groups_by_board_names(
        groups,
        &[],
        boards,
        "axvisor",
        || format!("no Axvisor board test groups found under {searched}"),
    )?;
    let descriptors = groups
        .iter()
        .map(|case| (case.name.as_str(), case.board_test_config_path.as_path()))
        .collect::<Vec<_>>();
    let selection =
        crate::axvisor::test::checks::select_runtime_members(&descriptors, selected_cases)?;
    Ok(selection
        .into_iter()
        .map(|(index, members)| {
            let mut case = groups[index].clone();
            case.selected_checks = members;
            case
        })
        .collect())
}

/// Select namespace roots and retain the existing default-group benchmark
/// entry for test commands. Applications never cross into another namespace.
fn board_test_group_roots(
    source: CaseSource<'_>,
    workspace_root: &Path,
    group: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    validate_group_name(group)?;
    let mut roots = Vec::new();
    for suite_root in source.suite_roots(workspace_root) {
        let group_dir = suite_root.join(group);
        if group_dir.is_dir() {
            roots.push(group_dir);
        }
    }
    if source == CaseSource::Tests && group == source.default_group() {
        let benchmark_suite_dir = benchmark_suite_root(workspace_root);
        if benchmark_suite_dir.is_dir() {
            roots.push(benchmark_suite_dir);
        }
    }
    if roots.is_empty() {
        bail!(
            "unsupported Axvisor test group `{group}`. Supported groups are: {}",
            supported_group_names(source, workspace_root)?
        );
    }
    Ok(roots)
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
        let refs =
            crate::axvisor::test::checks::runtime_check_references(&info.board_test_config_path)?;
        let name = if let Some(id) = refs.id {
            id
        } else if refs.checks.is_some() {
            relative_app_name(
                test_suite_dir,
                info.board_test_config_path
                    .parent()
                    .context("board configuration has no directory")?,
            )?
        } else {
            info.name
        };
        groups.push(BoardTestGroup {
            selected_checks: None,
            name,
            board_name: info.board_name,
            build_config,
            board_test_config_path: info.board_test_config_path,
        });
    }

    Ok(groups)
}

pub(crate) fn discover_uboot_test_group(
    workspace_root: &Path,
    board: &str,
    guest: &str,
) -> anyhow::Result<BoardTestGroup> {
    let board_name = format!("{board}-{guest}");
    let selected_boards = vec![board_name];
    let mut groups = discover_board_test_groups(
        CaseSource::Tests,
        workspace_root,
        CaseSource::Tests.default_group(),
        &[],
        &selected_boards,
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
    let mut runtime: toml::Table = toml::from_str(&std::fs::read_to_string(path)?)?;
    runtime.remove("axbuild");
    toml::Value::Table(runtime)
        .try_into::<ostool::board::config::BoardRunConfig>()
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

pub(crate) fn ensure_file_exists(path: &Path, label: &str) -> anyhow::Result<()> {
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
pub(crate) fn group_dirs(
    source: CaseSource<'_>,
    workspace_root: &Path,
    group: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    validate_group_name(group)?;
    let dirs = source
        .suite_roots(workspace_root)
        .into_iter()
        .map(|root| root.join(group))
        .filter(|dir| dir.is_dir())
        .collect::<Vec<_>>();
    if dirs.is_empty() {
        bail!(
            "unsupported Axvisor test group `{group}`. Supported groups are: {}",
            supported_group_names(source, workspace_root)?
        );
    }
    Ok(dirs)
}

/// Human-readable list of the AxVisor suite roots for error messages.
pub(crate) fn suite_roots_label(source: CaseSource<'_>, workspace_root: &Path) -> String {
    source
        .suite_roots(workspace_root)
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn discover_test_group_names(
    source: CaseSource<'_>,
    workspace_root: &Path,
) -> anyhow::Result<Vec<String>> {
    if source == CaseSource::Tests {
        return Ok(
            if source
                .suite_roots(workspace_root)
                .iter()
                .any(|root| root.is_dir())
            {
                vec![source.default_group().to_string()]
            } else {
                Vec::new()
            },
        );
    }
    let mut groups = BTreeSet::new();
    for root in source.suite_roots(workspace_root) {
        groups.extend(test_suite::discover_group_names_in_root(&root)?);
    }
    Ok(groups.into_iter().collect())
}

pub(crate) fn supported_group_names(
    source: CaseSource<'_>,
    workspace_root: &Path,
) -> anyhow::Result<String> {
    let groups = discover_test_group_names(source, workspace_root)?;
    Ok(if groups.is_empty() {
        "<none>".to_string()
    } else {
        groups.join(", ")
    })
}

fn relative_app_name(root: &Path, directory: &Path) -> anyhow::Result<String> {
    Ok(directory
        .strip_prefix(root)?
        .to_string_lossy()
        .replace('\\', "/"))
}

fn app_matches(name: &str, selected: &str) -> bool {
    name == selected
        || name
            .strip_prefix(selected)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

/// A group is the namespace root (`.`) or one child, never another suite.
fn validate_group_name(group: &str) -> anyhow::Result<()> {
    let mut components = Path::new(group).components();
    anyhow::ensure!(
        matches!(
            components.next(),
            Some(std::path::Component::Normal(_) | std::path::Component::CurDir)
        ) && components.next().is_none(),
        "invalid AxVisor group name `{group}`"
    );
    Ok(())
}
