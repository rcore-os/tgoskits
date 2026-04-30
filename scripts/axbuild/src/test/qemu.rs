use std::path::Path;

use anyhow::bail;
use ostool::run::qemu::QemuConfig;

use crate::{context::validate_supported_target, test::case::TestQemuCase};

const TIMEOUT_SCALE_ENV: &str = "AXBUILD_TEST_TIMEOUT_SCALE";

pub(crate) fn qemu_config_name(arch: &str) -> String {
    format!("qemu-{arch}.toml")
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
