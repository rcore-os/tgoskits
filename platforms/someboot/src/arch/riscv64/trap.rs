use crate::{SystimerArch, arch::Arch};

pub fn setup() {
    // SAFETY: someboot owns the supervisor stack and boot trap policy with IRQs masked.
    unsafe { ax_cpu::boot::install_boot_vector() };
}

pub fn trap_addr() -> usize {
    ax_cpu::boot::boot_vector()
}

struct BootTrap;
#[trait_ffi::impl_extern_trait]
impl ax_cpu::trap::boot::BootTrapHandler for BootTrap {
    fn handle(exception: &ax_cpu::trap::boot::BootException) {
        if exception.syndrome == (1u64 << 63) | 5 {
            Arch::systimer_ack();
            return;
        }
        panic!("Unhandled RISC-V boot trap: {exception:?}");
    }
}
