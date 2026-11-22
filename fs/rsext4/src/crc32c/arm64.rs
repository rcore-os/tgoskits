#[cfg(all(target_arch = "aarch64", not(target_os = "macos")))]
use core::arch::asm;
#[cfg(target_arch = "aarch64")]
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_arch = "aarch64")]
static CRC32_HW_CHECKED: AtomicBool = AtomicBool::new(false);
#[cfg(target_arch = "aarch64")]
static CRC32_HW_SUPPORTED: AtomicBool = AtomicBool::new(false);

/// Returns whether the current CPU supports hardware CRC32C instructions.
///
/// The result is computed once on first call and cached for all subsequent calls.
///
/// Safety: `CRC32_HW_CHECKED` (Acquire/Release) provides happens-before ordering;
/// `CRC32_HW_SUPPORTED` uses Relaxed because it is always accessed after
/// the Acquire load on `CRC32_HW_CHECKED` has observed the init-complete store.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn is_hardware_crc32_supported() -> bool {
    if CRC32_HW_CHECKED.load(Ordering::Acquire) {
        return CRC32_HW_SUPPORTED.load(Ordering::Relaxed);
    }
    let supported = has_hardware_crc32();
    CRC32_HW_SUPPORTED.store(supported, Ordering::Relaxed);
    CRC32_HW_CHECKED.store(true, Ordering::Release);
    supported
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
    #[cfg(target_os = "macos")]
    {
        // EL1 feature registers are unavailable to macOS userspace tests.
        false
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut reg_val: u64;
        unsafe {
            // SAFETY: `mrs` only copies the read-only ID register into a
            // general-purpose register and touches no memory. At EL1 the read
            // is architectural. At EL0 it traps, and the instruction is only
            // valid there because the kernel emulates the ID registers for
            // user space (Linux does so when it sets `HWCAP_CPUID`); a host
            // that does not emulate them delivers the trap as an illegal
            // instruction.
            // mrs: Move from System Register to general purpose register
            asm!("mrs {}, ID_AA64ISAR0_EL1", out(reg) reg_val);
        }

        // Bits [19:16] encode CRC32 feature support.
        let crc_field = (reg_val >> 16) & 0xF;

        // `>= 1` means CRC32 and CRC32C instructions are present.
        crc_field >= 1
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
    let mut p = data.as_ptr();
    let mut len = data.len();

    // Every read below is in bounds because `len` counts the bytes left
    // between `p` and the end of `data`, and each step reads at most `len`
    // bytes before advancing `p` and lowering `len` by the same amount.

    // 1. Consume the unaligned prefix so the hot loop can use 64-bit loads.
    while len > 0 && !(p as usize).is_multiple_of(8) {
        unsafe {
            // SAFETY: `len > 0`, so `p` points at a byte of `data`, and
            // advancing it by one keeps it within `data` or one past its end.
            crc = __crc32cb(crc, *p);
            p = p.add(1);
        }
        len -= 1;
    }

    // 2. Main loop: process aligned 64-bit chunks.
    while len >= 8 {
        unsafe {
            // SAFETY: the prefix loop left `p` 8-byte aligned and this loop
            // advances it in steps of 8, so the read is aligned; `len >= 8`
            // keeps all eight bytes in `data`, and `p` ends within `data` or
            // one past its end.
            let val = *(p as *const u64);
            crc = __crc32cd(crc, val);
            p = p.add(8);
        }
        len -= 8;
    }

    // 3. Finish the tail with progressively smaller chunk sizes.
    if len >= 4 {
        unsafe {
            // SAFETY: `p` is 8-byte aligned here, so also 4-byte aligned;
            // `len >= 4` keeps the four bytes in `data`, and `p` ends within
            // `data` or one past its end.
            let val = *(p as *const u32);
            crc = __crc32cw(crc, val);
            p = p.add(4);
        }
        len -= 4;
    }

    if len >= 2 {
        unsafe {
            // SAFETY: `p` advanced by 0 or 4 from an 8-byte boundary, so it
            // is 2-byte aligned; `len >= 2` keeps the two bytes in `data`, and
            // `p` ends within `data` or one past its end.
            let val = *(p as *const u16);
            crc = __crc32ch(crc, val);
            p = p.add(2);
        }
        len -= 2;
    }

    if len > 0 {
        unsafe {
            // SAFETY: `len > 0`, so `p` points at the last byte of `data`.
            crc = __crc32cb(crc, *p);
        }
    }

    crc
}
