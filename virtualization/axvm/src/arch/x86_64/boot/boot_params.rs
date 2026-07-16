// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Linux x86 `boot_params` / zero page construction.

use alloc::vec::Vec;

use super::linux::{
    BOOT_PARAMS_SIZE, X86LinuxHeader, X86LinuxLayoutError, X86LinuxLoadLayout, X86LinuxRange,
};

const SETUP_HEADER_START: usize = 0x1f1;
const SETUP_HEADER_END: usize = 0x290;

const EXT_RAMDISK_IMAGE_OFFSET: usize = 0x0c0;
const EXT_RAMDISK_SIZE_OFFSET: usize = 0x0c4;
const EXT_CMD_LINE_PTR_OFFSET: usize = 0x0c8;
const ACPI_RSDP_ADDR_OFFSET: usize = 0x070;
const E820_ENTRIES_OFFSET: usize = 0x1e8;
const SENTINEL_OFFSET: usize = 0x1ef;
const TYPE_OF_LOADER_OFFSET: usize = 0x210;
const LOADFLAGS_OFFSET: usize = 0x211;
const CODE32_START_OFFSET: usize = 0x214;
const RAMDISK_IMAGE_OFFSET: usize = 0x218;
const RAMDISK_SIZE_OFFSET: usize = 0x21c;
const HEAP_END_PTR_OFFSET: usize = 0x224;
const CMD_LINE_PTR_OFFSET: usize = 0x228;
const SETUP_DATA_OFFSET: usize = 0x250;
const E820_TABLE_OFFSET: usize = 0x2d0;

const COMMAND_LINE_OFFSET: usize = 0xe00;

const TYPE_OF_LOADER_UNSPECIFIED: u8 = 0xff;
const LOADFLAG_CAN_USE_HEAP: u8 = 0x80;

const E820_ENTRY_SIZE: usize = 20;
const E820_MAX_ENTRIES: usize = 128;
const E820_TYPE_RAM: u32 = 1;
const E820_TYPE_RESERVED: u32 = 2;

const LEGACY_RESERVED_START: usize = 0x000a_0000;
const LEGACY_RESERVED_SIZE: usize = 0x0006_0000;

/// Builds a Linux x86 boot_params page for the direct-boot path.
pub struct BootParamsBuilder<'a> {
    kernel_image: &'a [u8],
    header: X86LinuxHeader,
    layout: X86LinuxLoadLayout,
    ram_ranges: Vec<X86LinuxRange>,
    reserved_ranges: Vec<X86LinuxRange>,
    command_line: Option<&'a str>,
    acpi_rsdp_address: Option<u64>,
}

impl<'a> BootParamsBuilder<'a> {
    pub fn new(
        kernel_image: &'a [u8],
        header: X86LinuxHeader,
        layout: X86LinuxLoadLayout,
        main_memory: X86LinuxRange,
    ) -> Self {
        Self {
            kernel_image,
            header,
            layout,
            ram_ranges: alloc::vec![main_memory],
            reserved_ranges: alloc::vec![
                layout.boot_params,
                layout.boot_stub,
                X86LinuxRange::new(LEGACY_RESERVED_START, LEGACY_RESERVED_SIZE),
            ],
            command_line: None,
            acpi_rsdp_address: None,
        }
    }

    pub fn add_ram_range(&mut self, range: X86LinuxRange) {
        if range.size != 0 {
            self.ram_ranges.push(range);
        }
    }

    pub fn set_command_line(&mut self, command_line: &'a str) -> Result<(), BootParamsError> {
        self.validate_command_line(command_line)?;
        self.command_line = Some(command_line);
        Ok(())
    }

    pub fn add_reserved_range(&mut self, range: X86LinuxRange) {
        if range.size != 0 {
            self.reserved_ranges.push(range);
        }
    }

