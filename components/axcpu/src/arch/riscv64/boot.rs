//! Helper functions to initialize the CPU states on systems bootstrapping.

/// Initializes trap handling on the current CPU.
///
/// In detail, it initializes the trap vector on RISC-V platforms.
pub fn init_trap() {
    unsafe extern "C" {
        fn trap_vector_base();
    }
    unsafe {
        #[cfg(feature = "uspace")]
        riscv::register::sstatus::set_sum();
        crate::asm::write_trap_vector_base(trap_vector_base as *const () as usize);
    }
}

/// Returns the current running address of the supervisor boot vector.
pub fn boot_vector() -> usize {
    super::entry::boot::vector()
}

/// Installs the supervisor boot vector in direct mode.
///
/// # Safety
/// Execute at S mode with IRQs masked and a valid supervisor stack and boot
/// handler. No user may enter through this vector; all handler code must remain
/// mapped until the owner installs runtime vectors.
pub unsafe fn install_boot_vector() {
    // SAFETY: the owner retains the CPU vector and the supervisor stack contract.
    unsafe {
        core::arch::asm!("csrw stvec, {}", in(reg) boot_vector(), options(nostack));
    }
}

/// Transfers to a boot entry on a fresh stack with one argument in a0.
///
/// # Safety
/// Entry and the aligned exclusive stack must be valid in the active address
/// space; the argument must satisfy the entry contract. Abandoned stack values
/// must not require destruction, and CPU state must satisfy the destination.
#[unsafe(naked)]
pub unsafe extern "C" fn jump_to(
    _argument: usize,
    _stack: crate::VirtAddr,
    _entry: crate::VirtAddr,
) -> ! {
    core::arch::naked_asm!("mv sp, a1", "jr a2");
}
