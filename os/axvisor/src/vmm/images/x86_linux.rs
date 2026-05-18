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

//! Linux x86 boot protocol header parsing.
//!
//! This module only recognizes and parses bzImage setup header fields needed by
//! the staged direct-boot work. It deliberately does not lay out or load the
//! protected-mode payload; that starts in phase 2.

const SETUP_SECTS_DEFAULT: usize = 4;

const SETUP_SECTS_OFFSET: usize = 0x1f1;
const BOOT_FLAG_OFFSET: usize = 0x1fe;
const HEADER_OFFSET: usize = 0x202;
const VERSION_OFFSET: usize = 0x206;
const LOADFLAGS_OFFSET: usize = 0x211;
const CODE32_START_OFFSET: usize = 0x214;
const HEAP_END_PTR_OFFSET: usize = 0x224;
const INITRD_ADDR_MAX_OFFSET: usize = 0x22c;
const KERNEL_ALIGNMENT_OFFSET: usize = 0x230;
const RELOCATABLE_KERNEL_OFFSET: usize = 0x234;
const CMDLINE_SIZE_OFFSET: usize = 0x238;

const BOOT_FLAG_MAGIC: u16 = 0xaa55;
const HEADER_MAGIC: u32 = u32::from_le_bytes(*b"HdrS");

/// Number of bytes needed to parse every field used by [`X86LinuxHeader`].
pub const HEADER_READ_SIZE: usize = CMDLINE_SIZE_OFFSET + core::mem::size_of::<u32>();

/// Parsed subset of Linux x86 setup header fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X86LinuxHeader {
    pub setup_sects: usize,
    pub boot_protocol_version: u16,
    pub code32_start: u32,
    pub cmdline_size: u32,
    pub initrd_addr_max: u32,
    pub kernel_alignment: u32,
    pub relocatable_kernel: bool,
    pub loadflags: u8,
    pub heap_end_ptr: u16,
}

impl X86LinuxHeader {
    pub fn parse(image: &[u8]) -> Result<Self, X86LinuxHeaderError> {
        let boot_flag = read_u16(image, BOOT_FLAG_OFFSET)?;
        if boot_flag != BOOT_FLAG_MAGIC {
            return Err(X86LinuxHeaderError::InvalidBootFlag { value: boot_flag });
        }

        let header = read_u32(image, HEADER_OFFSET)?;
        if header != HEADER_MAGIC {
            return Err(X86LinuxHeaderError::InvalidHeader { value: header });
        }

        let raw_setup_sects = read_u8(image, SETUP_SECTS_OFFSET)?;
        let setup_sects = match raw_setup_sects {
            0 => SETUP_SECTS_DEFAULT,
            value => value as usize,
        };

        Ok(Self {
            setup_sects,
            boot_protocol_version: read_u16(image, VERSION_OFFSET)?,
            code32_start: read_u32(image, CODE32_START_OFFSET)?,
            cmdline_size: read_u32(image, CMDLINE_SIZE_OFFSET)?,
            initrd_addr_max: read_u32(image, INITRD_ADDR_MAX_OFFSET)?,
            kernel_alignment: read_u32(image, KERNEL_ALIGNMENT_OFFSET)?,
            relocatable_kernel: read_u8(image, RELOCATABLE_KERNEL_OFFSET)? != 0,
            loadflags: read_u8(image, LOADFLAGS_OFFSET)?,
            heap_end_ptr: read_u16(image, HEAP_END_PTR_OFFSET)?,
        })
    }

    /// Offset of the protected-mode kernel payload in the bzImage.
    pub fn payload_offset(&self) -> usize {
        (self.setup_sects + 1) * 512
    }
}

/// Error returned while parsing a Linux x86 setup header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X86LinuxHeaderError {
    Truncated {
        offset: usize,
        size: usize,
        image_size: usize,
    },
    InvalidBootFlag {
        value: u16,
    },
    InvalidHeader {
        value: u32,
    },
}

fn read_u8(image: &[u8], offset: usize) -> Result<u8, X86LinuxHeaderError> {
    image
        .get(offset)
        .copied()
        .ok_or_else(|| truncated(offset, core::mem::size_of::<u8>(), image.len()))
}