    /// Publishes the generated ACPI RSDP through Linux `boot_params`.
    pub fn set_acpi_rsdp_address(&mut self, address: u64) {
        self.acpi_rsdp_address = Some(address);
    }

    pub fn build(mut self) -> Result<[u8; BOOT_PARAMS_SIZE], BootParamsError> {
        let mut boot_params = [0u8; BOOT_PARAMS_SIZE];
        self.copy_setup_header(&mut boot_params)?;
        self.patch_setup_header(&mut boot_params)?;
        self.write_e820(&mut boot_params)?;
        Ok(boot_params)
    }

    fn copy_setup_header(&self, boot_params: &mut [u8]) -> Result<(), BootParamsError> {
        let source = self
            .kernel_image
            .get(SETUP_HEADER_START..SETUP_HEADER_END)
            .ok_or(BootParamsError::SetupHeaderTruncated {
                image_size: self.kernel_image.len(),
                required: SETUP_HEADER_END,
            })?;
        boot_params[SETUP_HEADER_START..SETUP_HEADER_END].copy_from_slice(source);
        Ok(())
    }

    fn patch_setup_header(&self, boot_params: &mut [u8]) -> Result<(), BootParamsError> {
        write_u8(boot_params, SENTINEL_OFFSET, 0xff);
        write_u8(
            boot_params,
            TYPE_OF_LOADER_OFFSET,
            TYPE_OF_LOADER_UNSPECIFIED,
        );
        write_u8(
            boot_params,
            LOADFLAGS_OFFSET,
            self.header.loadflags | LOADFLAG_CAN_USE_HEAP,
        );
        write_u16(boot_params, HEAP_END_PTR_OFFSET, self.header.heap_end_ptr);
        write_u32(
            boot_params,
            CODE32_START_OFFSET,
            self.layout.kernel.start as u32,
        );
        write_u64(boot_params, SETUP_DATA_OFFSET, 0);
        if let Some(address) = self.acpi_rsdp_address {
            write_u64(boot_params, ACPI_RSDP_ADDR_OFFSET, address);
        }

        let cmdline_ptr = self
            .layout
            .boot_params
            .start
            .checked_add(COMMAND_LINE_OFFSET)
            .ok_or(BootParamsError::AddressOverflow)?;
        write_u32(boot_params, CMD_LINE_PTR_OFFSET, cmdline_ptr as u32);
        write_u32(
            boot_params,
            EXT_CMD_LINE_PTR_OFFSET,
            (cmdline_ptr >> 32) as u32,
        );
        self.write_command_line(boot_params)?;

        if let Some(initrd) = self.layout.initrd {
            write_u32(boot_params, RAMDISK_IMAGE_OFFSET, initrd.start as u32);
            write_u32(boot_params, RAMDISK_SIZE_OFFSET, initrd.size as u32);
            write_u32(
                boot_params,
                EXT_RAMDISK_IMAGE_OFFSET,
                (initrd.start >> 32) as u32,
            );
            write_u32(
                boot_params,
                EXT_RAMDISK_SIZE_OFFSET,
                (initrd.size >> 32) as u32,
            );
        }

        Ok(())
    }

    fn write_command_line(&self, boot_params: &mut [u8]) -> Result<(), BootParamsError> {
        let command_line = self
            .command_line
            .ok_or(BootParamsError::CommandLineMissing)?;
        self.validate_command_line(command_line)?;
        let bytes = command_line.as_bytes();
        let end = COMMAND_LINE_OFFSET + bytes.len();
        boot_params[COMMAND_LINE_OFFSET..end].copy_from_slice(bytes);
        write_u8(boot_params, end, 0);
        Ok(())
    }

    fn validate_command_line(&self, command_line: &str) -> Result<(), BootParamsError> {
        if command_line.as_bytes().contains(&0) {
            return Err(BootParamsError::CommandLineContainsNul);
        }

        let max_len = self.command_line_capacity();
        if command_line.len() > max_len {
            return Err(BootParamsError::CommandLineTooLong {
                len: command_line.len(),
                max: max_len,
            });
        }
        Ok(())
    }

