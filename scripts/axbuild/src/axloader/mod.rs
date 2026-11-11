use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use anyhow::{Context, bail, ensure};
use clap::{Args, Subcommand};

use crate::support::{ovmf::OvmfFirmware, process::ProcessExt};

mod ota_qemu;

const AXLOADER_PACKAGE: &str = "axloader";
const AXLOADER_BIN: &str = "axloader";
const LAUNCHER_BIN: &str = "axloader-launcher";
const DEFAULT_UEFI_TARGET: &str = "x86_64-unknown-uefi";
const LOONGARCH64_UEFI_TARGET: &str = "loongarch64-unknown-uefi";
const PE_MACHINE_X86_64: u16 = 0x8664;
const PE_MACHINE_LOONGARCH64: u16 = 0x6264;
const PE32_PLUS_MAGIC: u16 = 0x020b;
const PE_SUBSYSTEM_EFI_APPLICATION: u16 = 10;
const PE_SECTION_WRITE: u32 = 0x8000_0000;
const ELF_MACHINE_LOONGARCH64: u16 = 258;
const ELF_TYPE_DYN: u16 = 3;
const ELF_PROGRAM_HEADER_SIZE: usize = 56;
const ELF_SECTION_HEADER_SIZE: usize = 64;
const ELF_RELA_SIZE: usize = 24;
const PT_LOAD: u32 = 1;
const SHT_RELA: u32 = 4;
const SHT_REL: u32 = 9;
const SHT_RELR: u32 = 19;
const SHF_ALLOC: u64 = 2;
const R_LARCH_RELATIVE: u32 = 3;
const ELF64_WORD_SIZE: u64 = 8;

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

    /// Start directly with an assigned v6 OTA on the persistent FAT disk.
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
    run_loader_check(workspace.root(), workspace.target_dir(), &args.target)?;

    run_loader_build(
        workspace.root(),
        workspace.target_dir(),
        &args.target,
        true,
        true,
    )?;
    let firmware = match (args.firmware_code, args.firmware_vars) {
        (Some(code), Some(vars)) => Some(OvmfFirmware::from_paths(code, vars)?),
        (None, None) => None,
        _ => bail!("--firmware-code and --firmware-vars must be supplied together"),
    };
    ota_qemu::test_direct_ota(workspace, &args.target, args.server_only, firmware).await
}

