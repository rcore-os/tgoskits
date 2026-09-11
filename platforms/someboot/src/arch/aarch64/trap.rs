//! Boot exception policy over CPU-owned vectors and saved registers.

use ax_cpu::trap::boot::{BootException, BootTrapHandler, TrapKind};

struct BootTraps;

#[trait_ffi::impl_extern_trait]
impl BootTrapHandler for BootTraps {
    fn handle(exception: &BootException) {
        match exception.kind {
            TrapKind::Synchronous => match (exception.syndrome >> 26) & 0x3f {
                0x15 => log::warn!("No syscall is supported during boot"),
                0x3c => {}
                _ => panic!("Unhandled boot exception: {exception:#x?}"),
            },
            _ => panic!("Unexpected boot interrupt: {exception:#x?}"),
        }
    }
}

pub fn setup() {
    // SAFETY: someboot installs its vector after mapping the image and stack,
    // before enabling IRQs or transferring ownership to the runtime.
    unsafe {
        match ax_cpu::registers::current_exception_level() {
            1 => ax_cpu::boot::El1::init_boot_trap(),
            2 => ax_cpu::boot::El2::init_boot_trap(),
            _ => panic!("unsupported boot exception level"),
        }
    }
}

pub fn trap_addr() -> usize {
    match ax_cpu::registers::current_exception_level() {
        1 => ax_cpu::boot::El1::vector_base().as_usize(),
        2 => ax_cpu::boot::El2::vector_base().as_usize(),
        _ => panic!("unsupported boot exception level"),
    }
}