    fn command_line_capacity(&self) -> usize {
        let zero_page_capacity = BOOT_PARAMS_SIZE - COMMAND_LINE_OFFSET - 1;
        if self.header.cmdline_size == 0 {
            zero_page_capacity
        } else {
            zero_page_capacity.min(self.header.cmdline_size as usize)
        }
    }

    fn write_e820(&mut self, boot_params: &mut [u8]) -> Result<(), BootParamsError> {
        let entries = self.e820_entries()?;
        if entries.len() > E820_MAX_ENTRIES {
            return Err(BootParamsError::TooManyE820Entries {
                entries: entries.len(),
            });
        }

        write_u8(boot_params, E820_ENTRIES_OFFSET, entries.len() as u8);
        for (idx, entry) in entries.iter().enumerate() {
            let offset = E820_TABLE_OFFSET + idx * E820_ENTRY_SIZE;
            write_u64(boot_params, offset, entry.addr);
            write_u64(boot_params, offset + 8, entry.size);
            write_u32(boot_params, offset + 16, entry.entry_type);
        }

        Ok(())
    }

    fn e820_entries(&mut self) -> Result<Vec<E820Entry>, BootParamsError> {
        let mut entries = Vec::new();
        let ram_ranges = normalized_ranges(&self.ram_ranges)?;
        let reserved = normalized_ranges(&self.reserved_ranges)?;

        for ram in ram_ranges.iter().copied() {
            let ram_end = ram.end().map_err(BootParamsError::Layout)?;
            let mut cursor = ram.start;

            for range in reserved.iter().copied() {
                let range_end = range.end().map_err(BootParamsError::Layout)?;
                if range_end <= ram.start || range.start >= ram_end {
                    continue;
                }

                let reserved_start = range.start.max(ram.start);
                let reserved_end = range_end.min(ram_end);
                if cursor < reserved_start {
                    entries.push(E820Entry::ram(cursor, reserved_start - cursor)?);
                }
                entries.push(E820Entry::reserved(X86LinuxRange::new(
                    reserved_start,
                    reserved_end - reserved_start,
                ))?);
                cursor = cursor.max(reserved_end);
            }

            if cursor < ram_end {
                entries.push(E820Entry::ram(cursor, ram_end - cursor)?);
            }
        }

        for range in reserved {
            let overlaps_ram = ram_ranges
                .iter()
                .try_fold(false, |found, ram| Ok(found || range.overlaps(ram)?))
                .map_err(BootParamsError::Layout)?;
            if !overlaps_ram {
                entries.push(E820Entry::reserved(range)?);
            }
        }

        entries.sort_by_key(|entry| entry.addr);
        Ok(entries)
    }
}

