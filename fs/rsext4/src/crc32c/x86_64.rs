//! x86_64 CRC32C acceleration without SIMD register state.

use core::arch::asm;

/// Updates a raw CRC32C accumulator with x86 `crc32` instructions.
///
/// The instructions below use only general-purpose registers, so this helper
/// does not impose SIMD/FPU save-state requirements on an OS adapter.
///
/// # Safety
///
/// Every CPU that may execute this call must advertise SSE4.2 through CPUID
/// leaf 1 ECX bit 20. Checking only the CPU that makes the call is not enough:
/// the thread may move to another CPU while the call runs.
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
