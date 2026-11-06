use std::{fs, path::Path, process::Command as StdCommand};

use anyhow::{Context, ensure};
use clap::{Args, Subcommand};
use object::{Object, ObjectSection, ObjectSegment};

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
    /// Run axloader's host-side contract tests.
    Host,
    /// Run axloader host checks and persistent QEMU FAT scenarios
    Qemu(ArgsTestQemu),
}

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct ArgsTestQemu {
    #[arg(long, default_value = DEFAULT_UEFI_TARGET)]
    pub target: String,

    /// Start directly with an assigned v6 OTA on the persistent FAT disk.
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
        TestCommand::Host => run_host_checks(workspace),
        TestCommand::Qemu(args) => test_qemu(workspace, args).await,
    }
}

fn run_host_checks(workspace: &crate::context::WorkspaceContext) -> anyhow::Result<()> {
    run_cargo(
        workspace.root(),
        workspace.target_dir(),
        ["test", "-p", AXLOADER_PACKAGE, "--all-targets"],
    )
}

async fn test_qemu(
    workspace: &crate::context::WorkspaceContext,
    args: ArgsTestQemu,
) -> anyhow::Result<()> {
    run_host_checks(workspace)?;
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
    ota_qemu::test_direct_ota(workspace, &args.target, args.server_only).await
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
        args.extend(["--features", "uefi-launcher"]);
    }
    if release {
        args.push("--release");
    }
    run_cargo(workspace_root, target_dir, args)?;
    if target == crate::build::RISCV64_UEFI_TARGET {
        let directory = target_dir
            .join(target)
            .join(if release { "release" } else { "debug" });
        flatten_riscv_efi(
            &directory.join("axloader.elf"),
            &directory.join("axloader.efi"),
        )?;
    }
    Ok(())
}

fn run_cargo<'a>(
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

fn flatten_riscv_efi(elf_path: &Path, efi_path: &Path) -> anyhow::Result<()> {
    let bytes = fs::read(elf_path)?;
    let elf = object::File::parse(bytes.as_slice()).context("invalid EFI ELF artifact")?;
    ensure!(
        elf.architecture() == object::Architecture::Riscv64
            && elf.is_little_endian()
            && elf.kind() == object::ObjectKind::Dynamic,
        "RISC-V EFI must be a little-endian ELF64 PIE"
    );
    let mut segments = elf.segments();
    let image = segments.next().context("EFI image has no load segment")?;
    ensure!(
        image.address() == 0 && segments.next().is_none(),
        "EFI ELF must have one contiguous image at link address zero"
    );
    let data_start = elf
        .section_by_name(".data")
        .context("EFI image has no writable data")?
        .address();
    if let Some(relocations) = elf.dynamic_relocations() {
        for (address, relocation) in relocations {
            ensure!(
                relocation.flags() == object::RelocationFlags::Elf { r_type: 3 }
                    && relocation.target() == object::RelocationTarget::Absolute
                    && address % 8 == 0
                    && address >= data_start
                    && address
                        .checked_add(8)
                        .is_some_and(|end| end <= image.size())
                    && relocation.addend() >= 0
                    && relocation.addend() as u64 <= image.size(),
                "EFI image contains an unsupported relocation at {address:#x}: {relocation:?}"
            );
        }
    }

    let output = StdCommand::new("rustc")
        .args(["--print", "target-libdir"])
        .output()
        .context("failed to locate the Rust toolchain LLVM tools")?;
    ensure!(
        output.status.success(),
        "rustc --print target-libdir failed"
    );
    let library = String::from_utf8(output.stdout).context("invalid Rust toolchain path")?;
    let objcopy = Path::new(library.trim())
        .parent()
        .context("Rust library directory has no parent")?
        .join("bin")
        .join(format!("llvm-objcopy{}", std::env::consts::EXE_SUFFIX));
    StdCommand::new(objcopy)
        .args(["-O", "binary"])
        .arg(elf_path)
        .arg(efi_path)
        .exec()?;
    validate_riscv_pe(&fs::read(efi_path)?, elf.entry(), image.size())?;
    log::info!("RISC-V EFI artifact: {}", efi_path.display());
    Ok(())
}

fn validate_riscv_pe(bytes: &[u8], entry: u64, image_size: u64) -> anyhow::Result<()> {
    use object::read::pe::{ImageNtHeaders, ImageOptionalHeader, PeFile64};

    let pe = PeFile64::parse(bytes).context("invalid RISC-V PE32+ artifact")?;
    let headers = pe.nt_headers();
    let optional = headers.optional_header();
    ensure!(
        headers.file_header().machine.get(object::LittleEndian) == 0x5064
            && optional.image_base() == 0
            && optional.subsystem() == object::pe::IMAGE_SUBSYSTEM_EFI_APPLICATION
            && u64::from(optional.size_of_image()) == image_size
            && u64::from(optional.address_of_entry_point()) == entry,
        "RISC-V PE header does not describe the linked ELF image"
    );
    let mut previous_end = u64::from(optional.size_of_headers());
    let mut executable_entry = false;
    for section in pe.sections() {
        let (offset, size) = section
            .file_range()
            .context("EFI section has no file range")?;
        let end = section
            .address()
            .checked_add(section.size())
            .context("EFI section virtual range overflows")?;
        ensure!(
            offset == section.address()
                && offset >= previous_end
                && offset % u64::from(optional.file_alignment()) == 0
                && offset
                    .checked_add(size)
                    .is_some_and(|end| end <= bytes.len() as u64)
                && end <= image_size,
            "EFI section has an invalid flat-image layout"
        );
        if let object::SectionFlags::Coff { characteristics } = section.flags() {
            executable_entry |= characteristics & object::pe::IMAGE_SCN_MEM_EXECUTE != 0
                && entry >= section.address()
                && entry < end;
        }
        previous_end = end;
    }
    ensure!(executable_entry, "EFI entry is outside executable sections");
    Ok(())
}
