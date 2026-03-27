use crate::{axvisor::qemu_test::ShellAutoInitConfig, context::starry_target_for_arch_checked};

pub(crate) const ARCEOS_TEST_PACKAGES: &[&str] = &[
    "arceos-memtest",
    "arceos-affinity",
    "arceos-irq",
    "arceos-parallel",
    "arceos-priority",
    "arceos-sleep",
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
#[cfg(test)]
const STARRY_TEST_SUCCESS_REGEX: &[&str] = &["^All tests passed!$"];
#[cfg(test)]
const STARRY_TEST_FAIL_REGEX: &[&str] = &["(?i)\\bpanic(?:ked)?\\b"];
const AXVISOR_TEST_ARCHES: &[&str] = &["aarch64", "x86_64"];
const AXVISOR_AARCH64_TEST_SHELL_PREFIX: &str = "~ #";
const AXVISOR_AARCH64_TEST_SHELL_INIT_CMD: &str = "pwd && echo 'guest test pass!'";
const AXVISOR_AARCH64_TEST_SUCCESS_REGEX: &[&str] = &["^guest test pass!$"];
const AXVISOR_X86_64_TEST_SHELL_PREFIX: &str = ">>";
const AXVISOR_X86_64_TEST_SHELL_INIT_CMD: &str = "hello_world";
const AXVISOR_X86_64_TEST_SUCCESS_REGEX: &[&str] = &["Hello world from user mode program!"];
const AXVISOR_UBOOT_TEST_BOARDS: &[&str] = &["phytiumpi", "roc-rk3568-pc"];
const AXVISOR_TEST_FAIL_REGEX: &[&str] = &[
    "(?i)\\bpanic(?:ked)?\\b",
    "(?i)kernel panic",
    "(?i)login incorrect",
    "(?i)permission denied",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AxvisorUbootBoardConfig {
    pub(crate) board: &'static str,
    pub(crate) build_config: &'static str,
    pub(crate) vmconfig: &'static str,
}

pub(crate) fn validate_arceos_target(target: &str) -> anyhow::Result<&str> {
    if ARCEOS_TEST_TARGETS.contains(&target) {
        Ok(target)
    } else {
        bail!(
            "unsupported target `{}` for arceos qemu tests. Supported targets are: {}",
            target,
            ARCEOS_TEST_TARGETS.join(", ")
        )
    }
}

pub(crate) fn parse_starry_test_target(target: &str) -> anyhow::Result<(&str, &'static str)> {
    if !STARRY_TEST_ARCHES.contains(&target) {
        bail!(
            "unsupported target `{}` for starry qemu tests. Supported arch values are: {}",
            target,
            STARRY_TEST_ARCHES.join(", ")
        );
    }
    Ok((target, starry_target_for_arch_checked(target)?))
}

pub(crate) fn parse_axvisor_test_target(target: &str) -> anyhow::Result<(&str, &'static str)> {
    if target.contains('-') {
        bail!(
            "unsupported target `{}` for axvisor qemu tests. Pass an arch value like: {}",
            target,
            AXVISOR_TEST_ARCHES.join(", ")
        );
    }
    if !AXVISOR_TEST_ARCHES.contains(&target) {
        bail!(
            "unsupported target `{}` for axvisor qemu tests. Supported arch values are: {}",
            target,
            AXVISOR_TEST_ARCHES.join(", ")
        );
    }
    Ok((
        target,
        match target {
            "aarch64" => "aarch64-unknown-none-softfloat",
            "x86_64" => "x86_64-unknown-none",
            _ => unreachable!(),
        },
    ))
}

pub(crate) fn axvisor_uboot_board_config(board: &str) -> anyhow::Result<AxvisorUbootBoardConfig> {
    match board {
        "phytiumpi" => Ok(AxvisorUbootBoardConfig {
            board: "phytiumpi",
            build_config: "os/axvisor/configs/board/phytiumpi.toml",
            vmconfig: "os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml",
        }),
        "roc-rk3568-pc" => Ok(AxvisorUbootBoardConfig {
            board: "roc-rk3568-pc",
            build_config: "os/axvisor/configs/board/roc-rk3568-pc.toml",
            vmconfig: "os/axvisor/configs/vms/linux-aarch64-rk3568-smp1.toml",
        }),
        _ => bail!(
            "unsupported board `{}` for axvisor uboot tests. Supported boards are: {}",
            board,
            AXVISOR_UBOOT_TEST_BOARDS.join(", ")
        ),
    }
}

#[cfg(test)]
fn default_starry_test_success_regex() -> Vec<String> {
    STARRY_TEST_SUCCESS_REGEX
        .iter()
        .map(|pattern| (*pattern).to_string())
        .collect()
}

#[cfg(test)]
fn default_starry_test_fail_regex() -> Vec<String> {
    STARRY_TEST_FAIL_REGEX
        .iter()
        .map(|pattern| (*pattern).to_string())
        .collect()
}

fn default_axvisor_test_success_regex() -> Vec<String> {
    AXVISOR_AARCH64_TEST_SUCCESS_REGEX
        .iter()
        .map(|pattern| (*pattern).to_string())
        .collect()
}

fn default_axvisor_test_fail_regex() -> Vec<String> {
    AXVISOR_TEST_FAIL_REGEX
        .iter()
        .map(|pattern| (*pattern).to_string())
        .collect()
}

pub(crate) fn axvisor_test_shell_config(arch: &str) -> ShellAutoInitConfig {
    match arch {
        "aarch64" => ShellAutoInitConfig {
            shell_prefix: AXVISOR_AARCH64_TEST_SHELL_PREFIX.to_string(),
            shell_init_cmd: AXVISOR_AARCH64_TEST_SHELL_INIT_CMD.to_string(),
            success_regex: default_axvisor_test_success_regex(),
            fail_regex: default_axvisor_test_fail_regex(),
        },
        "x86_64" => ShellAutoInitConfig {
            shell_prefix: AXVISOR_X86_64_TEST_SHELL_PREFIX.to_string(),
            shell_init_cmd: AXVISOR_X86_64_TEST_SHELL_INIT_CMD.to_string(),
            success_regex: AXVISOR_X86_64_TEST_SUCCESS_REGEX
                .iter()
                .map(|pattern| (*pattern).to_string())
                .collect(),
            fail_regex: default_axvisor_test_fail_regex(),
        },
        _ => panic!("unsupported axvisor test arch: {arch}"),
    }
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
    fn arceos_package_list_is_stable() {
        assert_eq!(
            ARCEOS_TEST_PACKAGES,
            &[
                "arceos-memtest",
                "arceos-affinity",
                "arceos-irq",
                "arceos-parallel",
                "arceos-priority",
                "arceos-sleep",
                "arceos-wait-queue",
                "arceos-yield",
            ]
        );
    }

    #[test]
    fn starry_package_list_is_stable() {
        assert_eq!(STARRY_TEST_PACKAGE, "starryos-test");
    }

    #[test]
    fn starry_default_regexes_match_expected_values() {
        assert_eq!(
            default_starry_test_success_regex(),
            vec!["^All tests passed!$".to_string()]
        );
        assert_eq!(
            default_starry_test_fail_regex(),
            vec!["(?i)\\bpanic(?:ked)?\\b".to_string()]
        );
    }

    #[test]
    fn axvisor_default_regexes_match_expected_values() {
        assert_eq!(
            default_axvisor_test_success_regex(),
            vec!["^guest test pass!$".to_string()]
        );
        assert_eq!(
            default_axvisor_test_fail_regex(),
            vec![
                "(?i)\\bpanic(?:ked)?\\b".to_string(),
                "(?i)kernel panic".to_string(),
                "(?i)login incorrect".to_string(),
                "(?i)permission denied".to_string(),
            ]
        );
    }

    #[test]
    fn axvisor_x86_64_shell_config_matches_expected_values() {
        let shell = axvisor_test_shell_config("x86_64");

        assert_eq!(shell.shell_prefix, ">>");
        assert_eq!(shell.shell_init_cmd, "hello_world");
        assert_eq!(
            shell.success_regex,
            vec!["Hello world from user mode program!".to_string()]
        );
    }

    #[test]
    fn parses_axvisor_uboot_board_config_for_linux_smoke() {
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
        let err = axvisor_uboot_board_config("orangepi-5-plus").unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported board `orangepi-5-plus`")
        );
        assert!(err.to_string().contains("phytiumpi"));
        assert!(err.to_string().contains("roc-rk3568-pc"));
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
}
