use crate::{
    axvisor::qemu_test::ShellAutoInitConfig,
    context::{starry_target_for_arch_checked, target_for_arch_checked},
};

pub(crate) const ARCEOS_TEST_PACKAGES: &[&str] = &[
    "arceos-memtest",
    "arceos-exception",
    "arceos-affinity",
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

const ARCEOS_TEST_TARGETS: &[&str] = &[
    "x86_64-unknown-none",
    "riscv64gc-unknown-none-elf",
    "aarch64-unknown-none-softfloat",
    "loongarch64-unknown-none-softfloat",
];

pub(crate) const STARRY_TEST_PACKAGE: &str = "starryos-test";
const STARRY_TEST_ARCHES: &[&str] = &["x86_64", "riscv64", "aarch64", "loongarch64"];
const AXVISOR_TEST_ARCHES: &[&str] = &["aarch64", "x86_64"];
const AXVISOR_AARCH64_TEST_SHELL_PREFIX: &str = "~ #";
const AXVISOR_AARCH64_TEST_SHELL_INIT_CMD: &str = "pwd && echo 'guest test pass!'";
const AXVISOR_AARCH64_TEST_SUCCESS_REGEX: &[&str] = &["(?m)^guest test pass!\\s*$"];
const AXVISOR_X86_64_TEST_SHELL_PREFIX: &str = ">>";
const AXVISOR_X86_64_TEST_SHELL_INIT_CMD: &str = "hello_world";
const AXVISOR_X86_64_TEST_SUCCESS_REGEX: &[&str] = &["Hello world from user mode program!"];
const AXVISOR_TEST_FAIL_REGEX: &[&str] = &[
    "(?i)\\bpanic(?:ked)?\\b",
    "(?i)kernel panic",
    "(?i)login incorrect",
    "(?i)permission denied",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AxvisorUbootBoardConfig {
    pub(crate) board: &'static str,
    pub(crate) build_config: &'static str,
    pub(crate) vmconfig: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AxvisorBoardTestGroup {
    pub(crate) name: &'static str,
    pub(crate) build_config: &'static str,
    pub(crate) vmconfigs: &'static [&'static str],
    pub(crate) board_test_config: &'static str,
}

const AXVISOR_UBOOT_BOARD_CONFIGS: &[AxvisorUbootBoardConfig] = &[
    AxvisorUbootBoardConfig {
        board: "orangepi-5-plus",
        build_config: "os/axvisor/configs/board/orangepi-5-plus.toml",
        vmconfig: "os/axvisor/configs/vms/linux-aarch64-orangepi5p-smp1.toml",
    },
    AxvisorUbootBoardConfig {
        board: "phytiumpi",
        build_config: "os/axvisor/configs/board/phytiumpi.toml",
        vmconfig: "os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml",
    },
    AxvisorUbootBoardConfig {
        board: "roc-rk3568-pc",
        build_config: "os/axvisor/configs/board/roc-rk3568-pc.toml",
        vmconfig: "os/axvisor/configs/vms/linux-aarch64-rk3568-smp1.toml",
    },
];

const PHYTIUMPI_LINUX_VMCONFIGS: &[&str] =
    &["os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml"];
const ORANGEPI_5_PLUS_LINUX_VMCONFIGS: &[&str] =
    &["os/axvisor/configs/vms/linux-aarch64-orangepi5p-smp1.toml"];
const ROC_RK3568_PC_LINUX_VMCONFIGS: &[&str] =
    &["os/axvisor/configs/vms/linux-aarch64-rk3568-smp1.toml"];

const AXVISOR_BOARD_TEST_GROUPS: &[AxvisorBoardTestGroup] = &[
    AxvisorBoardTestGroup {
        name: "phytiumpi-linux",
        build_config: "os/axvisor/configs/board/phytiumpi.toml",
        vmconfigs: PHYTIUMPI_LINUX_VMCONFIGS,
        board_test_config: "os/axvisor/configs/board-test/phytiumpi-linux.toml",
    },
    AxvisorBoardTestGroup {
        name: "orangepi-5-plus-linux",
        build_config: "os/axvisor/configs/board/orangepi-5-plus.toml",
        vmconfigs: ORANGEPI_5_PLUS_LINUX_VMCONFIGS,
        board_test_config: "os/axvisor/configs/board-test/orangepi-5-plus-linux.toml",
    },
    AxvisorBoardTestGroup {
        name: "roc-rk3568-pc-linux",
        build_config: "os/axvisor/configs/board/roc-rk3568-pc.toml",
        vmconfigs: ROC_RK3568_PC_LINUX_VMCONFIGS,
        board_test_config: "os/axvisor/configs/board-test/roc-rk3568-pc-linux.toml",
    },
];

pub(crate) fn validate_arceos_target(target: &str) -> anyhow::Result<&str> {
    validate_supported_target(target, "arceos qemu tests", "targets", ARCEOS_TEST_TARGETS)?;
    Ok(target)
}

pub(crate) fn parse_starry_test_target(target: &str) -> anyhow::Result<(&str, &'static str)> {
    parse_arch_alias_target(
        target,
        "starry qemu tests",
        STARRY_TEST_ARCHES,
        starry_target_for_arch_checked,
    )
}

pub(crate) fn parse_axvisor_test_target(target: &str) -> anyhow::Result<(&str, &'static str)> {
    parse_arch_alias_target(
        target,
        "axvisor qemu tests",
        AXVISOR_TEST_ARCHES,
        target_for_arch_checked,
    )
}

pub(crate) fn axvisor_uboot_board_config(board: &str) -> anyhow::Result<AxvisorUbootBoardConfig> {
    AXVISOR_UBOOT_BOARD_CONFIGS
        .iter()
        .copied()
        .find(|config| config.board == board)
        .ok_or_else(|| {
            anyhow!(
                "unsupported board `{}` for axvisor uboot tests. Supported boards are: {}",
                board,
                supported_board_names()
            )
        })
}

pub(crate) fn axvisor_board_test_groups(
    test_group: Option<&str>,
) -> anyhow::Result<Vec<AxvisorBoardTestGroup>> {
    match test_group {
        Some(name) => AXVISOR_BOARD_TEST_GROUPS
            .iter()
            .copied()
            .find(|group| group.name == name)
            .map(|group| vec![group])
            .ok_or_else(|| {
                anyhow!(
                    "unsupported axvisor board test group `{}`. Supported groups are: {}",
                    name,
                    supported_board_test_group_names()
                )
            }),
        None => Ok(AXVISOR_BOARD_TEST_GROUPS.to_vec()),
    }
}

fn default_axvisor_test_success_regex() -> Vec<String> {
    owned_patterns(AXVISOR_AARCH64_TEST_SUCCESS_REGEX)
}

fn default_axvisor_test_fail_regex() -> Vec<String> {
    owned_patterns(AXVISOR_TEST_FAIL_REGEX)
}

pub(crate) fn axvisor_test_shell_config(arch: &str) -> anyhow::Result<ShellAutoInitConfig> {
    match arch {
        "aarch64" => Ok(ShellAutoInitConfig {
            shell_prefix: AXVISOR_AARCH64_TEST_SHELL_PREFIX.to_string(),
            shell_init_cmd: AXVISOR_AARCH64_TEST_SHELL_INIT_CMD.to_string(),
            success_regex: default_axvisor_test_success_regex(),
            fail_regex: default_axvisor_test_fail_regex(),
        }),
        "x86_64" => Ok(ShellAutoInitConfig {
            shell_prefix: AXVISOR_X86_64_TEST_SHELL_PREFIX.to_string(),
            shell_init_cmd: AXVISOR_X86_64_TEST_SHELL_INIT_CMD.to_string(),
            success_regex: owned_patterns(AXVISOR_X86_64_TEST_SUCCESS_REGEX),
            fail_regex: default_axvisor_test_fail_regex(),
        }),
        _ => bail!(
            "unsupported target `{arch}` for axvisor qemu tests. Supported arch values are: {}",
            AXVISOR_TEST_ARCHES.join(", ")
        ),
    }
}

fn validate_supported_target(
    target: &str,
    suite_name: &str,
    supported_kind: &str,
    supported: &[&str],
) -> anyhow::Result<()> {
    if supported.contains(&target) {
        Ok(())
    } else {
        bail!(
            "unsupported target `{}` for {}. Supported {} are: {}",
            target,
            suite_name,
            supported_kind,
            supported.join(", ")
        )
    }
}

fn validate_supported_arch_alias(
    target: &str,
    suite_name: &str,
    supported_arches: &[&str],
) -> anyhow::Result<()> {
    if target.contains('-') {
        bail!(
            "unsupported target `{target}` for {suite_name}. Pass an arch value like: {}",
            supported_arches.join(", ")
        );
    }

    validate_supported_target(target, suite_name, "arch values", supported_arches)
}

fn parse_arch_alias_target<'a>(
    target: &'a str,
    suite_name: &str,
    supported_arches: &[&str],
    resolve_target: fn(&str) -> anyhow::Result<&'static str>,
) -> anyhow::Result<(&'a str, &'static str)> {
    validate_supported_arch_alias(target, suite_name, supported_arches)?;
    Ok((target, resolve_target(target)?))
}

fn supported_board_names() -> String {
    AXVISOR_UBOOT_BOARD_CONFIGS
        .iter()
        .map(|config| config.board)
        .collect::<Vec<_>>()
        .join(", ")
}

fn supported_board_test_group_names() -> String {
    AXVISOR_BOARD_TEST_GROUPS
        .iter()
        .map(|group| group.name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn owned_patterns(patterns: &[&str]) -> Vec<String> {
    patterns
        .iter()
        .map(|pattern| (*pattern).to_string())
        .collect()
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

pub(crate) fn finalize_board_test_run(failed: &[String]) -> anyhow::Result<()> {
    if failed.is_empty() {
        println!("all axvisor board tests passed");
        Ok(())
    } else {
        bail!(
            "axvisor board tests failed for {} group(s): {}",
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
    fn accepts_supported_arceos_targets() {
        assert_eq!(
            validate_arceos_target("x86_64-unknown-none").unwrap(),
            "x86_64-unknown-none"
        );
        assert_eq!(
            validate_arceos_target("aarch64-unknown-none-softfloat").unwrap(),
            "aarch64-unknown-none-softfloat"
        );
    }

    #[test]
    fn rejects_unsupported_arceos_targets() {
        let err = validate_arceos_target("aarch64").unwrap_err();

        assert!(err.to_string().contains("unsupported target `aarch64`"));
    }

    #[test]
    fn parses_supported_starry_arch_aliases() {
        assert_eq!(
            parse_starry_test_target("x86_64").unwrap(),
            ("x86_64", "x86_64-unknown-none")
        );
        assert_eq!(
            parse_starry_test_target("aarch64").unwrap(),
            ("aarch64", "aarch64-unknown-none-softfloat")
        );
    }

    #[test]
    fn rejects_starry_full_target_triples() {
        let err = parse_starry_test_target("x86_64-unknown-none").unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported target `x86_64-unknown-none`")
        );
    }

    #[test]
    fn parses_supported_axvisor_arch_aliases() {
        assert_eq!(
            parse_axvisor_test_target("aarch64").unwrap(),
            ("aarch64", "aarch64-unknown-none-softfloat")
        );
        assert_eq!(
            parse_axvisor_test_target("x86_64").unwrap(),
            ("x86_64", "x86_64-unknown-none")
        );
    }

    #[test]
    fn rejects_axvisor_full_target_triples() {
        let err = parse_axvisor_test_target("aarch64-unknown-none-softfloat").unwrap_err();

        assert!(
            err.to_string().contains("Pass an arch value like: aarch64"),
            "{}",
            err
        );
    }

    #[test]
    fn rejects_unsupported_axvisor_arches() {
        let err = parse_axvisor_test_target("riscv64").unwrap_err();

        assert!(
            err.to_string()
                .contains("Supported arch values are: aarch64")
        );
    }

    #[test]
    fn parses_axvisor_uboot_board_config_for_linux_smoke() {
        assert_eq!(
            axvisor_uboot_board_config("orangepi-5-plus").unwrap(),
            AxvisorUbootBoardConfig {
                board: "orangepi-5-plus",
                build_config: "os/axvisor/configs/board/orangepi-5-plus.toml",
                vmconfig: "os/axvisor/configs/vms/linux-aarch64-orangepi5p-smp1.toml",
            }
        );
        assert_eq!(
            axvisor_uboot_board_config("phytiumpi").unwrap(),
            AxvisorUbootBoardConfig {
                board: "phytiumpi",
                build_config: "os/axvisor/configs/board/phytiumpi.toml",
                vmconfig: "os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml",
            }
        );
        assert_eq!(
            axvisor_uboot_board_config("roc-rk3568-pc").unwrap(),
            AxvisorUbootBoardConfig {
                board: "roc-rk3568-pc",
                build_config: "os/axvisor/configs/board/roc-rk3568-pc.toml",
                vmconfig: "os/axvisor/configs/vms/linux-aarch64-rk3568-smp1.toml",
            }
        );
    }

    #[test]
    fn rejects_unsupported_axvisor_uboot_board() {
        let err = axvisor_uboot_board_config("unknown-board").unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported board `unknown-board`")
        );
        assert!(err.to_string().contains("orangepi-5-plus"));
        assert!(err.to_string().contains("phytiumpi"));
        assert!(err.to_string().contains("roc-rk3568-pc"));
    }

    #[test]
    fn returns_all_axvisor_board_test_groups_when_no_filter_is_given() {
        let groups = axvisor_board_test_groups(None).unwrap();

        assert_eq!(
            groups.iter().map(|group| group.name).collect::<Vec<_>>(),
            vec![
                "phytiumpi-linux",
                "orangepi-5-plus-linux",
                "roc-rk3568-pc-linux"
            ]
        );
    }

    #[test]
    fn filters_axvisor_board_test_group_by_name() {
        let groups = axvisor_board_test_groups(Some("phytiumpi-linux")).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "phytiumpi-linux");
        assert_eq!(
            groups[0].vmconfigs,
            &["os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml"]
        );
    }

    #[test]
    fn rejects_unknown_axvisor_board_test_group() {
        let err = axvisor_board_test_groups(Some("unknown-linux")).unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported axvisor board test group `unknown-linux`")
        );
        assert!(err.to_string().contains("phytiumpi-linux"));
        assert!(err.to_string().contains("orangepi-5-plus-linux"));
        assert!(err.to_string().contains("roc-rk3568-pc-linux"));
    }

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

    #[test]
    fn board_failure_summary_is_aggregated() {
        let err = finalize_board_test_run(&["phytiumpi-linux".to_string()]).unwrap_err();

        assert!(
            err.to_string()
                .contains("axvisor board tests failed for 1 group(s): phytiumpi-linux")
        );
    }
}
