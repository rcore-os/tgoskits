//! LoongArch Linux ELF direct loader.

use core::ops::Range;

use crate::{AxVmResult, GuestPhysAddr, ax_err_type, boot::images::*};

const COMMAND_LINE_SIZE: usize = 512;
const BOOT_INFO_SIZE: usize = 0x10_0000;
const SYSTEM_TABLE_BASE: usize = 0x1_0000;
const CONFIG_TABLE_BASE: usize = 0x2_0000;
const BOOT_MEMMAP_BASE: usize = 0x3_0000;
const INITRD_TABLE_BASE: usize = 0x4_0000;
const FDT_BASE: u64 = 0x10_0000;
const EFI_SYSTEM_TABLE_SIGNATURE: u64 = 0x5453_5953_2049_4249;
const EFI_SYSTEM_TABLE_REVISION: u32 = (2 << 16) | 100;
const EFI_SYSTEM_TABLE_SIZE: u32 = 120;
const EFI_MEMORY_DESCRIPTOR_SIZE: usize = 40;
const EFI_LOADER_CODE: u32 = 1;
const EFI_PAGE_SIZE: u64 = 4096;
const LINUX_EFI_BOOT_MEMMAP_GUID: [u8; 16] = [
    0x3f, 0x68, 0x0f, 0x80, 0x8b, 0xd0, 0x3a, 0x42, 0xa2, 0x93, 0x96, 0x5c, 0x3c, 0x6f, 0xe2, 0xb4,
];
const LINUX_EFI_INITRD_MEDIA_GUID: [u8; 16] = [
    0x27, 0xe4, 0x68, 0x55, 0xfc, 0x68, 0x3d, 0x4f, 0xac, 0x74, 0xca, 0x55, 0x52, 0x31, 0xcc, 0x68,
];
const DEVICE_TREE_GUID: [u8; 16] = [
    0xd5, 0x21, 0xb6, 0xb1, 0x9c, 0xf1, 0xa5, 0x41, 0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa, 0xe0,
];

const ELF_HEADER_SIZE: usize = 64;
const ELF_PROGRAM_HEADER_SIZE: usize = 56;
const ELF_MACHINE_LOONGARCH: u16 = 258;
const PT_LOAD: u32 = 1;
const LOONGARCH_DMW_PREFIX: u64 = 0x9000_0000_0000_0000;
const LOONGARCH_DMW_MASK: u64 = 0x0000_ffff_ffff_ffff;

pub(super) const fn boot_args() -> [usize; 3] {
    [1, 0, SYSTEM_TABLE_BASE]
}

