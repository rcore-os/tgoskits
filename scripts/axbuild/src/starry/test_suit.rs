use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, bail};

use super::board;
use crate::context::{
    arch_for_target_checked, resolve_starry_arch_and_target, validate_supported_target,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StarryTestGroup {
    Normal,
    Stress,
}

impl StarryTestGroup {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Stress => "stress",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuCase {
    pub(crate) name: String,
    pub(crate) case_dir: PathBuf,
    pub(crate) qemu_config_path: PathBuf,
    pub(crate) build_config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StarryQemuCaseOutcome {
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuCaseReport {
    pub(crate) name: String,
    pub(crate) outcome: StarryQemuCaseOutcome,
    pub(crate) duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuRunReport {
    pub(crate) group: StarryTestGroup,
    pub(crate) cases: Vec<StarryQemuCaseReport>,
    pub(crate) total_duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryBoardTestGroup {
    pub(crate) name: String,
    pub(crate) board_name: String,
    pub(crate) arch: String,
    pub(crate) target: String,
    pub(crate) build_config_path: PathBuf,
    pub(crate) board_test_config_path: PathBuf,
}

pub(crate) fn parse_test_target(
    workspace_root: &Path,
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    let supported_targets = board::board_default_list(workspace_root)?
        .into_iter()
        .filter(|board| board.name.starts_with("qemu-"))
        .map(|board| board.target)
        .collect::<Vec<_>>();

    let supported_target_refs = supported_targets
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let supported_arches = supported_targets
        .iter()
        .map(|target| arch_for_target_checked(target))
        .collect::<anyhow::Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();

    let (arch, target) = resolve_starry_arch_and_target(arch.clone(), target.clone())?;
    validate_supported_target(&arch, "starry qemu tests", "arch values", &supported_arches)?;
    validate_supported_target(
        &target,
        "starry qemu tests",
        "targets",
        &supported_target_refs,
    )?;
    Ok((arch, target))
}

pub(crate) fn discover_qemu_cases(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    group: StarryTestGroup,
) -> anyhow::Result<Vec<StarryQemuCase>> {
    let test_suite_dir = test_suite_dir(workspace_root, group);
    let config_name = qemu_config_name(arch);

    if let Some(case_name) = selected_case {
        let case_dir = test_suite_dir.join(case_name);
        if !case_dir.is_dir() {
            bail!(
                "unknown Starry {} test case `{case_name}` in {}; available cases are discovered \
                 from direct subdirectories",
                group.as_str(),
                test_suite_dir.display()
            );
        }

        let qemu_config_path = case_dir.join(&config_name);
        if !qemu_config_path.is_file() {
            bail!(
                "Starry test case `{case_name}` does not provide `{}`",
                qemu_config_path.display()
            );
        }

        let build_config_path = resolve_case_build_config_path(&case_dir, arch, target);
        return Ok(vec![StarryQemuCase {
            name: case_name.to_string(),
            case_dir,
            qemu_config_path,
            build_config_path,
        }]);
    }

    let mut cases = fs::read_dir(&test_suite_dir)
        .with_context(|| format!("failed to read {}", test_suite_dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }

            let name = entry.file_name().into_string().ok()?;
            let qemu_config_path = path.join(&config_name);
            if qemu_config_path.is_file() {
                Some(StarryQemuCase {
                    name,
                    build_config_path: resolve_case_build_config_path(&path, arch, target),
                    case_dir: path,
                    qemu_config_path,
                })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    cases.sort_by(|left, right| left.name.cmp(&right.name));

    if cases.is_empty() {
        bail!(
            "no Starry {} qemu test cases for arch `{arch}` found under {}",
            group.as_str(),
            test_suite_dir.display()
        );
    }

    Ok(cases)
}

pub(crate) fn finalize_qemu_case_run(report: &StarryQemuRunReport) -> anyhow::Result<()> {
    println!("{}", render_qemu_case_summary(report));

    let failed = report
        .cases
        .iter()
        .filter(|case| case.outcome == StarryQemuCaseOutcome::Failed)
        .map(|case| case.name.clone())
        .collect::<Vec<_>>();

    if failed.is_empty() {
        Ok(())
    } else {
        bail!(
            "starry {} qemu tests failed for {} case(s): {}",
            report.group.as_str(),
            failed.len(),
            failed.join(", ")
        )
    }
}

pub(crate) fn discover_board_test_groups(
    workspace_root: &Path,
    selected_group: Option<&str>,
) -> anyhow::Result<Vec<StarryBoardTestGroup>> {
    let test_suite_dir = test_suite_dir(workspace_root, StarryTestGroup::Normal);
    let mut groups = collect_board_test_groups(workspace_root, &test_suite_dir)?;
    groups.sort_by(|left, right| left.name.cmp(&right.name));

    if let Some(group_name) = selected_group {
        return groups
            .into_iter()
            .find(|group| group.name == group_name)
            .map(|group| vec![group])
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "unsupported Starry board test group `{group_name}`. Supported groups are: {}",
                    supported_board_test_group_names(workspace_root)
                        .unwrap_or_else(|_| "<none>".to_string())
                )
            });
    }

    if groups.is_empty() {
        bail!(
            "no Starry board test groups found under {}",
            test_suite_dir.display()
        );
    }

    Ok(groups)
}

pub(crate) fn finalize_board_test_run(failed: &[String]) -> anyhow::Result<()> {
    if failed.is_empty() {
        println!("all starry board test groups passed");
        Ok(())
    } else {
        bail!(
            "starry board tests failed for {} group(s): {}",
            failed.len(),
            failed.join(", ")
        )
    }
}

fn test_suite_dir(workspace_root: &Path, group: StarryTestGroup) -> PathBuf {
    workspace_root
        .join("test-suit")
        .join("starryos")
        .join(group.as_str())
}

fn qemu_config_name(arch: &str) -> String {
    format!("qemu-{arch}.toml")
}

pub(crate) fn resolve_case_build_config_path(
    case_dir: &Path,
    arch: &str,
    target: &str,
) -> Option<PathBuf> {
    let bare_target = case_dir.join(format!("build-{target}.toml"));
    if bare_target.is_file() {
        return Some(bare_target);
    }

    let dotted_target = case_dir.join(format!(".build-{target}.toml"));
    if dotted_target.is_file() {
        return Some(dotted_target);
    }

    let bare_arch = case_dir.join(format!("build-{arch}.toml"));
    if bare_arch.is_file() {
        return Some(bare_arch);
    }

    let dotted_arch = case_dir.join(format!(".build-{arch}.toml"));
    if dotted_arch.is_file() {
        return Some(dotted_arch);
    }

    None
}

fn render_qemu_case_summary(report: &StarryQemuRunReport) -> String {
    let passed = report
        .cases
        .iter()
        .filter(|case| case.outcome == StarryQemuCaseOutcome::Passed)
        .collect::<Vec<_>>();
    let failed = report
        .cases
        .iter()
        .filter(|case| case.outcome == StarryQemuCaseOutcome::Failed)
        .collect::<Vec<_>>();

    let mut lines = Vec::new();
    lines.push(format!("starry {} qemu summary:", report.group.as_str()));
    lines.push(format!("passed ({}):", passed.len()));
    if passed.is_empty() {
        lines.push("  <none>".to_string());
    } else {
        lines.extend(
            passed
                .iter()
                .map(|case| format!("  {} ({})", case.name, format_duration(case.duration))),
        );
    }

    lines.push(format!("failed ({}):", failed.len()));
    if failed.is_empty() {
        lines.push("  <none>".to_string());
    } else {
        lines.extend(
            failed
                .iter()
                .map(|case| format!("  {} ({})", case.name, format_duration(case.duration))),
        );
    }

    lines.push(format!("total: {}", format_duration(report.total_duration)));
    lines.join("\n")
}

fn format_duration(duration: Duration) -> String {
    format!("{:.2}s", duration.as_secs_f64())
}

fn collect_board_test_groups(
    workspace_root: &Path,
    test_suite_dir: &Path,
) -> anyhow::Result<Vec<StarryBoardTestGroup>> {
    let mut groups = Vec::new();
    for entry in fs::read_dir(test_suite_dir)
        .with_context(|| format!("failed to read {}", test_suite_dir.display()))?
    {
        let entry = entry?;
        let case_dir = entry.path();
        if !case_dir.is_dir() {
            continue;
        }

        let case_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };

        for config_entry in fs::read_dir(&case_dir)
            .with_context(|| format!("failed to read {}", case_dir.display()))?
        {
            let config_entry = config_entry?;
            let config_path = config_entry.path();
            if !config_path.is_file() || config_path.extension().is_none_or(|ext| ext != "toml") {
                continue;
            }

            let Some(stem) = config_path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Some(board_name) = stem.strip_prefix("board-") else {
                continue;
            };

            let default_build_config_path = workspace_root
                .join("os/StarryOS/configs/board")
                .join(format!("{board_name}.toml"));
            if !default_build_config_path.is_file() {
                bail!(
                    "Starry board test group `{}-{board_name}` maps to missing build config `{}`",
                    case_name,
                    default_build_config_path.display()
                );
            }

            let board_file =
                board::load_board_file(&default_build_config_path).with_context(|| {
                    format!(
                        "failed to load mapped Starry build config for board test group \
                         `{}-{board_name}`",
                        case_name
                    )
                })?;
            let build_config_path = resolve_case_build_config_path(
                &case_dir,
                arch_for_target_checked(&board_file.target)?,
                &board_file.target,
            )
            .unwrap_or(default_build_config_path);
            groups.push(StarryBoardTestGroup {
                name: format!("{case_name}-{board_name}"),
                board_name: board_name.to_string(),
                arch: arch_for_target_checked(&board_file.target)?.to_string(),
                target: board_file.target,
                build_config_path,
                board_test_config_path: config_path,
            });
        }
    }

    Ok(groups)
}

fn supported_board_test_group_names(workspace_root: &Path) -> anyhow::Result<String> {
    let test_suite_dir = test_suite_dir(workspace_root, StarryTestGroup::Normal);
    let mut groups = collect_board_test_groups(workspace_root, &test_suite_dir)?
        .into_iter()
        .map(|group| group.name)
        .collect::<Vec<_>>();
    groups.sort();
    Ok(groups.join(", "))
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use tempfile::tempdir;

    use super::*;

    fn write_board_build_config(root: &Path, board_name: &str, target: &str) {
        let path = root
            .join("os/StarryOS/configs/board")
            .join(format!("{board_name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!(
                "target = \"{target}\"\nenv = {{}}\nfeatures = [\"qemu\"]\nlog = \
                 \"Info\"\nplat_dyn = false\n"
            ),
        )
        .unwrap();
    }

    fn write_board_test_config(root: &Path, case_name: &str, board_name: &str) -> PathBuf {
        let path = root
            .join("test-suit/starryos/normal")
            .join(case_name)
            .join(format!("board-{board_name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \
             \"orangepi@orangepi5plus:~\"\nshell_init_cmd = \"pwd && echo 'test \
             pass'\"\nsuccess_regex = [\"(?m)^test pass\\\\s*$\"]\nfail_regex = []\ntimeout = \
             300\n",
        )
        .unwrap();
        path
    }

    #[test]
    fn discovers_board_test_group_and_build_mapping() {
        let root = tempdir().unwrap();
        write_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        let board_test_config = write_board_test_config(root.path(), "smoke", "orangepi-5-plus");

        let groups = discover_board_test_groups(root.path(), None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke-orangepi-5-plus");
        assert_eq!(groups[0].board_name, "orangepi-5-plus");
        assert_eq!(groups[0].arch, "aarch64");
        assert_eq!(groups[0].target, "aarch64-unknown-none-softfloat");
        assert_eq!(
            groups[0].build_config_path,
            root.path()
                .join("os/StarryOS/configs/board/orangepi-5-plus.toml")
        );
        assert_eq!(groups[0].board_test_config_path, board_test_config);
    }

    #[test]
    fn filters_board_test_group_by_name() {
        let root = tempdir().unwrap();
        write_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_board_build_config(root.path(), "vision-five2", "riscv64gc-unknown-none-elf");
        write_board_test_config(root.path(), "smoke", "orangepi-5-plus");
        write_board_test_config(root.path(), "smoke", "vision-five2");

        let groups =
            discover_board_test_groups(root.path(), Some("smoke-orangepi-5-plus")).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke-orangepi-5-plus");
    }

    #[test]
    fn rejects_missing_mapped_board_build_config() {
        let root = tempdir().unwrap();
        write_board_test_config(root.path(), "smoke", "orangepi-5-plus");

        let err = discover_board_test_groups(root.path(), None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("smoke-orangepi-5-plus"));
        assert!(err.contains("os/StarryOS/configs/board/orangepi-5-plus.toml"));
    }

    fn write_qemu_test_config(root: &Path, group: StarryTestGroup, case_name: &str, arch: &str) {
        let path = root
            .join("test-suit/starryos")
            .join(group.as_str())
            .join(case_name)
            .join(format!("qemu-{arch}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "timeout = 1\n").unwrap();
    }

    fn write_case_build_config(root: &Path, relative_dir: &str, name: &str) -> PathBuf {
        let path = root.join(relative_dir).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "features = [\"qemu\"]\nlog = \"Info\"\n").unwrap();
        path
    }

    #[test]
    fn discovers_only_cases_with_matching_qemu_config() {
        let root = tempdir().unwrap();
        write_qemu_test_config(root.path(), StarryTestGroup::Normal, "smoke", "x86_64");
        fs::create_dir_all(root.path().join("test-suit/starryos/normal/usb")).unwrap();

        let cases = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            None,
            StarryTestGroup::Normal,
        )
        .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].name, "smoke");
        assert_eq!(cases[0].build_config_path, None);
        assert_eq!(
            cases[0].case_dir,
            root.path().join("test-suit/starryos/normal/smoke")
        );
    }

    #[test]
    fn selected_case_requires_matching_qemu_config() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("test-suit/starryos/normal/usb")).unwrap();

        let err = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("usb"),
            StarryTestGroup::Normal,
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("does not provide"));
        assert!(err.contains("qemu-x86_64.toml"));
    }

    #[test]
    fn qemu_case_prefers_target_build_config() {
        let root = tempdir().unwrap();
        write_qemu_test_config(root.path(), StarryTestGroup::Normal, "smoke", "aarch64");
        let build = write_case_build_config(
            root.path(),
            "test-suit/starryos/normal/smoke",
            "build-aarch64-unknown-none-softfloat.toml",
        );

        let cases = discover_qemu_cases(
            root.path(),
            "aarch64",
            "aarch64-unknown-none-softfloat",
            None,
            StarryTestGroup::Normal,
        )
        .unwrap();

        assert_eq!(cases[0].build_config_path, Some(build));
    }

    #[test]
    fn qemu_case_uses_dotted_target_build_config_when_bare_missing() {
        let root = tempdir().unwrap();
        write_qemu_test_config(root.path(), StarryTestGroup::Normal, "smoke", "aarch64");
        let build = write_case_build_config(
            root.path(),
            "test-suit/starryos/normal/smoke",
            ".build-aarch64-unknown-none-softfloat.toml",
        );

        let cases = discover_qemu_cases(
            root.path(),
            "aarch64",
            "aarch64-unknown-none-softfloat",
            None,
            StarryTestGroup::Normal,
        )
        .unwrap();

        assert_eq!(cases[0].build_config_path, Some(build));
    }

    #[test]
    fn qemu_case_falls_back_to_arch_build_config() {
        let root = tempdir().unwrap();
        write_qemu_test_config(root.path(), StarryTestGroup::Normal, "smoke", "aarch64");
        let build = write_case_build_config(
            root.path(),
            "test-suit/starryos/normal/smoke",
            "build-aarch64.toml",
        );

        let cases = discover_qemu_cases(
            root.path(),
            "aarch64",
            "aarch64-unknown-none-softfloat",
            None,
            StarryTestGroup::Normal,
        )
        .unwrap();

        assert_eq!(cases[0].build_config_path, Some(build));
    }

    #[test]
    fn qemu_case_prefers_target_build_config_over_arch_build_config() {
        let root = tempdir().unwrap();
        write_qemu_test_config(root.path(), StarryTestGroup::Normal, "smoke", "aarch64");
        let target_build = write_case_build_config(
            root.path(),
            "test-suit/starryos/normal/smoke",
            "build-aarch64-unknown-none-softfloat.toml",
        );
        write_case_build_config(
            root.path(),
            "test-suit/starryos/normal/smoke",
            "build-aarch64.toml",
        );

        let cases = discover_qemu_cases(
            root.path(),
            "aarch64",
            "aarch64-unknown-none-softfloat",
            None,
            StarryTestGroup::Normal,
        )
        .unwrap();

        assert_eq!(cases[0].build_config_path, Some(target_build));
    }

    #[test]
    fn board_test_group_prefers_case_target_build_config() {
        let root = tempdir().unwrap();
        write_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_board_test_config(root.path(), "smoke", "orangepi-5-plus");
        let build = write_case_build_config(
            root.path(),
            "test-suit/starryos/normal/smoke",
            "build-aarch64-unknown-none-softfloat.toml",
        );

        let groups = discover_board_test_groups(root.path(), None).unwrap();

        assert_eq!(groups[0].build_config_path, build);
    }

    #[test]
    fn board_test_group_falls_back_to_mapped_board_build_config() {
        let root = tempdir().unwrap();
        write_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_board_test_config(root.path(), "smoke", "orangepi-5-plus");

        let groups = discover_board_test_groups(root.path(), None).unwrap();

        assert_eq!(
            groups[0].build_config_path,
            root.path()
                .join("os/StarryOS/configs/board/orangepi-5-plus.toml")
        );
    }

    #[test]
    fn qemu_summary_lists_passed_and_failed_cases() {
        let report = StarryQemuRunReport {
            group: StarryTestGroup::Normal,
            cases: vec![
                StarryQemuCaseReport {
                    name: "smoke".to_string(),
                    outcome: StarryQemuCaseOutcome::Passed,
                    duration: Duration::from_millis(500),
                },
                StarryQemuCaseReport {
                    name: "usb".to_string(),
                    outcome: StarryQemuCaseOutcome::Failed,
                    duration: Duration::from_secs(2),
                },
            ],
            total_duration: Duration::from_secs(3),
        };

        let summary = render_qemu_case_summary(&report);

        assert!(summary.contains("starry normal qemu summary:"));
        assert!(summary.contains("smoke (0.50s)"));
        assert!(summary.contains("usb (2.00s)"));
        assert!(summary.contains("total: 3.00s"));
    }
}
