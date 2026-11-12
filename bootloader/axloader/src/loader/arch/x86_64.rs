//! x86_64 native EFI preparation and compatibility policy.

use core::{
    arch::x86_64::{_mm_lfence, _rdtsc},
    time::Duration,
};

use uefi::Status;

pub(in crate::loader) const ALLOW_IDENTITY_COUNTER_FALLBACK: bool = true;
pub(in crate::loader) const RETRY_IDENTITY_FAILURE: bool = true;

pub(in crate::loader) fn run() -> uefi::Status {
    super::super::snp::run()
}

/// Calibrates the boot CPU's TSC while boot services remain available.
pub(in crate::loader) fn clock_frequency() -> uefi::Result<u64> {
    const CALIBRATION_MICROSECONDS: u64 = 20_000;

    // The native EFI network loop stays on the boot CPU. A long enough stall
    // keeps firmware call overhead small relative to the calibration window.
    let start = clock_ticks();
    uefi::boot::stall(Duration::from_micros(CALIBRATION_MICROSECONDS));
    let elapsed = clock_ticks()
        .checked_sub(start)
        .ok_or(Status::DEVICE_ERROR)?;
    let frequency = elapsed
        .checked_mul(1_000_000)
        .map(|ticks| ticks / CALIBRATION_MICROSECONDS)
        .filter(|frequency| *frequency != 0)
        .ok_or(Status::UNSUPPORTED)?;
    Ok(frequency)
}

pub(in crate::loader) fn clock_ticks() -> u64 {
    // SAFETY: x86_64 UEFI executes with access to the TSC and SSE2 fences.
    // These instructions only read the counter and order instruction execution;
    // they do not access memory. The fences keep calibration reads around Stall.
    unsafe {
        _mm_lfence();
        let ticks = _rdtsc();
        _mm_lfence();
        ticks
    }
}

/// x86 uses coherent instruction and data caches for normal memory.
pub(in crate::loader) fn flush_instruction_cache() {}
