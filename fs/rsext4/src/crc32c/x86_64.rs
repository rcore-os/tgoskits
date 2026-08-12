//! x86_64 CRC32C acceleration without SIMD register state.

use core::{
    arch::{asm, x86_64::__cpuid},
    sync::atomic::{AtomicBool, Ordering},
};

static CRC32_HW_CHECKED: AtomicBool = AtomicBool::new(false);
static CRC32_HW_SUPPORTED: AtomicBool = AtomicBool::new(false);

/// Returns whether x86_64 CRC32C instructions are available.
///
/// Acquire/Release publishes the cached feature result. Concurrent first
/// callers may repeat CPUID, but they publish the same immutable CPU feature.
#[inline]
pub(crate) fn is_hardware_crc32_supported() -> bool {
    if CRC32_HW_CHECKED.load(Ordering::Acquire) {
        return CRC32_HW_SUPPORTED.load(Ordering::Relaxed);
    }

    let supported = has_hardware_crc32();
    CRC32_HW_SUPPORTED.store(supported, Ordering::Relaxed);
    CRC32_HW_CHECKED.store(true, Ordering::Release);
    supported
}

#[inline]
fn has_hardware_crc32() -> bool {
    // CPUID is part of the x86_64 architectural baseline. Leaf 1 only reads
    // feature registers and has no memory or OS-runtime precondition.
    let feature = __cpuid(1);
    feature.ecx & (1 << 20) != 0
}

/// Updates a raw CRC32C accumulator with x86 `crc32` instructions.
///
/// The instructions below use only general-purpose registers, so this helper
/// does not impose SIMD/FPU save-state requirements on an OS adapter.
///
/// # Safety
///
/// The current CPU must advertise SSE4.2 through CPUID leaf 1 ECX bit 20.
pub(crate) unsafe fn crc32c_hardware(mut crc: u32, data: &[u8]) -> u32 {
    let (chunks, remainder) = data.as_chunks::<8>();
    for chunk in chunks {
        let value = u64::from_le_bytes(*chunk);
        let mut accumulator = u64::from(crc);
        unsafe {
            // SAFETY: the caller established SSE4.2 support. Both operands are
            // plain GPR values, and the instruction has no memory side effect.
            asm!(
                "crc32 {crc:r}, {value:r}",
                crc = inout(reg) accumulator,
                value = in(reg) value,
                options(nomem, nostack, preserves_flags),
            );
        }
        crc = accumulator as u32;
    }

    for &byte in remainder {
        unsafe {
            // SAFETY: identical feature precondition to the 64-bit loop; the
            // byte operand and CRC accumulator remain general-purpose values.
            asm!(
                "crc32 {crc:e}, {byte}",
                crc = inout(reg) crc,
                byte = in(reg_byte) byte,
                options(nomem, nostack, preserves_flags),
            );
        }
    }
    crc
}
