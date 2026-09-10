//! Wrapper functions for assembly instructions.

use core::arch::asm;

use aarch64_cpu::{asm::barrier, registers::*};
use ax_memory_addr::{PhysAddr, VirtAddr};

use super::asid::configured_tag_capacity;
#[cfg(kernel_tls)]
use crate::KernelTlsBase;
#[cfg(feature = "uspace")]
use crate::mmu::HardwareAddressSpace;

/// Returns the number of AArch64 ASIDs, including reserved ASID 0.
///
/// The result reflects both the hardware capability and the ASID width selected
/// by the boot owner in `TCR_EL1.AS`.
pub fn address_space_tag_capacity() -> u32 {
    configured_tag_capacity(
        ID_AA64MMFR0_EL1.read(ID_AA64MMFR0_EL1::ASIDBits),
        TCR_EL1.read(TCR_EL1::AS),
    )
}

#[cfg(feature = "uspace")]
fn flush_tlb_asid(asid: u16) {
    let operand = u64::from(asid) << 48;
    // SAFETY: the caller runs at EL1. The barriers match Linux's ASID
    // invalidation ordering: page-table stores, TLBI, completion, then fetch.
    unsafe {
        asm!(
            "dsb ishst; tlbi aside1is, {operand}; dsb ish; isb",
            operand = in(reg) operand,
        )
    }
}

/// Installs one complete userspace identity into TTBR0_EL1.
///
/// Tagged installation invalidates the incoming ASID before publishing the
/// root. Untagged installation invalidates every EL1 stage-one translation.
///
/// # Safety
///
/// The caller must own the current CPU with interrupts disabled and the root
/// must remain alive for the complete activation lease.
#[cfg(feature = "uspace")]
pub unsafe fn install_user_address_space(address_space: HardwareAddressSpace) {
    if address_space.hardware_tag() != 0 {
        let capacity = address_space_tag_capacity();
        if u32::from(address_space.hardware_tag()) < capacity {
            flush_tlb_asid(address_space.hardware_tag());
            let value = address_space.root().as_usize() as u64
                | (u64::from(address_space.hardware_tag()) << 48);
            TTBR0_EL1.set(value);
            barrier::isb(barrier::SY);
            return;
        }
    }

    TTBR0_EL1.set(address_space.root().as_usize() as u64);
    flush_tlb(None);
}

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

/// Waits for an interrupt after the caller masks local IRQ delivery.
///
/// AArch64 `WFI` observes enabled pending interrupt sources even while
/// `DAIF.I` masks delivery. Keeping delivery masked through `WFI` closes the
/// scheduler wake-loss window. The function returns with local IRQs enabled.
#[inline]
pub fn wait_for_irqs_disabled() {
    debug_assert!(!irqs_enabled());
    barrier::dsb(barrier::SY);
    aarch64_cpu::asm::wfi();
    enable_irqs();
}

/// Halt the current CPU.
#[inline]
pub fn halt() {
    disable_irqs();
    aarch64_cpu::asm::wfi(); // should never return
}

/// Reads the current page table root register for kernel space (`TTBR1_EL1`).
///
/// Returns the physical address of the page table root.
#[inline]
pub fn read_kernel_page_table() -> PhysAddr {
    super::mmu::El1::read_kernel_page_table()
}

/// Reads the current page table root register for user space (`TTBR0_EL1`).
///
/// Returns the physical address of the page table root.
#[inline]
pub fn read_user_page_table() -> PhysAddr {
    const TTBR_BADDR_MASK: u64 = (1 << 48) - 1;
    let root = TTBR0_EL1.get() & TTBR_BADDR_MASK;
    pa!(root as usize)
}

/// Writes the register to update the current page table root for kernel space
/// (`TTBR1_EL1`).
///
/// Note that the TLB is **NOT** flushed after this operation.
///
/// # Safety
///
/// This function is unsafe as it changes the virtual memory address space.
#[inline]
pub unsafe fn write_kernel_page_table(root_paddr: PhysAddr) {
    // SAFETY: this forwards the caller's EL1 mapping lifetime and execution contract.
    unsafe { super::mmu::El1::write_kernel_page_table(root_paddr) };
}

/// Writes the register to update the current page table root for user space
/// (`TTBR0_EL1`).
/// Note that the TLB is **NOT** flushed after this operation.
///
/// # Safety
///
/// This function is unsafe as it changes the virtual memory address space.
#[inline]
pub unsafe fn write_user_page_table(root_paddr: PhysAddr) {
    TTBR0_EL1.set(root_paddr.as_usize() as _);
}

/// Makes page-table writes visible to the inner-shareable domain.
///
/// Cross-CPU shootdown must execute this before sending any IPI. A barrier on
/// the remote CPU cannot order page-table writes performed by the initiating
/// CPU.
#[inline]
pub fn synchronize_page_table_writes() {
    unsafe { asm!("dsb ishst") };
}

/// Flushes the local TLB.
///
/// If `vaddr` is [`None`], flushes the entire TLB. Otherwise, flushes the TLB
/// entry that maps the given virtual address.
#[inline]
pub fn flush_tlb(vaddr: Option<VirtAddr>) {
    super::mmu::El1::flush_tlb(vaddr);
}

