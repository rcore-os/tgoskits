use std::{path::Path, process::Command as StdCommand};

use clap::{Args, Subcommand};

use crate::support::process::ProcessExt;

mod ota_qemu;

const AXLOADER_PACKAGE: &str = "axloader";
const AXLOADER_BIN: &str = "axloader";
const LAUNCHER_BIN: &str = "axloader-launcher";
const DEFAULT_UEFI_TARGET: &str = "x86_64-unknown-uefi";

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
    /// Run axloader host checks and persistent QEMU FAT scenarios
    Qemu(ArgsTestQemu),
}

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct ArgsTestQemu {
    #[arg(long, default_value = DEFAULT_UEFI_TARGET)]
    pub target: String,

    /// Start directly with an assigned v5 OTA on the persistent FAT disk.
    #[arg(long)]
    pub server_only: bool,
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
    )
}

async fn test_with_context(
    workspace: &crate::context::WorkspaceContext,
    args: ArgsTest,
) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(args) => test_qemu(workspace, args).await,
    }
}

async fn test_qemu(
    workspace: &crate::context::WorkspaceContext,
    args: ArgsTestQemu,
) -> anyhow::Result<()> {
    run_cargo(
        workspace.root(),
        workspace.target_dir(),
        ["test", "-p", AXLOADER_PACKAGE, "--all-targets"],
    )?;
    let result = run_cargo(
        workspace.root(),
        workspace.target_dir(),
        [
            "check",
            "-p",
            AXLOADER_PACKAGE,
            "--target",
            args.target.as_str(),
            "--bin",
            AXLOADER_BIN,
        ],
    );
    result?;

    run_loader_build(workspace.root(), workspace.target_dir(), &args.target, true)?;
    ota_qemu::test_direct_ota(
        workspace.root(),
        workspace.target_dir(),
        &args.target,
        args.server_only,
    )
    .await
}

fn run_loader_build(
    workspace_root: &Path,
    target_dir: &Path,
    target: &str,
    release: bool,
) -> anyhow::Result<()> {
    let mut args = vec![
        "build",
        "-p",
        AXLOADER_PACKAGE,
        "--target",
        target,
        "--bin",
        AXLOADER_BIN,
    ];
    if target == DEFAULT_UEFI_TARGET {
        args.extend(["--bin", LAUNCHER_BIN]);
    }
    if release {
        args.push("--release");
    }
    run_cargo(workspace_root, target_dir, args)
}

fn run_cargo<'a>(
    workspace_root: &Path,
    target_dir: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> anyhow::Result<()> {
    let mut command = StdCommand::new("cargo");
    command
        .current_dir(workspace_root)
        .args(args)
        .arg("--target-dir")
        .arg(target_dir);
    command.exec()
}

fn minimal_x86_64_kernel_elf() -> Vec<u8> {
    const EHDR_SIZE: usize = 64;
    const PHDR_SIZE: usize = 56;
    const LOAD_OFFSET: usize = 0x1000;
    const LOAD_ADDR: u64 = 0x20_0000;
    const LOAD_MEM_SIZE: u64 = 0x1000;
    let code = [0xeb, 0xfe]; // jmp .
    let mut image = vec![0; LOAD_OFFSET + code.len()];

    image[0..4].copy_from_slice(b"\x7fELF");
    image[4] = 2;
    image[5] = 1;
    image[6] = 1;
    put_u16(&mut image, 16, 2);
    put_u16(&mut image, 18, 62);
    put_u32(&mut image, 20, 1);
    put_u64(&mut image, 24, LOAD_ADDR);
    put_u64(&mut image, 32, EHDR_SIZE as u64);
    put_u16(&mut image, 52, EHDR_SIZE as u16);
    put_u16(&mut image, 54, PHDR_SIZE as u16);
    put_u16(&mut image, 56, 1);

    let ph = EHDR_SIZE;
    put_u32(&mut image, ph, 1);
    put_u32(&mut image, ph + 4, 5);
    put_u64(&mut image, ph + 8, LOAD_OFFSET as u64);
    put_u64(&mut image, ph + 16, LOAD_ADDR);
    put_u64(&mut image, ph + 24, LOAD_ADDR);
    put_u64(&mut image, ph + 32, code.len() as u64);
    put_u64(&mut image, ph + 40, LOAD_MEM_SIZE);
    put_u64(&mut image, ph + 48, 0x1000);

    image[LOAD_OFFSET..LOAD_OFFSET + code.len()].copy_from_slice(&code);
    image
}

fn put_u16(image: &mut [u8], offset: usize, value: u16) {
    image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(image: &mut [u8], offset: usize, value: u32) {
    image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(image: &mut [u8], offset: usize, value: u64) {
    image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