/// Error returned while building Linux x86 boot_params.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootParamsError {
    SetupHeaderTruncated { image_size: usize, required: usize },
    CommandLineMissing,
    CommandLineContainsNul,
    CommandLineTooLong { len: usize, max: usize },
    AddressOverflow,
    Layout(X86LinuxLayoutError),
    TooManyE820Entries { entries: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct E820Entry {
    addr: u64,
    size: u64,
    entry_type: u32,
}

impl E820Entry {
    fn ram(start: usize, size: usize) -> Result<Self, BootParamsError> {
        Self::new(start, size, E820_TYPE_RAM)
    }

    fn reserved(range: X86LinuxRange) -> Result<Self, BootParamsError> {
        Self::new(range.start, range.size, E820_TYPE_RESERVED)
    }

    fn new(start: usize, size: usize, entry_type: u32) -> Result<Self, BootParamsError> {
        Ok(Self {
            addr: start as u64,
            size: size as u64,
            entry_type,
        })
    }
}

fn normalized_ranges(ranges: &[X86LinuxRange]) -> Result<Vec<X86LinuxRange>, BootParamsError> {
    let mut ranges = ranges
        .iter()
        .copied()
        .filter(|range| range.size != 0)
        .collect::<Vec<_>>();
    ranges.sort_by_key(|range| range.start);

    let mut normalized = Vec::<X86LinuxRange>::new();
    for range in ranges {
        range.end().map_err(BootParamsError::Layout)?;
        if let Some(last) = normalized.last_mut() {
            let last_end = last.end().map_err(BootParamsError::Layout)?;
            let range_end = range.end().map_err(BootParamsError::Layout)?;
            if range.start <= last_end {
                last.size = range_end.max(last_end) - last.start;
                continue;
            }
        }
        normalized.push(range);
    }

    Ok(normalized)
}

fn write_u8(buffer: &mut [u8], offset: usize, value: u8) {
    buffer[offset] = value;
}

fn write_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(buffer: &mut [u8], offset: usize, value: u64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::{
        super::linux::{BOOT_PARAMS_GPA, BOOT_STUB_GPA, BOOT_STUB_SIZE},
        *,
    };

    const SETUP_SECTS_OFFSET: usize = 0x1f1;
    const BOOT_FLAG_OFFSET: usize = 0x1fe;
    const HEADER_OFFSET: usize = 0x202;
    const VERSION_OFFSET: usize = 0x206;
    const LOADFLAGS_OFFSET: usize = 0x211;
    const CODE32_START_OFFSET: usize = 0x214;
    const INITRD_ADDR_MAX_OFFSET: usize = 0x22c;
    const KERNEL_ALIGNMENT_OFFSET: usize = 0x230;
    const RELOCATABLE_KERNEL_OFFSET: usize = 0x234;
    const CMDLINE_SIZE_OFFSET: usize = 0x238;

    fn read_u8(buffer: &[u8], offset: usize) -> u8 {
        buffer[offset]
    }

    fn read_u32(buffer: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap())
    }

    fn read_e820_entry(buffer: &[u8], idx: usize) -> E820Entry {
        let offset = E820_TABLE_OFFSET + idx * E820_ENTRY_SIZE;
        E820Entry {
            addr: u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap()),
            size: u64::from_le_bytes(buffer[offset + 8..offset + 16].try_into().unwrap()),
            entry_type: u32::from_le_bytes(buffer[offset + 16..offset + 20].try_into().unwrap()),
        }
    }

    fn write_header_u16(image: &mut [u8], offset: usize, value: u16) {
        image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_header_u32(image: &mut [u8], offset: usize, value: u32) {
        image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn valid_image() -> Vec<u8> {
        let mut image = alloc::vec![0u8; SETUP_HEADER_END + 0x1000];
        image[SETUP_SECTS_OFFSET] = 5;
        write_header_u16(&mut image, BOOT_FLAG_OFFSET, 0xaa55);
        write_header_u32(&mut image, HEADER_OFFSET, u32::from_le_bytes(*b"HdrS"));
        write_header_u16(&mut image, VERSION_OFFSET, 0x020f);
        image[LOADFLAGS_OFFSET] = 0x01;
        write_header_u32(&mut image, CODE32_START_OFFSET, 0x100000);
        write_header_u32(&mut image, INITRD_ADDR_MAX_OFFSET, 0x7fff_ffff);
        write_header_u32(&mut image, KERNEL_ALIGNMENT_OFFSET, 0x20_0000);
        image[RELOCATABLE_KERNEL_OFFSET] = 1;
        write_header_u32(&mut image, CMDLINE_SIZE_OFFSET, 4096);
        image
    }

    fn valid_layout(header: &X86LinuxHeader) -> X86LinuxLoadLayout {
        X86LinuxLoadLayout::new(
            header,
            0x20_0000,
            0x10_0000,
            Some(X86LinuxRange::new(0x40_0000, 0x20_0000)),
        )
        .unwrap()
    }

    #[test]
    fn builds_boot_params_with_patched_header_and_initrd() {
        let image = valid_image();
        let header = X86LinuxHeader::parse(&image).unwrap();
        let layout = valid_layout(&header);
        let mut builder =
            BootParamsBuilder::new(&image, header, layout, X86LinuxRange::new(0, 0x80_0000));
        builder
            .set_command_line("console=ttyS0 rdinit=/init")
            .unwrap();
        builder.set_acpi_rsdp_address(0x000e_0000);
        let params = builder.build().unwrap();

        assert_eq!(read_u8(&params, SENTINEL_OFFSET), 0xff);
        assert_eq!(
            read_u8(&params, TYPE_OF_LOADER_OFFSET),
            TYPE_OF_LOADER_UNSPECIFIED
        );
        assert_eq!(
            read_u8(&params, LOADFLAGS_OFFSET),
            0x01 | LOADFLAG_CAN_USE_HEAP
        );
        assert_eq!(read_u32(&params, CODE32_START_OFFSET), 0x20_0000);
        assert_eq!(read_u32(&params, RAMDISK_IMAGE_OFFSET), 0x40_0000);
        assert_eq!(read_u32(&params, RAMDISK_SIZE_OFFSET), 0x20_0000);
        assert_eq!(
            read_u32(&params, CMD_LINE_PTR_OFFSET),
            (BOOT_PARAMS_GPA + COMMAND_LINE_OFFSET) as u32
        );
        assert_eq!(
            &params[COMMAND_LINE_OFFSET..COMMAND_LINE_OFFSET + 26],
            b"console=ttyS0 rdinit=/init"
        );
        assert_eq!(read_u8(&params, COMMAND_LINE_OFFSET + 26), 0);
        assert_eq!(
            u64::from_le_bytes(
                params[ACPI_RSDP_ADDR_OFFSET..ACPI_RSDP_ADDR_OFFSET + 8]
                    .try_into()
                    .unwrap()
            ),
            0x000e_0000
        );
    }

    #[test]
    fn rejects_missing_command_line() {
        let image = valid_image();
        let header = X86LinuxHeader::parse(&image).unwrap();
        let layout = valid_layout(&header);

        assert_eq!(
            BootParamsBuilder::new(&image, header, layout, X86LinuxRange::new(0, 0x80_0000))
                .build(),
            Err(BootParamsError::CommandLineMissing)
        );
    }

    #[test]
    fn rejects_command_line_that_does_not_fit_zero_page() {
        let image = valid_image();
        let header = X86LinuxHeader::parse(&image).unwrap();
        let layout = valid_layout(&header);
        let mut builder =
            BootParamsBuilder::new(&image, header, layout, X86LinuxRange::new(0, 0x20_0000));
        let long_command_line = alloc::string::String::from_utf8(alloc::vec![
            b'a';
            BOOT_PARAMS_SIZE - COMMAND_LINE_OFFSET
        ])
        .unwrap();

        assert_eq!(
            builder.set_command_line(&long_command_line),
            Err(BootParamsError::CommandLineTooLong {
                len: BOOT_PARAMS_SIZE - COMMAND_LINE_OFFSET,
                max: BOOT_PARAMS_SIZE - COMMAND_LINE_OFFSET - 1,
            })
        );
    }

    #[test]
    fn rejects_command_line_with_nul() {
        let image = valid_image();
        let header = X86LinuxHeader::parse(&image).unwrap();
        let layout = valid_layout(&header);
        let mut builder =
            BootParamsBuilder::new(&image, header, layout, X86LinuxRange::new(0, 0x20_0000));

        assert_eq!(
            builder.set_command_line("console=ttyS0\0rdinit=/init"),
            Err(BootParamsError::CommandLineContainsNul)
        );
    }

    #[test]
    fn builds_e820_with_ram_and_reserved_low_ranges() {
        let image = valid_image();
        let header = X86LinuxHeader::parse(&image).unwrap();
        let layout = valid_layout(&header);
        let mut builder =
            BootParamsBuilder::new(&image, header, layout, X86LinuxRange::new(0, 0x20_0000));
        builder.set_command_line("console=ttyS0").unwrap();
        let params = builder.build().unwrap();

        let entries = read_u8(&params, E820_ENTRIES_OFFSET) as usize;
        assert!(entries >= 5);
        assert_eq!(
            read_e820_entry(&params, 0),
            E820Entry::new(0, BOOT_PARAMS_GPA, 1).unwrap()
        );
        assert_eq!(
            read_e820_entry(&params, 1),
            E820Entry::reserved(X86LinuxRange::new(
                BOOT_PARAMS_GPA,
                BOOT_STUB_GPA + BOOT_STUB_SIZE - BOOT_PARAMS_GPA
            ))
            .unwrap()
        );
    }

    #[test]
    fn builds_e820_with_multiple_ram_ranges() {
        let image = valid_image();
        let header = X86LinuxHeader::parse(&image).unwrap();
        let layout = valid_layout(&header);
        let mut builder = BootParamsBuilder::new(
            &image,
            header,
            layout,
            X86LinuxRange::new(0x0960_0000, 0x0800_0000),
        );
        builder.add_ram_range(X86LinuxRange::new(0, 0x10_0000));
        builder.set_command_line("console=ttyS0").unwrap();
        let params = builder.build().unwrap();

        let entries = read_u8(&params, E820_ENTRIES_OFFSET) as usize;
        let has_low_usable = (0..entries).any(|idx| {
            read_e820_entry(&params, idx) == E820Entry::new(0, BOOT_PARAMS_GPA, 1).unwrap()
        });
        let has_trampoline_usable = (0..entries).any(|idx| {
            read_e820_entry(&params, idx)
                == E820Entry::new(BOOT_STUB_GPA + BOOT_STUB_SIZE, 0xa0000 - 0x9000, 1).unwrap()
        });
        let has_high_usable = (0..entries).any(|idx| {
            read_e820_entry(&params, idx) == E820Entry::new(0x0960_0000, 0x0800_0000, 1).unwrap()
        });

        assert!(has_low_usable);
        assert!(has_trampoline_usable);
        assert!(has_high_usable);
    }

    #[test]
    fn records_reserved_passthrough_ranges_in_e820() {
        let image = valid_image();
        let header = X86LinuxHeader::parse(&image).unwrap();
        let layout = valid_layout(&header);
        let mut builder =
            BootParamsBuilder::new(&image, header, layout, X86LinuxRange::new(0, 0x20_0000));
        builder.add_reserved_range(X86LinuxRange::new(0xfec0_0000, 0x1000));
        builder.set_command_line("console=ttyS0").unwrap();
        let params = builder.build().unwrap();

        let entries = read_u8(&params, E820_ENTRIES_OFFSET) as usize;
        let found = (0..entries).any(|idx| {
            read_e820_entry(&params, idx)
                == E820Entry::reserved(X86LinuxRange::new(0xfec0_0000, 0x1000)).unwrap()
        });
        assert!(found);
    }

    #[test]
    fn rejects_truncated_setup_header_copy() {
        let image = valid_image();
        let header = X86LinuxHeader::parse(&image).unwrap();
        let layout = valid_layout(&header);
        let short_image = &image[..SETUP_HEADER_END - 1];

        assert_eq!(
            BootParamsBuilder::new(
                short_image,
                header,
                layout,
                X86LinuxRange::new(0, 0x20_0000)
            )
            .build(),
            Err(BootParamsError::SetupHeaderTruncated {
                image_size: SETUP_HEADER_END - 1,
                required: SETUP_HEADER_END,
            })
        );
    }
}