pub(super) fn load_boot_info(
    loader: &ImageLoaderCore<'_>,
    initrd: Option<(u64, u64)>,
) -> AxVmResult {
    let cmdline = loader.config.kernel.cmdline.as_deref().unwrap_or("");
    if cmdline.len() >= COMMAND_LINE_SIZE {
        return Err(ax_err_type!(
            InvalidData,
            "LoongArch Linux command line exceeds 511 bytes"
        ));
    }

    let mut boot_info = std::vec![0; BOOT_INFO_SIZE];
    boot_info[..cmdline.len()].copy_from_slice(cmdline.as_bytes());

    write_u64_at(
        &mut boot_info,
        SYSTEM_TABLE_BASE,
        EFI_SYSTEM_TABLE_SIGNATURE,
    );
    write_u32_at(
        &mut boot_info,
        SYSTEM_TABLE_BASE + 8,
        EFI_SYSTEM_TABLE_REVISION,
    );
    write_u32_at(
        &mut boot_info,
        SYSTEM_TABLE_BASE + 12,
        EFI_SYSTEM_TABLE_SIZE,
    );

    let config_table_count = if initrd.is_some() { 3 } else { 2 };
    write_u64_at(&mut boot_info, SYSTEM_TABLE_BASE + 104, config_table_count);
    write_u64_at(
        &mut boot_info,
        SYSTEM_TABLE_BASE + 112,
        CONFIG_TABLE_BASE as u64,
    );

    let mut config_table_index = 0;
    write_config_table(
        &mut boot_info,
        config_table_index,
        LINUX_EFI_BOOT_MEMMAP_GUID,
        BOOT_MEMMAP_BASE as u64,
    );
    config_table_index += 1;
    if let Some((base, size)) = initrd {
        write_config_table(
            &mut boot_info,
            config_table_index,
            LINUX_EFI_INITRD_MEDIA_GUID,
            INITRD_TABLE_BASE as u64,
        );
        config_table_index += 1;
        write_u64_at(&mut boot_info, INITRD_TABLE_BASE, base);
        write_u64_at(&mut boot_info, INITRD_TABLE_BASE + 8, size);
    }
    write_config_table(
        &mut boot_info,
        config_table_index,
        DEVICE_TREE_GUID,
        FDT_BASE,
    );

    let regions = super::ram_regions(&loader.vm);
    let map_size = regions
        .len()
        .checked_mul(EFI_MEMORY_DESCRIPTOR_SIZE)
        .ok_or_else(|| ax_err_type!(InvalidData, "LoongArch EFI memory map is too large"))?;
    let map_end = BOOT_MEMMAP_BASE
        .checked_add(40)
        .and_then(|start| start.checked_add(map_size))
        .ok_or_else(|| ax_err_type!(InvalidData, "LoongArch EFI memory map overflows"))?;
    if map_end > INITRD_TABLE_BASE {
        return Err(ax_err_type!(
            InvalidData,
            "LoongArch EFI memory map exceeds its boot-info slot"
        ));
    }
    write_u64_at(&mut boot_info, BOOT_MEMMAP_BASE, map_size as u64);
    write_u64_at(
        &mut boot_info,
        BOOT_MEMMAP_BASE + 8,
        EFI_MEMORY_DESCRIPTOR_SIZE as u64,
    );
    write_u32_at(&mut boot_info, BOOT_MEMMAP_BASE + 16, 1);
    write_u64_at(&mut boot_info, BOOT_MEMMAP_BASE + 32, map_size as u64);
    for (index, region) in regions.into_iter().enumerate() {
        let descriptor = BOOT_MEMMAP_BASE + 40 + index * EFI_MEMORY_DESCRIPTOR_SIZE;
        write_u32_at(&mut boot_info, descriptor, EFI_LOADER_CODE);
        write_u64_at(&mut boot_info, descriptor + 8, region.base);
        write_u64_at(&mut boot_info, descriptor + 24, region.size / EFI_PAGE_SIZE);
    }

    load_vm_image_from_memory(&boot_info, GuestPhysAddr::from(0), loader.vm.clone())
}

fn write_config_table(image: &mut [u8], index: usize, guid: [u8; 16], table: u64) {
    let offset = CONFIG_TABLE_BASE + index * 24;
    image[offset..offset + guid.len()].copy_from_slice(&guid);
    write_u64_at(image, offset + 16, table);
}

