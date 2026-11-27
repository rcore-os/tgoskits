//! EFI artifact conversion tools and shared PE layout checks.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as StdCommand,
};

use anyhow::{Context, ensure};

use crate::support::process::ProcessExt;

mod loongarch64;
mod riscv64;

pub(super) use loongarch64::convert_elf_to_efi;
pub(super) use riscv64::flatten_riscv_efi;

const PE32_PLUS_MAGIC: u16 = 0x020b;
const PE_SUBSYSTEM_EFI_APPLICATION: u16 = 10;
const PE_SECTION_WRITE: u32 = 0x8000_0000;

fn convert_elf_to_binary(elf: &Path, efi: &Path) -> anyhow::Result<()> {
    StdCommand::new(llvm_objcopy()?)
        .args(["-O", "binary"])
        .arg(elf)
        .arg(efi)
        .exec()
        .with_context(|| format!("failed to convert {} to EFI", elf.display()))
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

pub(super) fn validate_efi(
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
        loongarch64::validate_pe_header(&image, path, optional, optional_size)?;
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
        loongarch64::validate_relocation_targets(path, targets, image_size, &writable_sections)?;
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
