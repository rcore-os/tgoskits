//! LoongArch embedded-PE ELF and relocation validation.

use std::{fs, path::Path};

use anyhow::{Context, bail, ensure};

use super::{convert_elf_to_binary, read_u16, read_u32, read_u64};

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

pub(in crate::axloader) fn convert_elf_to_efi(
    output_dir: &Path,
    binary: &str,
) -> anyhow::Result<Vec<u64>> {
    let elf = output_dir.join(binary);
    ensure!(
        elf.is_file(),
        "cargo did not produce ELF at {}",
        elf.display()
    );
    let relocations = validate_loong_elf(&elf)?;
    let efi = output_dir.join(format!("{binary}.efi"));
    convert_elf_to_binary(&elf, &efi)?;
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

pub(super) fn validate_pe_header(
    image: &[u8],
    path: &Path,
    optional: usize,
    optional_size: usize,
) -> anyhow::Result<()> {
    ensure!(
        read_u64(image, optional + 24)? == 0,
        "{}: LoongArch EFI ImageBase must be zero",
        path.display()
    );
    let directory_count = read_u32(image, optional + 108)?;
    if directory_count > 5 {
        ensure!(
            optional
                .checked_add(112 + 5 * 8 + 8)
                .is_some_and(|end| end <= optional + optional_size),
            "{}: truncated PE base-relocation directory",
            path.display()
        );
        ensure!(
            read_u32(image, optional + 112 + 5 * 8)? == 0
                && read_u32(image, optional + 112 + 5 * 8 + 4)? == 0,
            "{}: LoongArch EFI base relocations are unsupported",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn validate_relocation_targets(
    path: &Path,
    targets: &[u64],
    image_size: usize,
    writable_sections: &[(usize, usize)],
) -> anyhow::Result<()> {
    for target in targets {
        let target = usize::try_from(*target).context("relative relocation target overflows")?;
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
    Ok(())
}
