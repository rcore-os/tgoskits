//! Wrapper functions for assembly instructions.

use core::arch::asm;

use aarch64_cpu::{asm::barrier, registers::*};
use memory_addr::{PhysAddr, VirtAddr};

/// Allows the current CPU to respond to interrupts.
///
/// In AArch64, it unmasks IRQs by clearing the I bit in the `DAIF` register.
#[inline]
pub fn enable_irqs() {
    unsafe { asm!("msr daifclr, #2") };
}

/// Makes the current CPU to ignore interrupts.
///
/// In AArch64, it masks IRQs by setting the I bit in the `DAIF` register.
#[inline]
pub fn disable_irqs() {
    unsafe { asm!("msr daifset, #2") };
}

/// Returns whether the current CPU is allowed to respond to interrupts.
///
/// In AArch64, it checks the I bit in the `DAIF` register.
#[inline]
pub fn irqs_enabled() -> bool {
    !DAIF.matches_all(DAIF::I::Masked)
}

/// Relaxes the current CPU and waits for interrupts.
///
/// It must be called with interrupts enabled, otherwise it will never return.
#[inline]
pub fn wait_for_irqs() {
    aarch64_cpu::asm::wfi();
}

/// Halt the current CPU.
#[inline]
pub fn halt() {
    disable_irqs();
    aarch64_cpu::asm::wfi(); // should never return
}

/// Reads the current page table root register for kernel space (`TTBR1_EL1`).
///
/// When the "arm-el2" feature is enabled,
/// TTBR0_EL2 is dedicated to the Hypervisor's Stage-2 page table base address.
///
/// Returns the physical address of the page table root.
#[inline]
pub fn read_kernel_page_table() -> PhysAddr {
    #[cfg(not(feature = "arm-el2"))]
    let root = TTBR1_EL1.get();

    #[cfg(feature = "arm-el2")]
    let root = TTBR0_EL2.get();

    pa!(root as usize)
}

/// Reads the current page table root register for user space (`TTBR0_EL1`).
///
/// When the "arm-el2" feature is enabled, for user-mode programs,
/// virtualization is completely transparent to them, so there is no need to modify
///
/// Returns the physical address of the page table root.
#[inline]
pub fn read_user_page_table() -> PhysAddr {
    let root = TTBR0_EL1.get();
    pa!(root as usize)
}

/// Writes the register to update the current page table root for kernel space
/// (`TTBR1_EL1`).
///
/// When the "arm-el2" feature is enabled,
/// TTBR0_EL2 is dedicated to the Hypervisor's Stage-2 page table base address.
///
/// Note that the TLB is **NOT** flushed after this operation.
///
/// # Safety
///
/// This function is unsafe as it changes the virtual memory address space.
#[inline]
pub unsafe fn write_kernel_page_table(root_paddr: PhysAddr) {
    #[cfg(not(feature = "arm-el2"))]
    {
        // kernel space page table use TTBR1 (0xffff_0000_0000_0000..0xffff_ffff_ffff_ffff)
        TTBR1_EL1.set(root_paddr.as_usize() as _);
    }

    #[cfg(feature = "arm-el2")]
    {
        // kernel space page table at EL2 use TTBR0_EL2 (0x0000_0000_0000_0000..0x0000_ffff_ffff_ffff)
        TTBR0_EL2.set(root_paddr.as_usize() as _);
    }
}

/// Writes the register to update the current page table root for user space
/// (`TTBR1_EL0`).
/// When the "arm-el2" feature is enabled, for user-mode programs,
/// virtualization is completely transparent to them, so there is no need to modify
///
/// Note that the TLB is **NOT** flushed after this operation.
///
/// # Safety
///
/// This function is unsafe as it changes the virtual memory address space.
#[inline]
pub unsafe fn write_user_page_table(root_paddr: PhysAddr) {
    TTBR0_EL1.set(root_paddr.as_usize() as _);
}

/// Flushes the TLB.
///
/// If `vaddr` is [`None`], flushes the entire TLB. Otherwise, flushes the TLB
/// entry that maps the given virtual address.
#[inline]
pub fn flush_tlb(vaddr: Option<VirtAddr>) {
    unsafe {
        if let Some(vaddr) = vaddr {
            const VA_MASK: usize = (1 << 44) - 1; // 0xFFFF_FFFF_FFFF
            let operand = (vaddr.as_usize() >> 12) & VA_MASK;

            #[cfg(not(feature = "arm-el2"))]
            {
                asm!("tlbi vaae1is, {}; dsb sy; isb", in(reg) operand)
            }
            #[cfg(feature = "arm-el2")]
            {
                asm!("tlbi vae2is, {}; dsb sy; isb", in(reg) operand)
            }
        } else {
            // flush the entire TLB
            #[cfg(not(feature = "arm-el2"))]
            {
                asm!("tlbi vmalle1; dsb sy; isb")
            }
            #[cfg(feature = "arm-el2")]
            {
                asm!("tlbi alle2; dsb sy; isb")
            }
        }
    }
}

/// Flushes the entire instruction cache.
#[inline]
pub fn flush_icache_all() {
    unsafe { asm!("ic iallu; dsb sy; isb") };
}

/// Flushes the data cache line (64 bytes) at the given virtual address
#[inline]
pub fn flush_dcache_line(vaddr: VirtAddr) {
    unsafe { asm!("dc ivac, {0:x}; dsb sy; isb", in(reg) vaddr.as_usize()) };
}

/// Writes exception vector base address register (`VBAR_EL1`).
///
/// # Safety
///
/// This function is unsafe as it changes the exception handling behavior of the
/// current CPU.
#[inline]
pub unsafe fn write_exception_vector_base(vbar: usize) {
    #[cfg(not(feature = "arm-el2"))]
    VBAR_EL1.set(vbar as _);
    #[cfg(feature = "arm-el2")]
    VBAR_EL2.set(vbar as _);
}

/// Reads the thread pointer of the current CPU (`TPIDR_EL0`).
///
/// It is used to implement TLS (Thread Local Storage).
#[inline]
pub fn read_thread_pointer() -> usize {
    TPIDR_EL0.get() as usize
}

/// Writes the thread pointer of the current CPU (`TPIDR_EL0`).
///
/// It is used to implement TLS (Thread Local Storage).
///
/// # Safety
///
/// This function is unsafe as it changes the current CPU states.
#[inline]
pub unsafe fn write_thread_pointer(tpidr_el0: usize) {
    TPIDR_EL0.set(tpidr_el0 as _)
}

/// Enable FP/SIMD instructions by setting the `FPEN` field in `CPACR_EL1`.
#[inline]
pub fn enable_fp() {
    CPACR_EL1.write(CPACR_EL1::FPEN::TrapNothing);
    barrier::isb(barrier::SY);
}
