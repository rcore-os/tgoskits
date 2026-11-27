//! LoongArch64 native EFI preparation and compatibility policy.

use core::arch::asm;

use uefi::Status;

pub(in crate::loader) const ALLOW_IDENTITY_COUNTER_FALLBACK: bool = true;
pub(in crate::loader) const RETRY_IDENTITY_FAILURE: bool = true;

pub(in crate::loader) fn run() -> uefi::Status {
    super::super::snp::run()
}

/// Returns the constant-counter frequency described by CPUCFG, in Hz.
pub(in crate::loader) fn clock_frequency() -> uefi::Result<u64> {
    let base_frequency = u64::from(read_cpucfg(4));
    let ratio = read_cpucfg(5);
    let multiplier = u64::from(ratio & 0xffff);
    let divisor = u64::from(ratio >> 16);
    let frequency = base_frequency
        .checked_mul(multiplier)
        .and_then(|frequency| frequency.checked_div(divisor))
        .filter(|frequency| *frequency != 0)
        .ok_or(Status::UNSUPPORTED)?;
    Ok(frequency)
}

pub(in crate::loader) fn clock_ticks() -> u64 {
    let ticks;
    // SAFETY: LoongArch UEFI exposes the constant counter to its boot image.
    // RDTIME.D only reads the counter and counter ID, without touching memory.
    unsafe { asm!("rdtime.d {}, {}", out(reg) ticks, out(reg) _, options(nomem, nostack)) };
    ticks
}

/// Makes bytes written by the loader visible to the target instruction stream.
pub(in crate::loader) fn flush_instruction_cache() {
    // SAFETY: The barriers have no memory operands and do not touch the stack.
    // They order the completed image writes before the following EFI entry call.
    unsafe {
        core::arch::asm!("dbar 0", "ibar 0", options(nostack));
    }
}

fn read_cpucfg(index: usize) -> u32 {
    let value: usize;
    // SAFETY: CPUCFG reads immutable CPU configuration without changing memory
    // or firmware state. Each configuration word occupies the low 32 bits.
    unsafe { asm!("cpucfg {}, {}", out(reg) value, in(reg) index, options(nomem, nostack)) };
    value as u32
}
