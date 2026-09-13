//! Architecture register images.

/// Integer register image in architectural X0 through X30 order.
pub type GeneralRegisters = [u64; 31];

#[cfg(kernel_tls)]
pub use super::asm::{read_thread_pointer, write_thread_pointer};
pub use super::{asm::enable_fp, fp::FpState};

/// Reads the current exception level as an architectural value from 0 to 3.
#[inline]
pub fn current_exception_level() -> usize {
    let value: usize;
    // SAFETY: privileged code may read CurrentEL; no pointer is followed.
    unsafe { core::arch::asm!("mrs {}, CurrentEL", out(reg) value, options(nostack)) };
    (value >> 2) & 3
}

/// Reads the EL1 software thread-ID register without interpreting its value.
#[inline]
pub fn read_tpidr_el1() -> usize {
    let value;
    // SAFETY: this only reads a register in the documented execution mode.
    // Retain a compiler memory barrier around the software anchor access.
    unsafe { core::arch::asm!("mrs {}, tpidr_el1", out(reg) value, options(nostack)) };
    value
}

/// Installs the EL1 software thread-ID register.
///
/// # Safety
/// The caller must own the register installation boundary at EL1 or above,
/// retain any memory referenced by the installed value, and exclude code that
/// could observe a partially installed CPU or task state.
#[inline]
pub unsafe fn write_tpidr_el1(value: usize) {
    // SAFETY: the caller owns this register transition and its selected state.
    // Do not move memory operations across the change of software anchor.
    unsafe { core::arch::asm!("msr tpidr_el1, {}", in(reg) value, options(nostack)) };
}

/// Reads the EL2 software thread-ID register. Requires EL2 or above.
#[inline]
pub fn read_tpidr_el2() -> usize {
    let value;
    // SAFETY: this only reads a register in the documented execution mode.
    // Retain a compiler memory barrier around the software anchor access.
    unsafe { core::arch::asm!("mrs {}, tpidr_el2", out(reg) value, options(nostack)) };
    value
}

/// Installs the EL2 software thread-ID register.
///
/// # Safety
/// The caller must own the register installation boundary at EL2 or above,
/// retain any memory referenced by the installed value, and exclude code that
/// could observe a partially installed CPU or task state.
#[inline]
pub unsafe fn write_tpidr_el2(value: usize) {
    // SAFETY: the caller owns this register transition and its selected state.
    // Do not move memory operations across the change of software anchor.
    unsafe { core::arch::asm!("msr tpidr_el2, {}", in(reg) value, options(nostack)) };
}

/// Reads SP_EL0 while executing on the current EL's own stack.
#[inline]
pub fn read_sp_el0() -> usize {
    let value;
    // SAFETY: this only reads a register in the documented execution mode.
    // Retain a compiler memory barrier around the software anchor access.
    unsafe { core::arch::asm!("mrs {}, sp_el0", out(reg) value, options(nostack)) };
    value
}

/// Installs SP_EL0 while executing on the current EL's own stack.
///
/// # Safety
/// The caller must own the stack/anchor transition and keep the selected
/// object alive. Exception entry and subsequent Rust code must agree on the
/// meaning of the new value before either can use it.
#[inline]
pub unsafe fn write_sp_el0(value: usize) {
    // SAFETY: the caller owns this register transition and its selected state.
    // Do not move memory operations across the change of software anchor.
    unsafe { core::arch::asm!("msr sp_el0, {}", in(reg) value, options(nostack)) };
}
