#[cfg(target_arch = "aarch64")]
#[allow(dead_code)]
use core::arch::asm;

#[cfg(target_arch = "aarch64")]
lazy_static::lazy_static! {
    #[allow(dead_code)]
    pub static ref HARDWARE_SUPPORT_CRC32: bool = has_hardware_crc32();
}

// In `core::arch::aarch64`, the intrinsics with the `c` suffix implement the
// Castagnoli polynomial used by CRC32C.

#[cfg(target_arch = "aarch64")]
#[allow(dead_code)]
use core::arch::aarch64::{__crc32cb, __crc32cd, __crc32ch, __crc32cw};
/// Returns whether the current CPU advertises CRC32/CRC32C instructions.
///
/// This reads `ID_AA64ISAR0_EL1[19:16]`.
#[cfg(target_arch = "aarch64")]
#[inline]
#[allow(dead_code)]
pub fn has_hardware_crc32() -> bool {
    use log::warn;

    let mut reg_val: u64;
    unsafe {
        // mrs: Move from System Register to general purpose register
        asm!("mrs {}, ID_AA64ISAR0_EL1", out(reg) reg_val);
    }

    // Bits [19:16] encode CRC32 feature support.
    let crc_field = (reg_val >> 16) & 0xF;
    warn!("ID_AA64ISAR0_EL1[19:16]: {crc_field:#x}");
    // `>= 1` means CRC32 and CRC32C instructions are present.
    if crc_field >= 1 {
        warn!("Hardware CRC32C support detected.");
        true
    } else {
        warn!("No hardware CRC32C support.");
        false
    }
}

/// Computes CRC32C with ARMv8 hardware instructions.
///
/// The implementation handles 64/32/16/8-bit chunks to maximize throughput.
///
/// # Safety
///
/// This function uses the `crc` target feature and must only run on CPUs that
/// support the CRC instruction set.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "crc")] // Tell the compiler the CRC feature is available here.
#[inline]
#[allow(dead_code)]
pub unsafe fn crc32c_hardware(mut crc: u32, data: &[u8]) -> u32 {
    // This helper only performs the raw CRC update step. Callers remain
    // responsible for any ext4-specific init/finalize inversion.
    unsafe {
        let mut p = data.as_ptr();
        let mut len = data.len();

        // 1. Consume the unaligned prefix so the hot loop can use 64-bit loads.
        while len > 0 && (p as usize) % 8 != 0 {
            crc = __crc32cb(crc, *p);
            p = p.add(1);
            len -= 1;
        }

        // 2. Main loop: process aligned 64-bit chunks.
        while len >= 8 {
            let val = *(p as *const u64);
            crc = __crc32cd(crc, val);
            p = p.add(8);
            len -= 8;
        }

        // 3. Finish the tail with progressively smaller chunk sizes.
        if len >= 4 {
            let val = *(p as *const u32);
            crc = __crc32cw(crc, val);
            p = p.add(4);
            len -= 4;
        }

        if len >= 2 {
            let val = *(p as *const u16);
            crc = __crc32ch(crc, val);
            p = p.add(2);
            len -= 2;
        }

        if len > 0 {
            crc = __crc32cb(crc, *p);
        }

        crc
    }
}