fn read_u16(image: &[u8], offset: usize) -> Result<u16, X86LinuxHeaderError> {
    let bytes = read_array::<2>(image, offset)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(image: &[u8], offset: usize) -> Result<u32, X86LinuxHeaderError> {
    let bytes = read_array::<4>(image, offset)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_array<const N: usize>(image: &[u8], offset: usize) -> Result<[u8; N], X86LinuxHeaderError> {
    let end = offset
        .checked_add(N)
        .ok_or_else(|| truncated(offset, N, image.len()))?;
    let bytes = image
        .get(offset..end)
        .ok_or_else(|| truncated(offset, N, image.len()))?;
    Ok(bytes.try_into().unwrap())
}

fn truncated(offset: usize, size: usize, image_size: usize) -> X86LinuxHeaderError {
    X86LinuxHeaderError::Truncated {
        offset,
        size,
        image_size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION: u16 = 0x020f;
    const CODE32_START: u32 = 0x0010_0000;
    const CMDLINE_SIZE: u32 = 4096;
    const INITRD_ADDR_MAX: u32 = 0x7fff_ffff;
    const KERNEL_ALIGNMENT: u32 = 0x20_0000;
    const LOADFLAGS: u8 = 0x81;
    const HEAP_END_PTR: u16 = 0xe000;

    fn write_u16(image: &mut [u8], offset: usize, value: u16) {
        image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(image: &mut [u8], offset: usize, value: u32) {
        image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn valid_bzimage_header() -> [u8; HEADER_READ_SIZE] {
        let mut image = [0u8; HEADER_READ_SIZE];
        image[SETUP_SECTS_OFFSET] = 5;
        write_u16(&mut image, BOOT_FLAG_OFFSET, BOOT_FLAG_MAGIC);
        write_u32(&mut image, HEADER_OFFSET, HEADER_MAGIC);
        write_u16(&mut image, VERSION_OFFSET, VERSION);
        image[LOADFLAGS_OFFSET] = LOADFLAGS;
        write_u32(&mut image, CODE32_START_OFFSET, CODE32_START);
        write_u16(&mut image, HEAP_END_PTR_OFFSET, HEAP_END_PTR);
        write_u32(&mut image, INITRD_ADDR_MAX_OFFSET, INITRD_ADDR_MAX);
        write_u32(&mut image, KERNEL_ALIGNMENT_OFFSET, KERNEL_ALIGNMENT);
        image[RELOCATABLE_KERNEL_OFFSET] = 1;
        write_u32(&mut image, CMDLINE_SIZE_OFFSET, CMDLINE_SIZE);
        image
    }

    #[test]
    fn parses_valid_bzimage_header() {
        let header = X86LinuxHeader::parse(&valid_bzimage_header()).unwrap();

        assert_eq!(header.setup_sects, 5);
        assert_eq!(header.boot_protocol_version, VERSION);
        assert_eq!(header.code32_start, CODE32_START);
        assert_eq!(header.cmdline_size, CMDLINE_SIZE);
        assert_eq!(header.initrd_addr_max, INITRD_ADDR_MAX);
        assert_eq!(header.kernel_alignment, KERNEL_ALIGNMENT);
        assert!(header.relocatable_kernel);
        assert_eq!(header.loadflags, LOADFLAGS);
        assert_eq!(header.heap_end_ptr, HEAP_END_PTR);
        assert_eq!(header.payload_offset(), 6 * 512);
    }

    #[test]
    fn treats_zero_setup_sects_as_four() {
        let mut image = valid_bzimage_header();
        image[SETUP_SECTS_OFFSET] = 0;

        let header = X86LinuxHeader::parse(&image).unwrap();

        assert_eq!(header.setup_sects, 4);
        assert_eq!(header.payload_offset(), 5 * 512);
    }

    #[test]
    fn rejects_non_linux_image_without_header_magic() {
        let mut image = valid_bzimage_header();
        write_u32(&mut image, HEADER_OFFSET, 0);

        assert_eq!(
            X86LinuxHeader::parse(&image),
            Err(X86LinuxHeaderError::InvalidHeader { value: 0 })
        );
    }

    #[test]
    fn rejects_non_bootable_image_without_boot_flag() {
        let mut image = valid_bzimage_header();
        write_u16(&mut image, BOOT_FLAG_OFFSET, 0);

        assert_eq!(
            X86LinuxHeader::parse(&image),
            Err(X86LinuxHeaderError::InvalidBootFlag { value: 0 })
        );
    }

    #[test]
    fn reports_truncated_header_offset() {
        assert_eq!(
            X86LinuxHeader::parse(&[0u8; 16]),
            Err(X86LinuxHeaderError::Truncated {
                offset: BOOT_FLAG_OFFSET,
                size: 2,
                image_size: 16,
            })
        );
    }
}
