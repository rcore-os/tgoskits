//! Built-in x86 Linux boot stub for the direct-boot path.
//!
//! Axvisor starts x86 guests in 16-bit real mode with a flat zero-based segment
//! state. This stub switches to 32-bit protected mode and enters the Linux
//! 32-bit boot protocol with `esi = boot_params`.

use super::linux::{BOOT_STUB_GPA, BOOT_STUB_SIZE, X86LinuxLoadLayout};

/// Default GPA where the Linux direct-boot stub is loaded.
pub const DEFAULT_LINUX_BOOT_LOAD_GPA: usize = BOOT_STUB_GPA;

const BOOT_PARAMS_IMM_OFFSET: usize = 0x3b;
const KERNEL_ENTRY_IMM_OFFSET: usize = 0x40;

/// Raw Linux direct-boot stub template.
///
/// The template assumes it is loaded at [`DEFAULT_LINUX_BOOT_LOAD_GPA`]. The two
/// immediate operands patched by [`build_boot_image`] are:
///
/// - `mov esi, imm32`: boot_params GPA
/// - `mov ecx, imm32`: Linux protected-mode entry GPA
const LINUX_BOOT_TEMPLATE: &[u8] = &[
    // 16-bit real mode entry.
    0xfa, // cli
    0xfc, // cld
    0x31, 0xc0, // xor ax, ax
    0x8e, 0xd8, // mov ds, ax
    0x8e, 0xc0, // mov es, ax
    0x8e, 0xd0, // mov ss, ax
    0xbc, 0x00, 0x70, // mov sp, 0x7000
    0x0f, 0x01, 0x16, 0x49, 0x80, // lgdt [0x8049]
    0x0f, 0x20, 0xc0, // mov eax, cr0
    0x66, 0x83, 0xc8, 0x01, // or eax, 1
    0x0f, 0x22, 0xc0, // mov cr0, eax
    0xea, 0x21, 0x80, 0x10, 0x00, // ljmp 0x10:0x8021
    // 32-bit protected mode entry. CS=0x10, flat 4G code segment.
    0x66, 0xb8, 0x18, 0x00, // mov ax, 0x18
    0x8e, 0xd8, // mov ds, ax
    0x8e, 0xc0, // mov es, ax
    0x8e, 0xd0, // mov ss, ax
    0x8e, 0xe0, // mov fs, ax
    0x8e, 0xe8, // mov gs, ax
    0xbc, 0x00, 0x70, 0x00, 0x00, // mov esp, 0x7000
    0x31, 0xed, // xor ebp, ebp
    0x31, 0xff, // xor edi, edi
    0x31, 0xdb, // xor ebx, ebx
    0xbe, 0x00, 0x70, 0x00, 0x00, // mov esi, boot_params
    0xb9, 0x00, 0x00, 0x20, 0x00, // mov ecx, kernel_entry
    0xff, 0xe1, // jmp ecx
    0xf4, 0xeb, 0xfd, // hlt; jmp -3
    // GDT descriptor at 0x8049.
    0x1f, 0x00, 0x4f, 0x80, 0x00, 0x00, // limit=31, base=0x804f
    // GDT at 0x804f. Selectors: code=0x10, data=0x18.
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // null
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // reserved
    0xff, 0xff, 0x00, 0x00, 0x00, 0x9a, 0xcf, 0x00, // 32-bit code
    0xff, 0xff, 0x00, 0x00, 0x00, 0x92, 0xcf, 0x00, // 32-bit data
];

/// Builds the Linux direct-boot stub page for the provided layout.
pub fn build_boot_image(
    layout: &X86LinuxLoadLayout,
) -> Result<[u8; BOOT_STUB_SIZE], LinuxBootError> {
    if layout.boot_stub.start != DEFAULT_LINUX_BOOT_LOAD_GPA {
        return Err(LinuxBootError::UnexpectedLoadGpa {
            expected: DEFAULT_LINUX_BOOT_LOAD_GPA,
            actual: layout.boot_stub.start,
        });
    }

    let mut image = [0u8; BOOT_STUB_SIZE];
    image[..LINUX_BOOT_TEMPLATE.len()].copy_from_slice(LINUX_BOOT_TEMPLATE);
    write_u32(
        &mut image,
        BOOT_PARAMS_IMM_OFFSET,
        checked_u32(layout.boot_params.start)?,
    );
    write_u32(
        &mut image,
        KERNEL_ENTRY_IMM_OFFSET,
        checked_u32(layout.kernel.start)?,
    );
    Ok(image)
}

/// Error returned while building the x86 Linux boot stub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxBootError {
    UnexpectedLoadGpa { expected: usize, actual: usize },
    AddressAbove4G { address: usize },
}

fn checked_u32(address: usize) -> Result<u32, LinuxBootError> {
    if address > u32::MAX as usize {
        return Err(LinuxBootError::AddressAbove4G { address });
    }
    Ok(address as u32)
}

fn write_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::x86::linux::{BOOT_PARAMS_GPA, X86LinuxHeader, X86LinuxRange};

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

    fn write_header_u16(image: &mut [u8], offset: usize, value: u16) {
        image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_header_u32(image: &mut [u8], offset: usize, value: u32) {
        image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn read_u32(image: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(image[offset..offset + 4].try_into().unwrap())
    }

    fn valid_header() -> X86LinuxHeader {
        let mut image = alloc::vec![0u8; CMDLINE_SIZE_OFFSET + 4];
        image[SETUP_SECTS_OFFSET] = 5;
        write_header_u16(&mut image, BOOT_FLAG_OFFSET, 0xaa55);
        write_header_u32(&mut image, HEADER_OFFSET, u32::from_le_bytes(*b"HdrS"));
        write_header_u16(&mut image, VERSION_OFFSET, 0x020f);
        image[LOADFLAGS_OFFSET] = 0x01;
        write_header_u32(&mut image, CODE32_START_OFFSET, 0x100000);
        write_header_u16(&mut image, HEAP_END_PTR_OFFSET, 0xe000);
        write_header_u32(&mut image, INITRD_ADDR_MAX_OFFSET, 0x7fff_ffff);
        write_header_u32(&mut image, KERNEL_ALIGNMENT_OFFSET, 0x20_0000);
        image[RELOCATABLE_KERNEL_OFFSET] = 1;
        write_header_u32(&mut image, CMDLINE_SIZE_OFFSET, 4096);
        X86LinuxHeader::parse(&image).unwrap()
    }

    #[test]
    fn builds_linux_boot_stub_with_patched_entry_registers() {
        let header = valid_header();
        let layout = X86LinuxLoadLayout::new(&header, 0x30_0000, 0x1000, None).unwrap();
        let image = build_boot_image(&layout).unwrap();

        assert_eq!(image[0], 0xfa);
        assert_eq!(
            read_u32(&image, BOOT_PARAMS_IMM_OFFSET),
            BOOT_PARAMS_GPA as u32
        );
        assert_eq!(read_u32(&image, KERNEL_ENTRY_IMM_OFFSET), 0x30_0000);
    }

    #[test]
    fn rejects_stub_load_gpa_mismatch() {
        let header = valid_header();
        let mut layout = X86LinuxLoadLayout::new(&header, 0x20_0000, 0x1000, None).unwrap();
        layout.boot_stub = X86LinuxRange::new(0x9000, BOOT_STUB_SIZE);

        assert_eq!(
            build_boot_image(&layout),
            Err(LinuxBootError::UnexpectedLoadGpa {
                expected: BOOT_STUB_GPA,
                actual: 0x9000,
            })
        );
    }
}