fn run_loader_check(workspace_root: &Path, target_dir: &Path, target: &str) -> anyhow::Result<()> {
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

fn run_loader_build(
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
        "--bin".to_string(),
        LAUNCHER_BIN.to_string(),
        "--features".to_string(),
        if inject_202_write_failure {
            "uefi-launcher,qemu-fail-202-write"
        } else {
            "uefi-launcher"
        }
        .to_string(),
    ];
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
            LOONGARCH64_UEFI_TARGET => {
                let resolved = crate::build::bare_build_target_for(target)
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

fn convert_elf_to_efi(output_dir: &Path, binary: &str) -> anyhow::Result<Vec<u64>> {
    let elf = output_dir.join(binary);
    ensure!(
        elf.is_file(),
        "cargo did not produce ELF at {}",
        elf.display()
    );
    let relocations = validate_loong_elf(&elf)?;
    let efi = output_dir.join(format!("{binary}.efi"));
    let mut command = StdCommand::new(llvm_objcopy()?);
    command.args(["-O", "binary"]).arg(&elf).arg(&efi);
    command
        .exec()
        .with_context(|| format!("failed to convert {} to EFI", elf.display()))?;
    Ok(relocations)
}

fn validate_loong_elf(path: &Path) -> anyhow::Result<Vec<u64>> {
    let image = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    ensure!(
        image.len() >= 64,
        "{}: truncated ELF header",
        path.display()
    );
    ensure!(
        &image[..4] == b"\x7fELF",
        "{}: invalid ELF magic",
        path.display()
    );
    ensure!(
        image[4] == 2 && image[5] == 1,
        "{}: expected little-endian ELF64",
        path.display()
    );
    ensure!(
        read_u16(&image, 16)? == ELF_TYPE_DYN,
        "{}: LoongArch UEFI image must be ET_DYN",
        path.display()
    );
    ensure!(
        read_u16(&image, 18)? == ELF_MACHINE_LOONGARCH64,
        "{}: unexpected ELF machine",
        path.display()
    );

    let program_offset =
        usize::try_from(read_u64(&image, 32)?).context("ELF program table overflows")?;
    let program_entry_size = usize::from(read_u16(&image, 54)?);
    let program_count = usize::from(read_u16(&image, 56)?);
    ensure!(
        program_count != 0
            && program_entry_size == ELF_PROGRAM_HEADER_SIZE
            && program_offset
                .checked_add(
                    program_count
                        .checked_mul(program_entry_size)
                        .context("ELF program table overflows")?,
                )
                .is_some_and(|end| end <= image.len()),
        "{}: invalid ELF program table",
        path.display()
    );

    let section_offset =
        usize::try_from(read_u64(&image, 40)?).context("ELF section table overflows")?;
    let section_entry_size = usize::from(read_u16(&image, 58)?);
    let section_count = usize::from(read_u16(&image, 60)?);
    ensure!(
        section_count != 0
            && section_entry_size >= ELF_SECTION_HEADER_SIZE
            && section_offset
                .checked_add(
                    section_count
                        .checked_mul(section_entry_size)
                        .context("ELF section table overflows")?,
                )
                .is_some_and(|end| end <= image.len()),
        "{}: invalid ELF section table",
        path.display()
    );

    let mut relocation_targets = Vec::new();
    for index in 0..section_count {
        let section = section_offset + index * section_entry_size;
        let section_type = read_u32(&image, section + 4)?;
        let flags = read_u64(&image, section + 8)?;
        let address = read_u64(&image, section + 16)?;
        let data_offset = read_u64(&image, section + 24)?;
        let data_size = read_u64(&image, section + 32)?;
        let entry_size = read_u64(&image, section + 56)?;
        match section_type {
            SHT_REL if data_size != 0 => {
                bail!("{}: unsupported SHT_REL relocation section", path.display());
            }
            SHT_RELR if data_size != 0 => {
                bail!("{}: RELR relocations are unsupported", path.display());
            }
            SHT_RELA => {
                let offset =
                    usize::try_from(data_offset).context("ELF relocation offset overflows")?;
                let size = usize::try_from(data_size).context("ELF relocation size overflows")?;
                let end = offset
                    .checked_add(size)
                    .context("ELF relocation range overflows")?;
                ensure!(
                    end <= image.len()
                        && size % ELF_RELA_SIZE == 0
                        && (entry_size == 0 || entry_size == ELF_RELA_SIZE as u64),
                    "{}: invalid ELF RELA section",
                    path.display()
                );
                if flags & SHF_ALLOC != 0 && size != 0 {
                    ensure!(
                        elf_range_is_loaded(
                            &image,
                            program_offset,
                            program_entry_size,
                            program_count,
                            address,
                            data_offset,
                            data_size,
                        )?,
                        "{}: allocatable RELA section is not loaded",
                        path.display()
                    );
                }
                for relocation in (offset..end).step_by(ELF_RELA_SIZE) {
                    let target = read_u64(&image, relocation)?;
                    let info = read_u64(&image, relocation + 8)?;
                    ensure!(
                        info as u32 == R_LARCH_RELATIVE && info >> 32 == 0,
                        "{}: unsupported ELF relocation type",
                        path.display()
                    );
                    ensure!(
                        target % ELF64_WORD_SIZE == 0
                            && target.checked_add(ELF64_WORD_SIZE).is_some(),
                        "{}: invalid relative relocation target",
                        path.display()
                    );
                    relocation_targets.push(target);
                }
            }
            _ => {}
        }
    }
    Ok(relocation_targets)
}

fn elf_range_is_loaded(
    image: &[u8],
    program_offset: usize,
    program_entry_size: usize,
    program_count: usize,
    address: u64,
    file_offset: u64,
    size: u64,
) -> anyhow::Result<bool> {
    let address_end = address
        .checked_add(size)
        .context("ELF section address range overflows")?;
    let file_end = file_offset
        .checked_add(size)
        .context("ELF section file range overflows")?;
    for index in 0..program_count {
        let header = program_offset + index * program_entry_size;
        if read_u32(image, header)? != PT_LOAD {
            continue;
        }
        let load_file_offset = read_u64(image, header + 8)?;
        let load_address = read_u64(image, header + 16)?;
        let load_file_size = read_u64(image, header + 32)?;
        let load_memory_size = read_u64(image, header + 40)?;
        let Some(load_file_end) = load_file_offset.checked_add(load_file_size) else {
            continue;
        };
        let Some(load_memory_end) = load_address.checked_add(load_memory_size) else {
            continue;
        };
        if file_offset >= load_file_offset
            && file_end <= load_file_end
            && address >= load_address
            && address_end <= load_memory_end
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn llvm_objcopy() -> anyhow::Result<PathBuf> {
    let output = StdCommand::new("rustc")
        .args(["--print", "target-libdir"])
        .output()
        .context("failed to locate the Rust toolchain LLVM tools")?;
    ensure!(
        output.status.success(),
        "rustc --print target-libdir failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let library = String::from_utf8(output.stdout).context("invalid Rust toolchain path")?;
    let tool = Path::new(library.trim())
        .parent()
        .context("Rust target library directory has no parent")?
        .join("bin")
        .join(format!("llvm-objcopy{}", std::env::consts::EXE_SUFFIX));
    ensure!(
        tool.is_file(),
        "Rust llvm-tools component is missing: {}",
        tool.display()
    );
    Ok(tool)
}

fn validate_efi(
    path: &Path,
    expected_machine: u16,
    strict_loong: bool,
    relocation_targets: Option<&[u64]>,
) -> anyhow::Result<()> {
    let image = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    ensure!(
        image.len() >= 0x40,
        "{}: truncated DOS header",
        path.display()
    );
    ensure!(
        &image[..2] == b"MZ",
        "{}: invalid MZ signature",
        path.display()
    );
    let pe_offset =
        usize::try_from(read_u32(&image, 0x3c)?).context("PE offset does not fit the host")?;
    ensure!(
        pe_offset
            .checked_add(24 + 112)
            .is_some_and(|end| end <= image.len()),
        "{}: truncated PE/optional header",
        path.display()
    );
    ensure!(
        read_u32(&image, pe_offset)? == 0x0000_4550,
        "{}: invalid PE signature",
        path.display()
    );
    ensure!(
        read_u16(&image, pe_offset + 4)? == expected_machine,
        "{}: unexpected PE machine",
        path.display()
    );
    let section_count = usize::from(read_u16(&image, pe_offset + 6)?);
    let optional_size = usize::from(read_u16(&image, pe_offset + 20)?);
    let optional = pe_offset + 24;
    ensure!(
        optional_size >= 112 && optional + optional_size <= image.len(),
        "{}: invalid optional header size",
        path.display()
    );
    ensure!(
        read_u16(&image, optional)? == PE32_PLUS_MAGIC,
        "{}: expected PE32+",
        path.display()
    );
    ensure!(
        read_u16(&image, optional + 68)? == PE_SUBSYSTEM_EFI_APPLICATION,
        "{}: expected EFI application subsystem",
        path.display()
    );
    let image_size =
        usize::try_from(read_u32(&image, optional + 56)?).context("PE image size overflows")?;
    let headers =
        usize::try_from(read_u32(&image, optional + 60)?).context("PE header size overflows")?;
    let section_alignment = read_u32(&image, optional + 32)?;
    let file_alignment =
        usize::try_from(read_u32(&image, optional + 36)?).context("PE file alignment overflows")?;
    let entry = usize::try_from(read_u32(&image, optional + 16)?).context("PE entry overflows")?;
    ensure!(
        image_size != 0 && headers != 0 && headers <= image.len() && entry >= headers,
        "{}: invalid PE image layout",
        path.display()
    );
    ensure!(
        section_alignment.is_power_of_two()
            && file_alignment.is_power_of_two()
            && (0x200..=0x1_0000).contains(&file_alignment),
        "{}: invalid PE alignment",
        path.display()
    );
    if strict_loong {
        ensure!(
            read_u64(&image, optional + 24)? == 0,
            "{}: LoongArch EFI ImageBase must be zero",
            path.display()
        );
        let directory_count = read_u32(&image, optional + 108)?;
        if directory_count > 5 {
            ensure!(
                optional
                    .checked_add(112 + 5 * 8 + 8)
                    .is_some_and(|end| end <= optional + optional_size),
                "{}: truncated PE base-relocation directory",
                path.display()
            );
            ensure!(
                read_u32(&image, optional + 112 + 5 * 8)? == 0
                    && read_u32(&image, optional + 112 + 5 * 8 + 4)? == 0,
                "{}: LoongArch EFI base relocations are unsupported",
                path.display()
            );
        }
    }
    let section_table = optional + optional_size;
    ensure!(
        section_count
            .checked_mul(40)
            .and_then(|size| section_table.checked_add(size))
            .is_some_and(|end| end <= image.len()),
        "{}: truncated PE section table",
        path.display()
    );
    let mut entry_is_executable = false;
    let mut section_ranges = Vec::with_capacity(section_count);
    let mut writable_sections = Vec::new();
    for index in 0..section_count {
        let section = section_table + index * 40;
        let virtual_size = usize::try_from(read_u32(&image, section + 8)?)?;
        let virtual_address = usize::try_from(read_u32(&image, section + 12)?)?;
        let raw_size = usize::try_from(read_u32(&image, section + 16)?)?;
        let raw_offset = usize::try_from(read_u32(&image, section + 20)?)?;
        let characteristics = read_u32(&image, section + 36)?;
        ensure!(
            raw_size % file_alignment == 0 && raw_offset % file_alignment == 0,
            "{}: EFI raw section is not file aligned",
            path.display()
        );
        ensure!(
            raw_offset
                .checked_add(raw_size)
                .is_some_and(|end| end <= image.len()),
            "{}: EFI raw section is truncated",
            path.display()
        );
        let virtual_end = virtual_address
            .checked_add(virtual_size.max(raw_size))
            .context("EFI section range overflows")?;
        ensure!(
            virtual_address >= headers && virtual_end <= image_size,
            "{}: EFI section is outside the image",
            path.display()
        );
        if characteristics & 0x2000_0000 != 0 && entry >= virtual_address && entry < virtual_end {
            entry_is_executable = true;
        }
        section_ranges.push((virtual_address, virtual_end));
        if characteristics & PE_SECTION_WRITE != 0 {
            writable_sections.push((virtual_address, virtual_end));
        }
    }
    section_ranges.sort_unstable_by_key(|range| range.0);
    ensure!(
        !section_ranges
            .windows(2)
            .any(|range| range[0].1 > range[1].0),
        "{}: EFI sections overlap",
        path.display()
    );
    ensure!(
        entry_is_executable,
        "{}: EFI entry is not inside executable code",
        path.display()
    );
    if let Some(targets) = relocation_targets {
        for target in targets {
            let target =
                usize::try_from(*target).context("relative relocation target overflows")?;
            let end = target
                .checked_add(ELF64_WORD_SIZE as usize)
                .context("relative relocation target overflows")?;
            ensure!(
                target % (ELF64_WORD_SIZE as usize) == 0
                    && end <= image_size
                    && writable_sections
                        .iter()
                        .any(|range| target >= range.0 && end <= range.1),
                "{}: relative relocation target is not in a writable image section",
                path.display()
            );
        }
    }
    Ok(())
}

fn read_u16(image: &[u8], offset: usize) -> anyhow::Result<u16> {
    let bytes = image
        .get(offset..offset + 2)
        .context("truncated EFI u16 field")?;
    Ok(u16::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u32(image: &[u8], offset: usize) -> anyhow::Result<u32> {
    let bytes = image
        .get(offset..offset + 4)
        .context("truncated EFI u32 field")?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn read_u64(image: &[u8], offset: usize) -> anyhow::Result<u64> {
    let bytes = image
        .get(offset..offset + 8)
        .context("truncated EFI u64 field")?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
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
