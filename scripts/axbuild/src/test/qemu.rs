use anyhow::bail;
use ostool::run::qemu::QemuConfig;

use crate::context::validate_supported_target;

const TIMEOUT_SCALE_ENV: &str = "AXBUILD_TEST_TIMEOUT_SCALE";

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

pub(crate) fn apply_memory_qemu_arg(qemu: &mut QemuConfig, bytes: Option<u64>) {
    let Some(bytes) = bytes else {
        return;
    };
    let value = qemu_memory_size_arg(bytes);

    if let Some(index) = qemu.args.iter().position(|arg| arg == "-m")
        && let Some(existing) = qemu.args.get_mut(index + 1)
    {
        *existing = value;
        return;
    }

    qemu.args.push("-m".to_string());
    qemu.args.push(value);
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

pub(crate) fn smp_from_qemu_arg(qemu: &QemuConfig) -> Option<usize> {
    let index = qemu.args.iter().position(|arg| arg == "-smp")?;
    let value = qemu.args.get(index + 1)?;
    parse_smp_qemu_value(value)
}

pub(crate) fn memory_size_from_qemu_arg(qemu: &QemuConfig) -> anyhow::Result<Option<u64>> {
    let Some(index) = qemu.args.iter().position(|arg| arg == "-m") else {
        return Ok(None);
    };
    let Some(value) = qemu.args.get(index + 1) else {
        bail!("QEMU `-m` argument is missing its memory size value");
    };
    parse_qemu_memory_size(value)
}

fn parse_smp_qemu_value(value: &str) -> Option<usize> {
    let first = value.split(',').next()?;
    if let Ok(cpu_num) = first.parse() {
        return Some(cpu_num);
    }

    value.split(',').find_map(|part| {
        let cpu_num = part.strip_prefix("cpus=")?;
        cpu_num.parse().ok()
    })
}

fn parse_qemu_memory_size(value: &str) -> anyhow::Result<Option<u64>> {
    let Some(size) = qemu_memory_size_token(value) else {
        return Ok(None);
    };
    let (digits, suffix) = split_qemu_memory_size(size);
    if digits.is_empty() {
        bail!("invalid QEMU memory size `{value}`");
    }

    let amount: u64 = digits
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid QEMU memory size `{value}`"))?;
    let multiplier = match suffix.to_ascii_lowercase().as_str() {
        "" | "m" | "mb" => 1024_u64 * 1024,
        "k" | "kb" => 1024,
        "g" | "gb" => 1024_u64 * 1024 * 1024,
        _ => bail!("unsupported QEMU memory size suffix in `{value}`"),
    };

    amount
        .checked_mul(multiplier)
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("QEMU memory size `{value}` is too large"))
}

fn qemu_memory_size_token(value: &str) -> Option<&str> {
    value.split(',').find_map(|part| {
        let part = part.trim();
        if part.is_empty() {
            None
        } else if let Some(size) = part.strip_prefix("size=") {
            Some(size)
        } else if part.contains('=') {
            None
        } else {
            Some(part)
        }
    })
}

fn split_qemu_memory_size(value: &str) -> (&str, &str) {
    let split = value
        .char_indices()
        .find_map(|(index, ch)| (!ch.is_ascii_digit()).then_some(index))
        .unwrap_or(value.len());
    value.split_at(split)
}

fn qemu_memory_size_arg(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes.is_multiple_of(MIB) {
        format!("{}M", bytes / MIB)
    } else {
        bytes.to_string()
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

pub(crate) fn finalize_qemu_test_run(suite_name: &str, failed: &[String]) -> anyhow::Result<()> {
    if failed.is_empty() {
        println!("all {} qemu tests passed", suite_name);
        Ok(())
    } else {
        bail!(
            "{} qemu tests failed for {} package(s): {}",
            suite_name,
            failed.len(),
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
        let err = finalize_qemu_test_run("arceos", &["pkg-b".to_string(), "pkg-c".to_string()])
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
