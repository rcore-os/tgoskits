use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use anyhow::{Context, bail};
use artifact::{convert_elf_to_efi, flatten_riscv_efi, validate_efi};
use clap::{Args, Subcommand};

pub(crate) use crate::build::LOONGARCH64_UEFI_TARGET;
use crate::support::process::ProcessExt;

mod artifact;

pub(crate) const AXLOADER_PACKAGE: &str = "axloader";
pub(crate) const AXLOADER_BIN: &str = "axloader";
pub(crate) const LAUNCHER_BIN: &str = "axloader-launcher";
pub(crate) const DEFAULT_UEFI_TARGET: &str = "x86_64-unknown-uefi";
const PE_MACHINE_X86_64: u16 = 0x8664;
const PE_MACHINE_LOONGARCH64: u16 = 0x6264;
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct ArgsBuild {
    #[arg(long, default_value = DEFAULT_UEFI_TARGET)]
    pub target: String,

    #[arg(long, conflicts_with = "debug")]
    pub release: bool,

    #[arg(long, conflicts_with = "release")]
    pub debug: bool,
}

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct ArgsTest {
    #[command(subcommand)]
    pub command: TestCommand,
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum TestCommand {
    /// Run axloader's host-side contract tests.
    Host,
    /// Run axloader host checks and persistent QEMU FAT scenarios
    Qemu(ArgsTestQemu),
}

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct ArgsTestQemu {
    #[arg(long, default_value = DEFAULT_UEFI_TARGET)]
    pub target: String,

    /// Start directly with an assigned v7 OTA on the persistent FAT disk.
    #[arg(long)]
    pub server_only: bool,

    /// Explicit UEFI CODE firmware for a test harness build.
    #[arg(long, value_name = "FILE", requires = "firmware_vars")]
    pub firmware_code: Option<PathBuf>,

    /// Explicit UEFI VARS firmware paired with --firmware-code.
    #[arg(long, value_name = "FILE", requires = "firmware_code")]
    pub firmware_vars: Option<PathBuf>,
}

/// Axloader host-side commands
#[derive(Subcommand)]
pub enum Command {
    /// Build axloader
    Build(ArgsBuild),
    /// Run axloader test suites
    Test(ArgsTest),
}

pub struct Axloader {
    workspace: crate::context::WorkspaceContext,
}

impl Axloader {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            workspace: crate::context::WorkspaceContext::discover(None)?,
        })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => build_with_context(&self.workspace, args),
            Command::Test(args) => test_with_context(&self.workspace, args).await,
        }
    }
}

pub fn build(workspace_root: &Path, args: ArgsBuild) -> anyhow::Result<()> {
    let workspace = crate::context::WorkspaceContext::from_root(workspace_root, None)?;
    build_with_context(&workspace, args)
}

pub async fn test(workspace_root: &Path, args: ArgsTest) -> anyhow::Result<()> {
    let workspace = crate::context::WorkspaceContext::from_root(workspace_root, None)?;
    test_with_context(&workspace, args).await
}

fn build_with_context(
    workspace: &crate::context::WorkspaceContext,
    args: ArgsBuild,
) -> anyhow::Result<()> {
    run_loader_build(
        workspace.root(),
        workspace.target_dir(),
        &args.target,
        args.release || !args.debug,
        false,
    )
}

async fn test_with_context(
    workspace: &crate::context::WorkspaceContext,
    args: ArgsTest,
) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Host => crate::test::ota::test_host(workspace),
        TestCommand::Qemu(args) => crate::test::ota::test_qemu(workspace, args).await,
    }
}

pub(crate) fn run_loader_check(
    workspace_root: &Path,
    target_dir: &Path,
    target: &str,
) -> anyhow::Result<()> {
    let plan = LoaderBuildPlan::for_target(target)?;
    let mut args = vec![
        "check".to_string(),
        "-p".to_string(),
        AXLOADER_PACKAGE.to_string(),
        "--target".to_string(),
        plan.cargo_target.clone(),
        "--bin".to_string(),
        AXLOADER_BIN.to_string(),
    ];
    args.extend(plan.cargo_args.iter().cloned());
    append_target_rustflags(&mut args, &plan);
    let mut command = StdCommand::new("cargo");
    command
        .current_dir(workspace_root)
        .args(&args)
        .arg("--target-dir")
        .arg(target_dir)
        .envs(&plan.env);
    command.exec()
}

