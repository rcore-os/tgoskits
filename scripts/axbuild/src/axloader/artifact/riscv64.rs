//! RISC-V flat EFI image conversion and its ELF/PE contract.

use std::{fs, path::Path};

use anyhow::{Context, ensure};
use object::{Object, ObjectSection, ObjectSegment};

use super::convert_elf_to_binary;

pub(in crate::axloader) fn flatten_riscv_efi(
    elf_path: &Path,
    efi_path: &Path,
) -> anyhow::Result<()> {
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

    convert_elf_to_binary(elf_path, efi_path)?;
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
