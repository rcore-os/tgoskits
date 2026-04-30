use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use ostool::run::qemu::QemuConfig;

use crate::{context::validate_supported_target, test::case::TestQemuCase};

const TIMEOUT_SCALE_ENV: &str = "AXBUILD_TEST_TIMEOUT_SCALE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestBuildGroup {
    pub(crate) name: String,
    pub(crate) dir: PathBuf,
    pub(crate) build_config_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveredQemuCase {
    pub(crate) name: String,
    pub(crate) display_name: String,
    pub(crate) case_dir: PathBuf,
    pub(crate) qemu_config_path: PathBuf,
    pub(crate) build_group: String,
    pub(crate) build_config_path: PathBuf,
}

pub(crate) struct QemuCaseGroup<'a, T> {
    pub(crate) build_group: &'a str,
    pub(crate) build_config_path: &'a Path,
    pub(crate) cases: Vec<&'a T>,
}

pub(crate) trait BuildConfigRef {
    fn build_group(&self) -> &str;
    fn build_config_path(&self) -> &Path;
}

pub(crate) fn qemu_config_name(arch: &str) -> String {
    format!("qemu-{arch}.toml")
}

pub(crate) fn resolve_build_config_path(dir: &Path, arch: &str, target: &str) -> Option<PathBuf> {
    let bare_target = dir.join(format!("build-{target}.toml"));
    if bare_target.is_file() {
        return Some(bare_target);
    }

    let dotted_target = dir.join(format!(".build-{target}.toml"));
    if dotted_target.is_file() {
        return Some(dotted_target);
    }

    let bare_arch = dir.join(format!("build-{arch}.toml"));
    if bare_arch.is_file() {
        return Some(bare_arch);
    }

    let dotted_arch = dir.join(format!(".build-{arch}.toml"));
    if dotted_arch.is_file() {
        return Some(dotted_arch);
    }

    None
}

pub(crate) fn discover_build_groups(
    test_suite_dir: &Path,
    arch: &str,
    target: &str,
    suite_name: &str,
    group_kind: &str,
) -> anyhow::Result<Vec<TestBuildGroup>> {
    let mut groups = Vec::new();
    for entry in fs::read_dir(test_suite_dir)
        .with_context(|| format!("failed to read {}", test_suite_dir.display()))?
    {
        let entry = entry?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(build_config_path) = resolve_build_config_path(&dir, arch, target) else {
            continue;
        };
        groups.push(TestBuildGroup {
            name,
            dir,
            build_config_path,
        });
    }
    groups.sort_by(|left, right| left.name.cmp(&right.name));

    if groups.is_empty() {
        bail!(
            "no {suite_name} {group_kind} build groups for arch `{arch}` target `{target}` found \
             under {}; expected build-{target}.toml or build-{arch}.toml in <build_group> \
             directories",
            test_suite_dir.display()
        );
    }

    Ok(groups)
}

pub(crate) fn discover_qemu_cases(
    test_suite_dir: &Path,
    build_groups: &[TestBuildGroup],
    arch: &str,
    selected_case: Option<&str>,
    suite_name: &str,
    group_label: &str,
) -> anyhow::Result<Vec<DiscoveredQemuCase>> {
    let config_name = qemu_config_name(arch);
    let mut cases = Vec::new();
    let mut selected_case_dirs_without_config = Vec::new();

    for build_group in build_groups {
        if let Some(case_name) = selected_case {
            let case_dir = build_group.dir.join(case_name);
            if case_dir.is_dir() {
                let qemu_config_path = case_dir.join(&config_name);
                if qemu_config_path.is_file() {
                    cases.push(discovered_qemu_case(
                        build_group,
                        case_name.to_string(),
                        case_dir,
                        qemu_config_path,
                    ));
                } else {
                    selected_case_dirs_without_config
                        .push((build_group.name.clone(), qemu_config_path));
                }
            }
            continue;
        }

        for entry in fs::read_dir(&build_group.dir)
            .with_context(|| format!("failed to read {}", build_group.dir.display()))?
        {
            let entry = entry?;
            let case_dir = entry.path();
            if !case_dir.is_dir() {
                continue;
            }
            let Ok(case_name) = entry.file_name().into_string() else {
                continue;
            };
            let qemu_config_path = case_dir.join(&config_name);
            if qemu_config_path.is_file() {
                cases.push(discovered_qemu_case(
                    build_group,
                    case_name,
                    case_dir,
                    qemu_config_path,
                ));
            }
        }
    }

    cases.sort_by(|left, right| left.display_name.cmp(&right.display_name));

    if cases.is_empty() {
        if let Some(case_name) = selected_case {
            if !selected_case_dirs_without_config.is_empty() {
                let searched = selected_case_dirs_without_config
                    .iter()
                    .map(|(build_group, path)| format!("{build_group}: {}", path.display()))
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "{suite_name} {group_label} test case `{case_name}` exists under matching \
                     build group(s), but none provide `{config_name}` for arch `{arch}`: \
                     {searched}"
                );
            }
            bail!(
                "unknown {suite_name} {group_label} test case `{case_name}` for arch `{arch}` \
                 under {}; cases are discovered from <build_group>/<case> directories with \
                 matching `{config_name}`",
                test_suite_dir.display()
            );
        }
        bail!(
            "no {suite_name} {group_label} qemu test cases for arch `{arch}` found under {}",
            test_suite_dir.display()
        );
    }

    Ok(cases)
}