fn write_u32_at(image: &mut [u8], offset: usize, value: u32) {
    image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64_at(image: &mut [u8], offset: usize, value: u64) {
    image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn load_elf(image: &[u8], loader: &ImageLoaderCore<'_>) -> AxVmResult {
    let image = ValidatedElf::parse(image)?;
    for segment in &image.segments {
        super::fill_vm_region(segment.load_gpa, segment.memory_size, 0, loader.vm.clone())?;
        load_vm_image_from_memory(
            &image.image[segment.file_range.clone()],
            segment.load_gpa,
            loader.vm.clone(),
        )?;
    }
    loader.vm.with_config(|config| {
        config.cpu_config.bsp_entry = image.entry;
        config.cpu_config.ap_entry = image.entry;
    });
    Ok(())
}

pub(super) fn validate_image_layout(
    kernel: &[u8],
    initrd: Option<(GuestPhysAddr, usize)>,
    fdt_size: usize,
) -> AxVmResult {
    let kernel = ValidatedElf::parse(kernel)?;
    let mut ranges = std::vec![
        ImageRange::new("boot information", 0, BOOT_INFO_SIZE as u64)?,
        ImageRange::new("device tree", FDT_BASE, fdt_size as u64)?,
    ];
    if let Some((start, size)) = initrd {
        ranges.push(ImageRange::new(
            "initramfs",
            start.as_usize() as u64,
            size as u64,
        )?);
    }
    for (index, segment) in kernel.segments.iter().enumerate() {
        ranges.push(ImageRange::new(
            std::format!("kernel segment {index}"),
            segment.load_gpa.as_usize() as u64,
            segment.memory_size as u64,
        )?);
    }

    for (index, range) in ranges.iter().enumerate() {
        if range.start == range.end {
            continue;
        }
        for other in &ranges[index + 1..] {
            if other.start != other.end && range.start < other.end && other.start < range.end {
                return Err(ax_err_type!(
                    InvalidData,
                    std::format!(
                        "LoongArch direct boot image ranges overlap: {} [{:#x}, {:#x}) and {} \
                         [{:#x}, {:#x})",
                        range.name,
                        range.start,
                        range.end,
                        other.name,
                        other.start,
                        other.end
                    )
                ));
            }
        }
    }
    Ok(())
}

struct ImageRange {
    name: std::string::String,
    start: u64,
    end: u64,
}

impl ImageRange {
    fn new(name: impl Into<std::string::String>, start: u64, size: u64) -> AxVmResult<Self> {
        let end = start.checked_add(size).ok_or_else(|| {
            ax_err_type!(InvalidData, "LoongArch direct boot image range overflows")
        })?;
        Ok(Self {
            name: name.into(),
            start,
            end,
        })
    }
}

struct ValidatedElf<'a> {
    image: &'a [u8],
    segments: std::vec::Vec<ValidatedLoadSegment>,
    entry: GuestPhysAddr,
}

struct ValidatedLoadSegment {
    load_gpa: GuestPhysAddr,
    memory_size: usize,
    file_range: Range<usize>,
}

impl<'a> ValidatedElf<'a> {
    fn parse(image: &'a [u8]) -> AxVmResult<Self> {
        let header = ElfHeader::parse(image)?;
        let mut segments = std::vec::Vec::new();
        let mut entry = None;
        for index in 0..header.program_header_count {
            let offset =
                header
                    .program_header_offset
                    .checked_add(index.checked_mul(header.program_header_size).ok_or_else(
                        || {
                            ax_err_type!(
                                InvalidData,
                                "LoongArch ELF program-header offset overflows"
                            )
                        },
                    )?)
                    .ok_or_else(|| {
                        ax_err_type!(InvalidData, "LoongArch ELF program-header offset overflows")
                    })?;
            let segment = LoadSegment::parse(image, offset)?;
            if segment.segment_type != PT_LOAD {
                continue;
            }
            let load_gpa = loongarch_guest_phys(segment.physical_address)?;
            let file_size = usize::try_from(segment.file_size)
                .map_err(|_| ax_err_type!(InvalidData, "LoongArch ELF segment is too large"))?;
            let memory_size = usize::try_from(segment.memory_size)
                .map_err(|_| ax_err_type!(InvalidData, "LoongArch ELF segment is too large"))?;
            if file_size > memory_size {
                return Err(ax_err_type!(
                    InvalidData,
                    "LoongArch ELF segment file size exceeds memory size"
                ));
            }
            load_gpa
                .as_usize()
                .checked_add(memory_size)
                .ok_or_else(|| {
                    ax_err_type!(InvalidData, "LoongArch ELF segment address range overflows")
                })?;
            let file_offset = usize::try_from(segment.file_offset).map_err(|_| {
                ax_err_type!(InvalidData, "LoongArch ELF segment offset is too large")
            })?;
            let file_end = file_offset.checked_add(file_size).ok_or_else(|| {
                ax_err_type!(InvalidData, "LoongArch ELF segment range overflows")
            })?;
            if image.get(file_offset..file_end).is_none() {
                return Err(ax_err_type!(
                    InvalidData,
                    "LoongArch ELF segment exceeds the kernel image"
                ));
            }
            let virtual_end = segment
                .virtual_address
                .checked_add(segment.memory_size)
                .ok_or_else(|| {
                    ax_err_type!(InvalidData, "LoongArch ELF virtual address range overflows")
                })?;
            if header.entry >= segment.virtual_address && header.entry < virtual_end {
                let entry_offset = usize::try_from(header.entry - segment.virtual_address)
                    .map_err(|_| {
                        ax_err_type!(InvalidData, "LoongArch ELF entry offset does not fit usize")
                    })?;
                entry = Some(GuestPhysAddr::from(
                    load_gpa
                        .as_usize()
                        .checked_add(entry_offset)
                        .ok_or_else(|| {
                            ax_err_type!(InvalidData, "LoongArch ELF entry address overflows")
                        })?,
                ));
            }
            segments.push(ValidatedLoadSegment {
                load_gpa,
                memory_size,
                file_range: file_offset..file_end,
            });
        }
        if segments.is_empty() {
            return Err(ax_err_type!(
                InvalidData,
                "LoongArch Linux ELF contains no loadable segment"
            ));
        }
        let entry = entry.ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                "LoongArch Linux ELF entry is outside every loadable segment"
            )
        })?;
        Ok(Self {
            image,
            segments,
            entry,
        })
    }
}