pub(crate) fn run_loader_build(
    workspace_root: &Path,
    target_dir: &Path,
    target: &str,
    release: bool,
    inject_202_write_failure: bool,
) -> anyhow::Result<()> {
    let plan = LoaderBuildPlan::for_target(target)?;
    let mut args = vec![
        "build".to_string(),
        "-p".to_string(),
        AXLOADER_PACKAGE.to_string(),
        "--target".to_string(),
        plan.cargo_target.clone(),
        "--bin".to_string(),
        AXLOADER_BIN.to_string(),
    ];
    args.extend([
        "--bin".into(),
        LAUNCHER_BIN.into(),
        "--features".into(),
        if inject_202_write_failure {
            "uefi-launcher,qemu-fail-202-write"
        } else {
            "uefi-launcher"
        }
        .into(),
    ]);
    args.extend(plan.cargo_args.iter().cloned());
    append_target_rustflags(&mut args, &plan);
    if release {
        args.push("--release".to_string());
    }
    let mut command = StdCommand::new("cargo");
    command
        .current_dir(workspace_root)
        .args(&args)
        .arg("--target-dir")
        .arg(target_dir)
        .envs(&plan.env);
    command.exec()?;

    let profile = if release { "release" } else { "debug" };
    let output_dir = target_dir.join(target).join(profile);
    if target == crate::build::RISCV64_UEFI_TARGET {
        for binary in [AXLOADER_BIN, LAUNCHER_BIN] {
            flatten_riscv_efi(
                &output_dir.join(format!("{binary}.elf")),
                &output_dir.join(format!("{binary}.efi")),
            )?;
        }
        return Ok(());
    }
    let (loader_relocations, launcher_relocations) = if plan.convert_elf {
        (
            Some(convert_elf_to_efi(&output_dir, AXLOADER_BIN)?),
            Some(convert_elf_to_efi(&output_dir, LAUNCHER_BIN)?),
        )
    } else {
        (None, None)
    };
    validate_efi(
        &output_dir.join(format!("{AXLOADER_BIN}.efi")),
        plan.pe_machine,
        plan.strict_loong_pe,
        loader_relocations.as_deref(),
    )?;
    validate_efi(
        &output_dir.join(format!("{LAUNCHER_BIN}.efi")),
        plan.pe_machine,
        plan.strict_loong_pe,
        launcher_relocations.as_deref(),
    )
}

struct LoaderBuildPlan {
    cargo_target: String,
    cargo_args: Vec<String>,
    rustflags: Vec<String>,
    env: HashMap<String, String>,
    convert_elf: bool,
    pe_machine: u16,
    strict_loong_pe: bool,
}

impl LoaderBuildPlan {
    fn for_target(target: &str) -> anyhow::Result<Self> {
        match target {
            DEFAULT_UEFI_TARGET => Ok(Self {
                cargo_target: target.into(),
                cargo_args: Vec::new(),
                rustflags: Vec::new(),
                env: HashMap::new(),
                convert_elf: false,
                pe_machine: PE_MACHINE_X86_64,
                strict_loong_pe: false,
            }),
            crate::build::RISCV64_UEFI_TARGET => {
                let resolved = crate::build::uefi_build_target_for(target)
                    .context("RISC-V UEFI target resolver returned no target")?;
                Ok(Self {
                    cargo_target: resolved.target,
                    cargo_args: resolved.cargo_args,
                    rustflags: resolved.rustflags,
                    env: resolved.env,
                    convert_elf: false,
                    pe_machine: 0x5064,
                    strict_loong_pe: false,
                })
            }
            LOONGARCH64_UEFI_TARGET => {
                let resolved = crate::build::uefi_build_target_for(target)
                    .context("LoongArch UEFI target resolver returned no target")?;
                Ok(Self {
                    cargo_target: resolved.target,
                    cargo_args: resolved.cargo_args,
                    rustflags: resolved.rustflags,
                    env: resolved.env,
                    convert_elf: true,
                    pe_machine: PE_MACHINE_LOONGARCH64,
                    strict_loong_pe: true,
                })
            }
            _ => bail!("unsupported axloader UEFI target `{target}`"),
        }
    }
}

fn append_target_rustflags(args: &mut Vec<String>, plan: &LoaderBuildPlan) {
    if plan.rustflags.is_empty() {
        return;
    }
    let target_key = Path::new(&plan.cargo_target)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(&plan.cargo_target);
    let rustflags = toml::Value::Array(
        plan.rustflags
            .iter()
            .cloned()
            .map(toml::Value::String)
            .collect(),
    )
    .to_string();
    args.push("--config".into());
    args.push(format!("target.{target_key}.rustflags={rustflags}"));
}

pub(crate) fn run_cargo<'a>(
    workspace_root: &Path,
    target_dir: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> anyhow::Result<()> {
    let mut args: Vec<String> = args.into_iter().map(str::to_string).collect();
    let target = args
        .iter()
        .position(|arg| arg == "--target")
        .and_then(|index| {
            crate::build::uefi_build_target_for(&args[index + 1]).map(|target| (index, target))
        });
    let mut command = StdCommand::new("cargo");
    if let Some((index, target)) = target {
        args[index + 1] = target.target;
        args.extend(target.cargo_args);
        command.envs(target.env);
        if let Some(mut flags) = std::env::var_os("CARGO_ENCODED_RUSTFLAGS") {
            for flag in target.rustflags {
                flags.push("\x1f");
                flags.push(flag);
            }
            command.env("CARGO_ENCODED_RUSTFLAGS", flags);
        } else {
            let mut flags = std::env::var("RUSTFLAGS").unwrap_or_default();
            for flag in target.rustflags {
                flags.push(' ');
                flags.push_str(&flag);
            }
            command.env("RUSTFLAGS", flags);
        }
    }
    command
        .current_dir(workspace_root)
        .args(args)
        .arg("--target-dir")
        .arg(target_dir);
    command.exec()
}
