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

/// Architecture variants detected from the image header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageArch {
    Riscv {
        is_be: bool,
    },
    Arm64 {
        is_be: bool,
        page_size: PageSize,
        phys_placement_48bit: bool,
    },
}

#[allow(unused)]
#[derive(Debug, Clone)]
pub struct Header {
    pub text_offset: u64,
    pub image_size: u64,
    pub arch: ImageArch,
}

#[allow(unused)]
impl Header {
    pub fn parse(image: &[u8]) -> Option<Self> {
        if let Some(hdr) = ARM64Header::parse(image) {
            return Some(Self {
                text_offset: hdr.text_offset,
                image_size: hdr.image_size,
                arch: ImageArch::Arm64 {
                    is_be: hdr.kernel_is_be(),
                    page_size: hdr.page_size(),
                    phys_placement_48bit: hdr.phys_placement_48bit(),
                },
            });
        }

        if let Some(hdr) = RiscvHeader::parse(image) {
            return Some(Self {
                text_offset: hdr.text_offset,
                image_size: hdr.image_size,
                arch: ImageArch::Riscv {
                    is_be: hdr.kernel_is_be(),
                },
            });
        }

        None
    }

    pub fn hdr_size() -> usize {
        size_of::<ARM64Header>()
    }
}

#[allow(unused)]
#[repr(C)]
struct ARM64Header {
    code0: u32,
    code1: u32,
    text_offset: u64,
    image_size: u64,
    flags: u64,
    res2: u64,
    res3: u64,
    res4: u64,
    magic: u32,
    res5: u32,
}

impl ARM64Header {
    const MAGIC: u32 = 0x644d5241; // 'ARMd' in little-endian

    fn parse(buffer: &[u8]) -> Option<Self> {
        if buffer.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let hdr: Self = unsafe { core::ptr::read_unaligned(buffer.as_ptr() as *const _) };
        if hdr.magic != Self::MAGIC {
            return None;
        }
        Some(hdr)
    }

    /// Return whether the kernel image is big-endian according to flags bit 0.
    fn kernel_is_be(&self) -> bool {
        (self.flags & 0x1) != 0
    }

    /// Return page size encoded in flags bits 1-2.
    fn page_size(&self) -> PageSize {
        match (self.flags >> 1) & 0x3 {
            0 => PageSize::Unspecified,
            1 => PageSize::Size4K,
            2 => PageSize::Size16K,
            3 => PageSize::Size64K,
            _ => PageSize::Unspecified,
        }
    }

    /// Return physical placement mode (bit 3): false=default, true=48-bit constrained.
    fn phys_placement_48bit(&self) -> bool {
        ((self.flags >> 3) & 0x1) != 0
    }
}

/// Page size encoded in the image header flags (bits 1-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSize {
    Unspecified,
    Size4K,
    Size16K,
    Size64K,
}

#[allow(unused)]
struct RiscvHeader {
    code0: u32,
    code1: u32,
    text_offset: u64,
    image_size: u64,
    flags: u64,
    version: u32,
    res1: u32,
    res2: u64,
    magic: u64,
    magic2: u32,
    res4: u32,
}

impl RiscvHeader {
    const MAGIC: u64 = 0x5643534952; // 'RISCV\0\0\0' in little-endian
    const MAGIC2: u32 = 0x56534905; // secondary magic

    fn parse(buffer: &[u8]) -> Option<Self> {
        if buffer.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let hdr: Self = unsafe { core::ptr::read_unaligned(buffer.as_ptr() as *const _) };
        if hdr.magic != Self::MAGIC || hdr.magic2 != Self::MAGIC2 {
            return None;
        }
        Some(hdr)
    }

    fn kernel_is_be(&self) -> bool {
        (self.flags & 0x1) != 0
    }
}