/// Makes a page-table entry installed by the local page-fault handler visible
/// before retrying the faulting instruction.
///
/// AArch64 page-table updates are coherent with the hardware walker. As in
/// Linux, avoiding an unconditional barrier here keeps the minor-fault fast
/// path cheap; a rare spurious refault is safe to handle again.
#[inline]
pub fn update_mmu_cache(_vaddr: VirtAddr) {}

/// Flushes the entire instruction cache.
#[inline]
pub fn flush_icache_all() {
    unsafe { asm!("ic iallu; dsb sy; isb") };
}

#[inline]
fn read_ctr_el0() -> u64 {
    let value;
    unsafe {
        asm!("mrs {}, ctr_el0", out(reg) value);
    }
    value
}

/// Reads the data cache line size from `CTR_EL0` and returns it in bytes.
#[inline]
pub fn dcache_line_size_from_ctr() -> usize {
    let ctr = read_ctr_el0();

    // CTR_EL0.DminLine: bits [19:16]
    // bytes = 4 << DminLine
    let dminline = ((ctr >> 16) & 0xf) as usize;

    4usize << dminline
}

/// Reads the instruction cache line size from `CTR_EL0` and returns it in bytes.
#[inline]
pub fn icache_line_size_from_ctr() -> usize {
    let ctr = read_ctr_el0();

    // CTR_EL0.IminLine: bits [3:0]
    // bytes = 4 << IminLine
    let iminline = (ctr & 0xf) as usize;

    4usize << iminline
}

/// Reads the current kernel task's TLS base (`TPIDR_EL0`).
///
/// It is used to implement TLS (Thread Local Storage).
#[inline]
#[cfg(kernel_tls)]
pub fn read_thread_pointer() -> KernelTlsBase {
    KernelTlsBase::new(TPIDR_EL0.get() as usize)
}

/// Writes the current kernel task's TLS base (`TPIDR_EL0`).
///
/// It is used to implement TLS (Thread Local Storage).
///
/// # Safety
///
/// This function is unsafe as it changes the current CPU states.
#[inline]
#[cfg(kernel_tls)]
pub unsafe fn write_thread_pointer(kernel_tls: KernelTlsBase) {
    TPIDR_EL0.set(kernel_tls.as_usize() as _)
}

/// Enable FP/SIMD instructions by setting the `FPEN` field in `CPACR_EL1`.
#[inline]
pub fn enable_fp() {
    CPACR_EL1.write(CPACR_EL1::FPEN::TrapNothing);
    barrier::isb(barrier::SY);
}

#[cfg(feature = "uspace")]
core::arch::global_asm!(include_str!("user_copy.S"), include_str!("user_atomic.S"),);

#[cfg(feature = "uspace")]
unsafe extern "C" {
    /// Copies data from source to destination, where addresses may be in user
    /// space. Equivalent to memcpy.
    ///
    /// # Safety
    /// This function is unsafe because it performs raw memory operations.
    ///
    /// # Returns
    /// Returns the number of bytes not copied. This means 0 indicates success,
    /// while a value > 0 indicates failure.
    pub fn user_copy(dst: *mut u8, src: *const u8, size: usize) -> usize;
}

/// Probes whether EL0 is permitted to access the page containing `vaddr` under
/// the *current* user translation regime (`TTBR0_EL1`), without taking any lock.
///
/// Uses the `AT S1E0R` / `AT S1E0W` address-translation instruction, which asks
/// the MMU to translate `vaddr` for the requested EL0 read or write access
/// and reports the result in `PAR_EL1`. `PAR_EL1.F == 0` means the translation
/// succeeded and the access is permitted — exactly the permission the CPU itself
/// enforces for a user-mode access, read lock-free. A not-present page or one
/// lacking the requested EL0 permission (e.g. a copy-on-write page probed for
/// write) reports `F == 1`.
///
/// Returns `true` iff the MMU would permit the EL0 access.
///
/// # Safety
///
/// The caller MUST invoke this with interrupts disabled. `PAR_EL1` is a per-CPU
/// scratch register shared across contexts; an interrupt executing another `AT`
/// between this `AT` and the `mrs` would clobber the result. On the
/// pointer-validation path that could turn an inaccessible page into a `true`
/// result and thus a raw kernel dereference of an unchecked address. IRQs-off
/// guarantees no other `AT` runs on this CPU in between. Because violating this
/// precondition is a memory-safety hazard (not merely a wrong answer), the
/// function is `unsafe` so every call site must establish it.
#[cfg(feature = "uspace")]
#[inline]
pub unsafe fn user_access_ok_page(vaddr: usize, access: crate::UserAccessType) -> bool {
    let par: u64;
    // SAFETY: `AT` reads the current translation tables and writes `PAR_EL1`;
    // `mrs` reads it back. No memory is accessed and no flags are clobbered. The
    // caller holds IRQs off so the `AT`/`mrs` pair is not split by another `AT`.
    unsafe {
        if access == crate::UserAccessType::Write {
            asm!(
                "at s1e0w, {vaddr}",
                "isb",
                "mrs {par}, par_el1",
                vaddr = in(reg) vaddr,
                par = out(reg) par,
                options(nostack, preserves_flags),
            );
        } else {
            asm!(
                "at s1e0r, {vaddr}",
                "isb",
                "mrs {par}, par_el1",
                vaddr = in(reg) vaddr,
                par = out(reg) par,
                options(nostack, preserves_flags),
            );
        }
    }
    // PAR_EL1.F (bit 0): 0 = translation succeeded and the EL0 access is allowed.
    par & 1 == 0
}
