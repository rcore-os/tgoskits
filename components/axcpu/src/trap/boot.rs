//! Early exception handoff without runtime task or CPU-local dependencies.

#[cfg(target_arch = "aarch64")]
pub use crate::arch::current::trap::{TrapKind, TrapSource};

/// Copied machine state at an early exception boundary.
#[derive(Clone, Copy, Debug)]
pub struct BootException {
    /// Architecture-native saved integer registers.
    pub registers: crate::registers::GeneralRegisters,
    /// Saved exception return address.
    pub pc: usize,
    /// Interrupted stack pointer.
    pub sp: usize,
    /// Saved PSTATE, SSTATUS, PRMD or RFLAGS.
    pub status: u64,
    /// ESR, SCAUSE, ESTAT or x86 error code captured by the owning vector.
    pub syndrome: u64,
    /// FAR, STVAL, BADV or CR2 captured by the owning vector.
    pub fault_address: crate::VirtAddr,
    /// x86 exception vector; `syndrome` contains its hardware error code.
    #[cfg(target_arch = "x86_64")]
    pub vector: u8,
    /// Architectural exception level (1 or 2).
    #[cfg(target_arch = "aarch64")]
    pub level: u8,
    /// Synchronous, IRQ, FIQ or SError vector kind.
    #[cfg(target_arch = "aarch64")]
    pub kind: TrapKind,
    /// Vector source domain.
    #[cfg(target_arch = "aarch64")]
    pub source: TrapSource,
}

/// Early exception policy supplied by the boot owner.
/// The callback may run before runtime CPU-local, TLS or scheduling exists.
#[trait_ffi::def_extern_trait(mod_path = "trap::boot")]
pub trait BootTrapHandler {
    /// Reports or handles the copied exception without retaining stack references.
    /// Returning resumes the unchanged saved machine context. A fatal handler
    /// must use the boot owner's diagnostics without depending on runtime state.
    fn handle(exception: &BootException);
}