struct ElfHeader {
    entry: u64,
    program_header_offset: usize,
    program_header_size: usize,
    program_header_count: usize,
}

impl ElfHeader {
    fn parse(image: &[u8]) -> AxVmResult<Self> {
        if image.get(..4) != Some(b"\x7fELF") {
            return Err(ax_err_type!(
                InvalidData,
                "LoongArch direct kernel is not ELF"
            ));
        }
        if image.get(4) != Some(&2) || image.get(5) != Some(&1) || image.get(6) != Some(&1) {
            return Err(ax_err_type!(
                InvalidData,
                "LoongArch direct kernel must be a little-endian ELF64 image"
            ));
        }
        if read_u16(image, 18)? != ELF_MACHINE_LOONGARCH {
            return Err(ax_err_type!(
                InvalidData,
                "direct kernel ELF machine is not LoongArch"
            ));
        }
        let program_header_size = usize::from(read_u16(image, 54)?);
        if program_header_size != ELF_PROGRAM_HEADER_SIZE {
            return Err(ax_err_type!(
                InvalidData,
                "LoongArch ELF program-header size is unsupported"
            ));
        }
        Ok(Self {
            entry: read_u64(image, 24)?,
            program_header_offset: usize::try_from(read_u64(image, 32)?).map_err(|_| {
                ax_err_type!(
                    InvalidData,
                    "LoongArch ELF program-header offset is too large"
                )
            })?,
            program_header_size,
            program_header_count: usize::from(read_u16(image, 56)?),
        })
    }
}

struct LoadSegment {
    segment_type: u32,
    file_offset: u64,
    virtual_address: u64,
    physical_address: u64,
    file_size: u64,
    memory_size: u64,
}

impl LoadSegment {
    fn parse(image: &[u8], offset: usize) -> AxVmResult<Self> {
        let end = offset.checked_add(ELF_PROGRAM_HEADER_SIZE).ok_or_else(|| {
            ax_err_type!(InvalidData, "LoongArch ELF program-header range overflows")
        })?;
        if image.get(offset..end).is_none() {
            return Err(ax_err_type!(
                InvalidData,
                "LoongArch ELF program header exceeds the kernel image"
            ));
        }
        Ok(Self {
            segment_type: read_u32(image, offset)?,
            file_offset: read_u64(image, offset + 8)?,
            virtual_address: read_u64(image, offset + 16)?,
            physical_address: read_u64(image, offset + 24)?,
            file_size: read_u64(image, offset + 32)?,
            memory_size: read_u64(image, offset + 40)?,
        })
    }
}

