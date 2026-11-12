//! RISC-V EFI preparation before entering the shared SNP control plane.

mod clock;

pub(in crate::loader) use clock::{clock_frequency, clock_ticks};
use uefi::Status;

use crate::loader::{fdt::Fdt, snp};

// Native RISC-V requires RNG and returns when identity cannot be established.
pub(in crate::loader) const ALLOW_IDENTITY_COUNTER_FALLBACK: bool = false;
pub(in crate::loader) const RETRY_IDENTITY_FAILURE: bool = false;

/// Publishes the copied image to the RISC-V boot hart's instruction stream.
pub(in crate::loader) fn flush_instruction_cache() {
    // SAFETY: this boot hart copied the executable bytes into its uniquely
    // reserved image and enters the new code on the same hart.
    unsafe {
        core::arch::asm!("fence.i", options(nostack));
    }
}

pub(in crate::loader) fn run() -> Status {
    let _fdt = match Fdt::prepare() {
        Ok(fdt) => fdt,
        Err(error) => {
            crate::logln!("loader_fdt_unavailable: {error:?}");
            return error.status();
        }
    };
    snp::run()
}