fn discovered_qemu_case(
    build_group: &TestBuildGroup,
    name: String,
    case_dir: PathBuf,
    qemu_config_path: PathBuf,
) -> DiscoveredQemuCase {
    DiscoveredQemuCase {
        display_name: format!("{}/{}", build_group.name, name),
        name,
        case_dir,
        qemu_config_path,
        build_group: build_group.name.clone(),
        build_config_path: build_group.build_config_path.clone(),
    }
}

pub(crate) fn group_cases_by_build_config<T: BuildConfigRef>(
    cases: &[T],
) -> Vec<QemuCaseGroup<'_, T>> {
    let mut groups: Vec<QemuCaseGroup<'_, T>> = Vec::new();
    for case in cases {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.build_config_path == case.build_config_path())
        {
            group.cases.push(case);
        } else {
            groups.push(QemuCaseGroup {
                build_group: case.build_group(),
                build_config_path: case.build_config_path(),
                cases: vec![case],
            });
        }
    }

    groups
}

pub(crate) fn normalize_qemu_test_commands<I, S>(
    qemu_config_path: &Path,
    commands: I,
    suite_name: &str,
) -> anyhow::Result<Vec<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut test_commands = Vec::new();
    for command in commands {
        let command = command.as_ref().trim().to_string();
        if command.is_empty() {
            bail!(
                "{suite_name} grouped qemu case `{}` contains an empty test command",
                qemu_config_path.display()
            );
        }
        test_commands.push(command);
    }
    Ok(test_commands)
}

pub(crate) fn validate_grouped_qemu_commands(
    qemu: &QemuConfig,
    case: &TestQemuCase,
    suite_name: &str,
) -> anyhow::Result<()> {
    let shell_init_cmd_set = qemu
        .shell_init_cmd
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if shell_init_cmd_set && !case.test_commands.is_empty() {
        bail!(
            "{suite_name} grouped qemu case `{}` cannot define both `shell_init_cmd` and \
             `test_commands`",
            case.qemu_config_path.display()
        );
    }
    Ok(())
}

pub(crate) fn apply_smp_qemu_arg(qemu: &mut QemuConfig, smp: Option<usize>) {
    let Some(cpu_num) = smp else {
        return;
    };

    if let Some(index) = qemu.args.iter().position(|arg| arg == "-smp")
        && let Some(value) = qemu.args.get_mut(index + 1)
    {
        *value = cpu_num.to_string();
        return;
    }

    qemu.args.push("-smp".to_string());
    qemu.args.push(cpu_num.to_string());
}

pub(crate) fn apply_timeout_scale(qemu: &mut QemuConfig) {
    let Some(timeout) = qemu.timeout else {
        return;
    };
    if timeout == 0 {
        return;
    }

    let scale = match std::env::var(TIMEOUT_SCALE_ENV) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(scale) if scale > 1 => scale,
            Ok(_) | Err(_) => {
                eprintln!(
                    "warning: ignoring invalid {TIMEOUT_SCALE_ENV} value `{}`; expected integer > \
                     1",
                    value.trim()
                );
                return;
            }
        },
        Err(_) => return,
    };

    qemu.timeout = timeout.checked_mul(scale).or(Some(u64::MAX));
}

pub(crate) fn qemu_timeout_summary(qemu: &QemuConfig) -> String {
    match qemu.timeout {
        Some(0) | None => "disabled".to_string(),
        Some(timeout) => format!("{timeout}s"),
    }
}

pub(crate) fn parse_test_target(
    arch: &Option<String>,
    target: &Option<String>,
    suite_name: &str,
    supported_arches: &[&str],
    supported_targets: &[&str],
    resolve_arch_and_target: impl FnOnce(
        Option<String>,
        Option<String>,
    ) -> anyhow::Result<(String, String)>,
) -> anyhow::Result<(String, String)> {
    let (arch, target) = resolve_arch_and_target(arch.clone(), target.clone())?;
    validate_supported_target(&arch, suite_name, "arch values", supported_arches)?;
    validate_supported_target(&target, suite_name, "targets", supported_targets)?;
    Ok((arch, target))
}

pub(crate) fn finalize_qemu_test_run(
    suite_name: &str,
    unit: &str,
    failed: &[String],
) -> anyhow::Result<()> {
    if failed.is_empty() {
        println!("all {} qemu tests passed", suite_name);
        Ok(())
    } else {
        bail!(
            "{} qemu tests failed for {} {}(s): {}",
            suite_name,
            failed.len(),
            unit,
            failed.join(", ")
        )
    }
}

pub(crate) fn unsupported_uboot_test_command(os: &str) -> anyhow::Result<()> {
    bail!(
        "{os} does not support `test uboot` yet; only axvisor currently implements a U-Boot test \
         suite"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_failure_summary_is_aggregated() {
        let err = finalize_qemu_test_run(
            "arceos",
            "package",
            &["pkg-b".to_string(), "pkg-c".to_string()],
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("arceos qemu tests failed for 2 package(s): pkg-b, pkg-c")
        );
    }

    #[test]
    fn unsupported_uboot_error_is_explicit() {
        let err = unsupported_uboot_test_command("arceos").unwrap_err();

        assert!(
            err.to_string()
                .contains("arceos does not support `test uboot` yet")
        );
    }
}
