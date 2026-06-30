use anyhow::anyhow;

use super::{
    DEFAULT_ARCEOS_ARCH, DEFAULT_ARCEOS_TARGET, DEFAULT_AXVISOR_ARCH, DEFAULT_AXVISOR_TARGET,
    DEFAULT_STARRY_ARCH, DEFAULT_STARRY_TARGET,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CrossCompileSpec {
    pub(crate) llvm_target: &'static str,
    pub(crate) cmake_system_processor: &'static str,
    pub(crate) guest_tool_dir: &'static str,
    pub(crate) gnu_tool_prefix: &'static str,
    pub(crate) qemu_user_binaries: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArchSpec {
    pub(crate) arch: &'static str,
    pub(crate) target: &'static str,
    pub(crate) default_rootfs_image: &'static str,
    pub(crate) starry_default_platform: Option<&'static str>,
    pub(crate) cross_compile: CrossCompileSpec,
}

const ARCH_SPECS: &[ArchSpec] = &[
    ArchSpec {
        arch: "aarch64",
        target: "aarch64-unknown-none-softfloat",
        default_rootfs_image: "rootfs-aarch64-alpine.img",
        starry_default_platform: None,
        cross_compile: CrossCompileSpec {
            llvm_target: "aarch64-linux-musl",
            cmake_system_processor: "aarch64",
            guest_tool_dir: "usr/aarch64-alpine-linux-musl/bin",
            gnu_tool_prefix: "aarch64-linux-musl",
            qemu_user_binaries: &["qemu-aarch64-static", "qemu-aarch64"],
        },
    },
    ArchSpec {
        arch: "x86_64",
        target: "x86_64-unknown-none",
        default_rootfs_image: "rootfs-x86_64-alpine.img",
        starry_default_platform: None,
        cross_compile: CrossCompileSpec {
            llvm_target: "x86_64-linux-musl",
            cmake_system_processor: "x86_64",
            guest_tool_dir: "usr/x86_64-alpine-linux-musl/bin",
            gnu_tool_prefix: "x86_64-linux-musl",
            qemu_user_binaries: &["qemu-x86_64-static", "qemu-x86_64"],
        },
    },
    ArchSpec {
        arch: "riscv64",
        target: "riscv64gc-unknown-none-elf",
        default_rootfs_image: "rootfs-riscv64-alpine.img",
        starry_default_platform: None,
        cross_compile: CrossCompileSpec {
            llvm_target: "riscv64-linux-musl",
            cmake_system_processor: "riscv64",
            guest_tool_dir: "usr/riscv64-alpine-linux-musl/bin",
            gnu_tool_prefix: "riscv64-linux-musl",
            qemu_user_binaries: &["qemu-riscv64-static", "qemu-riscv64"],
        },
    },
    ArchSpec {
        arch: "loongarch64",
        target: "loongarch64-unknown-none-softfloat",
        default_rootfs_image: "rootfs-loongarch64-alpine.img",
        starry_default_platform: None,
        cross_compile: CrossCompileSpec {
            llvm_target: "loongarch64-linux-musl",
            cmake_system_processor: "loongarch64",
            guest_tool_dir: "usr/loongarch64-alpine-linux-musl/bin",
            gnu_tool_prefix: "loongarch64-linux-musl",
            qemu_user_binaries: &["qemu-loongarch64-static", "qemu-loongarch64"],
        },
    },
];

const SUPPORTED_ARCH_VALUES: &str = "aarch64, x86_64, riscv64, loongarch64";
const SUPPORTED_TARGET_VALUES: &str = "x86_64-unknown-none, aarch64-unknown-none-softfloat, \
                                       riscv64gc-unknown-none-elf, \
                                       loongarch64-unknown-none-softfloat";

pub(crate) fn supported_arches() -> Vec<&'static str> {
    ARCH_SPECS.iter().map(|spec| spec.arch).collect()
}

pub(crate) fn supported_targets() -> Vec<&'static str> {
    ARCH_SPECS.iter().map(|spec| spec.target).collect()
}

pub(crate) fn arch_spec(arch: &str) -> Option<&'static ArchSpec> {
    ARCH_SPECS.iter().find(|spec| spec.arch == arch)
}

pub(crate) fn arch_spec_for_target(target: &str) -> Option<&'static ArchSpec> {
    ARCH_SPECS.iter().find(|spec| spec.target == target)
}

pub(crate) fn arch_for_target(target: &str) -> Option<&'static str> {
    arch_spec_for_target(target).map(|spec| spec.arch)
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

pub(crate) fn default_rootfs_image_for_arch(arch: &str) -> Option<&'static str> {
    arch_spec(arch).map(|spec| spec.default_rootfs_image)
}

pub(crate) fn starry_default_platform_for_arch_checked(
    arch: &str,
) -> anyhow::Result<Option<&'static str>> {
    arch_spec(arch)
        .map(|spec| spec.starry_default_platform)
        .ok_or_else(|| unsupported_arch_error(arch, "Starry"))
}

pub(crate) fn cross_compile_spec_for_arch_checked(arch: &str) -> anyhow::Result<CrossCompileSpec> {
    arch_spec(arch)
        .map(|spec| spec.cross_compile)
        .ok_or_else(|| {
            anyhow!(
                "C-based QEMU test cases are only supported on {SUPPORTED_ARCH_VALUES}, but got \
                 `{arch}`"
            )
        })
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

fn target_for_arch_checked_impl(arch: &str, component: &str) -> anyhow::Result<&'static str> {
    arch_spec(arch)
        .map(|spec| spec.target)
        .ok_or_else(|| unsupported_arch_error(arch, component))
}

fn arch_for_target_checked_impl(target: &str, component: &str) -> anyhow::Result<&'static str> {
    arch_for_target(target).ok_or_else(|| {
        anyhow!(
            "unsupported {component} target `{target}`; expected one of {SUPPORTED_TARGET_VALUES}"
        )
    })
}

fn unsupported_arch_error(arch: &str, component: &str) -> anyhow::Error {
    anyhow!(
        "unsupported {component} architecture `{arch}`; expected one of {SUPPORTED_ARCH_VALUES}"
    )
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
