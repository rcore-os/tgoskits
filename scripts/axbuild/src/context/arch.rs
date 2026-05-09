use anyhow::anyhow;

use super::{
    DEFAULT_ARCEOS_ARCH, DEFAULT_ARCEOS_TARGET, DEFAULT_AXVISOR_ARCH, DEFAULT_AXVISOR_TARGET,
    DEFAULT_STARRY_ARCH, DEFAULT_STARRY_TARGET,
};

const ARCH_TARGETS: &[(&str, &str)] = &[
    ("aarch64", "aarch64-unknown-none-softfloat"),
    ("x86_64", "x86_64-unknown-none"),
    ("riscv64", "riscv64gc-unknown-none-elf"),
    ("loongarch64", "loongarch64-unknown-none-softfloat"),
];

const SUPPORTED_ARCH_VALUES: &str = "aarch64, x86_64, riscv64, loongarch64";
const SUPPORTED_TARGET_VALUES: &str = "x86_64-unknown-none, aarch64-unknown-none-softfloat, \
                                       riscv64gc-unknown-none-elf, \
                                       loongarch64-unknown-none-softfloat";

pub(crate) fn arch_for_target(target: &str) -> Option<&'static str> {
    ARCH_TARGETS
        .iter()
        .find_map(|(arch, candidate)| (*candidate == target).then_some(*arch))
}

pub(crate) fn starry_target_for_arch_checked(arch: &str) -> anyhow::Result<&'static str> {
    target_for_arch_checked_impl(arch, "Starry")
}

pub(crate) fn starry_arch_for_target_checked(target: &str) -> anyhow::Result<&'static str> {
    arch_for_target_checked_impl(target, "Starry")
}

pub(crate) fn arch_for_target_checked(target: &str) -> anyhow::Result<&'static str> {
    arch_for_target_checked_impl(target, "Starry")
}

pub(crate) fn resolve_starry_arch_and_target(
    arch: Option<String>,
    target: Option<String>,
) -> anyhow::Result<(String, String)> {
    resolve_arch_and_target(
        arch,
        target,
        DEFAULT_STARRY_ARCH,
        DEFAULT_STARRY_TARGET,
        "Starry",
    )
}

pub(crate) fn resolve_arceos_arch_and_target(
    arch: Option<String>,
    target: Option<String>,
) -> anyhow::Result<(String, String)> {
    resolve_arch_and_target(
        arch,
        target,
        DEFAULT_ARCEOS_ARCH,
        DEFAULT_ARCEOS_TARGET,
        "ArceOS",
    )
}

pub(crate) fn resolve_axvisor_arch_and_target(
    arch: Option<String>,
    target: Option<String>,
) -> anyhow::Result<(String, String)> {
    resolve_arch_and_target(
        arch,
        target,
        DEFAULT_AXVISOR_ARCH,
        DEFAULT_AXVISOR_TARGET,
        "Axvisor",
    )
}

pub(crate) fn validate_supported_target(
    target: &str,
    suite_name: &str,
    supported_kind: &str,
    supported: &[&str],
) -> anyhow::Result<()> {
    if supported.contains(&target) {
        Ok(())
    } else {
        anyhow::bail!(
            "unsupported target `{}` for {}. Supported {} are: {}",
            target,
            suite_name,
            supported_kind,
            supported.join(", ")
        )
    }
}

fn arch_target_entry(arch: &str) -> Option<&'static (&'static str, &'static str)> {
    ARCH_TARGETS
        .iter()
        .find(|(candidate, _)| *candidate == arch)
}

fn target_for_arch_checked_impl(arch: &str, component: &str) -> anyhow::Result<&'static str> {
    arch_target_entry(arch)
        .map(|(_, target)| *target)
        .ok_or_else(|| {
            anyhow!(
                "unsupported {component} architecture `{arch}`; expected one of \
                 {SUPPORTED_ARCH_VALUES}"
            )
        })
}

fn arch_for_target_checked_impl(target: &str, component: &str) -> anyhow::Result<&'static str> {
    arch_for_target(target).ok_or_else(|| {
        anyhow!(
            "unsupported {component} target `{target}`; expected one of {SUPPORTED_TARGET_VALUES}"
        )
    })
}

fn resolve_arch_and_target(
    arch: Option<String>,
    target: Option<String>,
    default_arch: &str,
    default_target: &str,
    component: &str,
) -> anyhow::Result<(String, String)> {
    match (arch, target) {
        (Some(arch), Some(target)) => {
            let expected_target = target_for_arch_checked_impl(&arch, component)?;
            if target != expected_target {
                anyhow::bail!(
                    "{component} arch `{arch}` maps to target `{expected_target}`, but got \
                     `{target}`"
                );
            }
            Ok((arch, target))
        }
        (Some(arch), None) => Ok((
            arch.clone(),
            target_for_arch_checked_impl(&arch, component)?.to_string(),
        )),
        (None, Some(target)) => Ok((
            arch_for_target_checked_impl(&target, component)?.to_string(),
            target,
        )),
        (None, None) => Ok((default_arch.to_string(), default_target.to_string())),
    }
}
