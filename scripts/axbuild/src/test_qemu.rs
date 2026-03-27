#[cfg(test)]
use std::future::Future;
use std::path::PathBuf;

use anyhow::Context;
use clap::Args;
use ostool::build::CargoQemuOverrideArgs;

use crate::{
    arceos,
    axvisor::{
        self,
        context::AxvisorContext,
        qemu_test::{
            ShellAutoInitConfig, prepare_linux_aarch64_guest_assets,
            prepare_nimbos_x86_64_guest_vmconfig, shell_autoinit_qemu_override_args,
        },
    },
    context::{
        AppContext, AxvisorCliArgs, BuildCliArgs, QemuRunConfig, StarryCliArgs,
        starry_target_for_arch_checked,
    },
    starry,
};

const ARCEOS_TEST_PACKAGES: &[&str] = &[
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

const STARRY_TEST_PACKAGE: &str = "starryos-test";
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

#[derive(Args, Debug, Clone)]
pub struct ArgsArceos {
    #[arg(long)]
    pub target: String,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsStarry {
    #[arg(long, alias = "arch", value_name = "ARCH")]
    pub target: String,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsAxvisor {
    #[arg(long, alias = "arch", value_name = "ARCH")]
    pub target: String,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsAxvisorUboot {
    #[arg(short = 'b', long, value_name = "BOARD")]
    pub board: String,

    #[arg(long)]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AxvisorUbootBoardConfig {
    board: &'static str,
    build_config: &'static str,
    vmconfig: &'static str,
}

pub async fn run_arceos_qemu_tests(args: ArgsArceos) -> anyhow::Result<()> {
    let target = validate_arceos_target(&args.target)?;
    let mut app = AppContext::new()?;
    let mut failed = Vec::new();

    println!(
        "running arceos qemu tests for {} package(s) on target: {}",
        ARCEOS_TEST_PACKAGES.len(),
        target
    );

    for (index, package) in ARCEOS_TEST_PACKAGES.iter().enumerate() {
        println!(
            "[{}/{}] arceos qemu {}",
            index + 1,
            ARCEOS_TEST_PACKAGES.len(),
            package
        );
        let (request, _snapshot) = app.prepare_arceos_request(
            BuildCliArgs {
                config: None,
                package: Some((*package).to_string()),
                target: Some(target.to_string()),
                plat_dyn: None,
            },
            None,
            None,
        )?;

        let cargo = arceos::build::load_cargo_config(&request)?;
        match app
            .qemu(
                cargo,
                request.build_info_path,
                QemuRunConfig {
                    qemu_config: request.qemu_config,
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("arceos qemu test failed for package `{package}`"))
        {
            Ok(()) => println!("ok: {}", package),
            Err(err) => {
                eprintln!("failed: {}: {:#}", package, err);
                failed.push((*package).to_string());
            }
        }
    }

    finalize_qemu_test_run("arceos", &failed)
}

pub async fn run_starry_qemu_tests(args: ArgsStarry) -> anyhow::Result<()> {
    let (arch, target) = parse_starry_test_target(&args.target)?;
    let mut app = AppContext::new()?;
    let mut failed = Vec::new();

    println!(
        "running starry qemu tests for package {} on arch: {} (target: {})",
        STARRY_TEST_PACKAGE, arch, target
    );

    for (index, package) in [STARRY_TEST_PACKAGE].iter().enumerate() {
        println!("[{}/{}] starry qemu {}", index + 1, 1, package);
        let (mut request, _snapshot) = app.prepare_starry_request(
            StarryCliArgs {
                config: None,
                arch: Some(arch.to_string()),
                target: None,
                plat_dyn: None,
            },
            None,
            None,
        )?;
        request.package = STARRY_TEST_PACKAGE.to_string();

        let cargo = starry::build::load_cargo_config(&request)?;
        let qemu_args = starry::rootfs::default_qemu_args(app.workspace_root(), &request).await?;
        match app
            .qemu(
                cargo,
                request.build_info_path,
                QemuRunConfig {
                    qemu_config: request.qemu_config,
                    default_args: CargoQemuOverrideArgs {
                        args: Some(qemu_args),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .with_context(|| "starry qemu test failed")
        {
            Ok(()) => println!("ok: {}", package),
            Err(err) => {
                eprintln!("failed: {}: {:#}", package, err);
                failed.push((*package).to_string());
            }
        }
    }

    finalize_qemu_test_run("starry", &failed)
}

pub async fn run_axvisor_qemu_tests(args: ArgsAxvisor) -> anyhow::Result<()> {
    let (arch, target) = parse_axvisor_test_target(&args.target)?;
    let guest_ctx = AxvisorContext::new()?;
    let mut app = AppContext::new()?;

    println!(
        "running axvisor qemu tests for arch: {} (target: {})",
        arch, target
    );

    let vmconfig = match arch {
        "aarch64" => {
            prepare_linux_aarch64_guest_assets(&guest_ctx)
                .await?
                .generated_vmconfig
        }
        "x86_64" => prepare_nimbos_x86_64_guest_vmconfig(&guest_ctx).await?,
        _ => unreachable!(),
    };

    let (request, _snapshot) = app.prepare_axvisor_request(
        AxvisorCliArgs {
            config: None,
            arch: Some(arch.to_string()),
            target: None,
            plat_dyn: None,
            vmconfigs: vec![vmconfig],
        },
        None,
        None,
    )?;

    let cargo = axvisor::build::load_cargo_config(&request)?;
    let qemu_config =
        axvisor::build::default_qemu_config_template_path(&request.axvisor_dir, &request.arch);
    let shell = axvisor_test_shell_config(arch);
    let override_args = shell_autoinit_qemu_override_args(&request, &shell)?;

    app.qemu(
        cargo,
        request.build_info_path,
        QemuRunConfig {
            qemu_config: Some(qemu_config),
            override_args,
            ..Default::default()
        },
    )
    .await
    .with_context(|| "axvisor qemu test failed")
}

pub async fn run_axvisor_uboot_tests(args: ArgsAxvisorUboot) -> anyhow::Result<()> {
    let board = axvisor_uboot_board_config(&args.board)?;
    let mut app = AppContext::new()?;
    let explicit_uboot_config = args.uboot_config.clone();
    let uboot_config_summary = explicit_uboot_config
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "using ostool default search".to_string());

    if let Some(path) = explicit_uboot_config.as_ref()
        && !path.exists()
    {
        bail!(
            "missing explicit U-Boot config `{}` for axvisor board tests",
            path.display()
        );
    }

    println!(
        "running axvisor uboot test for board: {} with vmconfig: {}",
        board.board, board.vmconfig
    );

    let (mut request, _snapshot) = app.prepare_axvisor_request(
        AxvisorCliArgs {
            config: Some(PathBuf::from(board.build_config)),
            arch: None,
            target: None,
            plat_dyn: None,
            vmconfigs: vec![PathBuf::from(board.vmconfig)],
        },
        None,
        explicit_uboot_config.clone(),
    )?;
    request.uboot_config = explicit_uboot_config;

    let cargo = axvisor::build::load_cargo_config(&request)?;
    app.uboot(cargo, request.build_info_path, request.uboot_config)
        .await
        .with_context(|| {
            format!(
                "axvisor uboot test failed for board `{}` (build_config={}, vmconfig={}, \
                 uboot_config={})",
                board.board, board.build_config, board.vmconfig, uboot_config_summary
            )
        })
}

fn validate_arceos_target(target: &str) -> anyhow::Result<&str> {
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

fn parse_starry_test_target(target: &str) -> anyhow::Result<(&str, &'static str)> {
    if !STARRY_TEST_ARCHES.contains(&target) {
        bail!(
            "unsupported target `{}` for starry qemu tests. Supported arch values are: {}",
            target,
            STARRY_TEST_ARCHES.join(", ")
        );
    }
    Ok((target, starry_target_for_arch_checked(target)?))
}

fn parse_axvisor_test_target(target: &str) -> anyhow::Result<(&str, &'static str)> {
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

fn axvisor_uboot_board_config(board: &str) -> anyhow::Result<AxvisorUbootBoardConfig> {
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

fn axvisor_test_shell_config(arch: &str) -> ShellAutoInitConfig {
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

#[cfg(test)]
async fn run_qemu_test_sequence<F, Fut>(
    suite_name: &str,
    packages: &[&str],
    mut run_one: F,
) -> anyhow::Result<()>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let mut failed = Vec::new();

    for (index, package) in packages.iter().enumerate() {
        println!(
            "[{}/{}] {} qemu {}",
            index + 1,
            packages.len(),
            suite_name,
            package
        );
        match run_one(package).await {
            Ok(()) => println!("ok: {}", package),
            Err(err) => {
                eprintln!("failed: {}: {:#}", package, err);
                failed.push((*package).to_string());
            }
        }
    }

    finalize_qemu_test_run(suite_name, &failed)
}

fn finalize_qemu_test_run(suite_name: &str, failed: &[String]) -> anyhow::Result<()> {
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

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, future};

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

    #[tokio::test]
    async fn qemu_test_sequence_succeeds_when_all_packages_pass() {
        run_qemu_test_sequence("arceos", &["pkg-a", "pkg-b"], |_| future::ready(Ok(())))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn qemu_test_sequence_reports_aggregated_failures() {
        let outcomes = HashMap::from([("pkg-a", true), ("pkg-b", false), ("pkg-c", false)]);
        let err = run_qemu_test_sequence("arceos", &["pkg-a", "pkg-b", "pkg-c"], |package| {
            let ok = *outcomes.get(package).unwrap();
            future::ready(if ok { Ok(()) } else { Err(anyhow!("boom")) })
        })
        .await
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("arceos qemu tests failed for 2 package(s): pkg-b, pkg-c")
        );
    }
}
