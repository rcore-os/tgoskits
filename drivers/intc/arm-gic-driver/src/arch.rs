//! Architecture operations used by the GIC implementation.

#[cfg(target_arch = "aarch64")]
use aarch64_cpu::{
    asm::barrier,
    registers::{CurrentEL, MPIDR_EL1, Readable},
};

#[inline]
pub(crate) fn isb() {
    #[cfg(target_arch = "aarch64")]
    barrier::isb(barrier::SY);
}

#[inline]
pub(crate) fn dsb() {
    #[cfg(target_arch = "aarch64")]
    barrier::dsb(barrier::SY);
}

#[inline]
pub(crate) fn mpidr() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        MPIDR_EL1.get()
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        0
    }
}

#[inline]
pub(crate) fn current_el() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        CurrentEL.read(CurrentEL::EL)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        0
    }
}
