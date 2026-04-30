#[cfg(test)]
use crate::{context::resolve_arceos_arch_and_target, test::qemu::parse_test_target};

pub(crate) const TEST_PACKAGES: &[&str] = &[
    "arceos-memtest",
    "arceos-exception",
    "arceos-affinity",
    "arceos-ipi",
    "arceos-net-echoserver",
    "arceos-net-httpclient",
    "arceos-net-httpserver",
    "arceos-irq",
    "arceos-parallel",
    "arceos-priority",
    "arceos-fs-shell",
    "arceos-sleep",
    "arceos-tls",
    "arceos-net-udpserver",
    "arceos-wait-queue",
    "arceos-yield",
];

pub(crate) const TEST_TARGETS: &[&str] = &[
    "x86_64-unknown-none",
    "riscv64gc-unknown-none-elf",
    "aarch64-unknown-none-softfloat",
    "loongarch64-unknown-none-softfloat",
];
pub(crate) const TEST_ARCHES: &[&str] = &["x86_64", "riscv64", "aarch64", "loongarch64"];

#[cfg(test)]
pub(crate) fn parse_target(
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    parse_test_target(
        arch,
        target,
        "arceos qemu tests",
        TEST_ARCHES,
        TEST_TARGETS,
        resolve_arceos_arch_and_target,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_supported_targets() {
        assert_eq!(
            parse_target(&None, &Some("x86_64-unknown-none".to_string())).unwrap(),
            ("x86_64".to_string(), "x86_64-unknown-none".to_string())
        );
        assert_eq!(
            parse_target(&None, &Some("aarch64-unknown-none-softfloat".to_string())).unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
    }

    #[test]
    fn accepts_supported_arch_aliases() {
        assert_eq!(
            parse_target(&Some("x86_64".to_string()), &None).unwrap(),
            ("x86_64".to_string(), "x86_64-unknown-none".to_string())
        );
        assert_eq!(
            parse_target(&Some("aarch64".to_string()), &None).unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
    }

    #[test]
    fn rejects_unsupported_targets() {
        let rejected_target = "mips64-unknown-none".to_string();
        let err = parse_target(&None, &Some(rejected_target.clone())).unwrap_err();
        assert!(err.to_string().contains(&rejected_target));
    }
}