fn loongarch_guest_phys(address: u64) -> AxVmResult<GuestPhysAddr> {
    let address = if address & !LOONGARCH_DMW_MASK == LOONGARCH_DMW_PREFIX {
        address & LOONGARCH_DMW_MASK
    } else if address & !LOONGARCH_DMW_MASK == 0 {
        address
    } else {
        return Err(ax_err_type!(
            InvalidData,
            "LoongArch ELF address is outside the physical and cached DMW windows"
        ));
    };
    usize::try_from(address)
        .map(GuestPhysAddr::from)
        .map_err(|_| ax_err_type!(InvalidData, "LoongArch ELF address does not fit usize"))
}

fn read_u16(image: &[u8], offset: usize) -> AxVmResult<u16> {
    let bytes = image
        .get(offset..offset + 2)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| ax_err_type!(InvalidData, "LoongArch ELF header is truncated"))?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(image: &[u8], offset: usize) -> AxVmResult<u32> {
    let bytes = image
        .get(offset..offset + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| ax_err_type!(InvalidData, "LoongArch ELF header is truncated"))?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(image: &[u8], offset: usize) -> AxVmResult<u64> {
    let bytes = image
        .get(offset..offset + 8)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| ax_err_type!(InvalidData, "LoongArch ELF header is truncated"))?;
    Ok(u64::from_le_bytes(bytes))
}

const _: () = assert!(ELF_HEADER_SIZE == 64);

#[cfg(test)]
mod tests {
    use super::*;

    fn direct_kernel(load_address: u64, memory_size: u64) -> std::vec::Vec<u8> {
        let file_offset = ELF_HEADER_SIZE + ELF_PROGRAM_HEADER_SIZE;
        let mut image = std::vec![0; file_offset + 4];
        image[..4].copy_from_slice(b"\x7fELF");
        image[4] = 2;
        image[5] = 1;
        image[6] = 1;
        image[18..20].copy_from_slice(&ELF_MACHINE_LOONGARCH.to_le_bytes());
        image[24..32].copy_from_slice(&load_address.to_le_bytes());
        image[32..40].copy_from_slice(&(ELF_HEADER_SIZE as u64).to_le_bytes());
        image[54..56].copy_from_slice(&(ELF_PROGRAM_HEADER_SIZE as u16).to_le_bytes());
        image[56..58].copy_from_slice(&1_u16.to_le_bytes());

        let program_header = ELF_HEADER_SIZE;
        image[program_header..program_header + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        image[program_header + 8..program_header + 16]
            .copy_from_slice(&(file_offset as u64).to_le_bytes());
        image[program_header + 16..program_header + 24]
            .copy_from_slice(&load_address.to_le_bytes());
        image[program_header + 24..program_header + 32]
            .copy_from_slice(&load_address.to_le_bytes());
        image[program_header + 32..program_header + 40].copy_from_slice(&4_u64.to_le_bytes());
        image[program_header + 40..program_header + 48].copy_from_slice(&memory_size.to_le_bytes());
        image
    }

    #[test]
    fn direct_boot_layout_rejects_overlapping_images() {
        let valid_kernel = direct_kernel(0x20_0000, 0x10_0000);
        validate_image_layout(
            &valid_kernel,
            Some((GuestPhysAddr::from(0x800_0000), 0x10_0000)),
            0x1_0000,
        )
        .expect("separate direct-boot images should be accepted");

        let low_kernel = direct_kernel(0x8_0000, 0x10_0000);
        assert!(
            validate_image_layout(&low_kernel, None, 0x1_0000)
                .unwrap_err()
                .to_string()
                .contains("ranges overlap")
        );
        assert!(
            validate_image_layout(
                &valid_kernel,
                Some((GuestPhysAddr::from(FDT_BASE as usize), 0x1000)),
                0x1_0000,
            )
            .unwrap_err()
            .to_string()
            .contains("ranges overlap")
        );
        assert!(
            validate_image_layout(&valid_kernel, None, 0x20_0000)
                .unwrap_err()
                .to_string()
                .contains("ranges overlap")
        );
    }
}
