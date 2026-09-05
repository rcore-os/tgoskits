//! Wrapper functions for assembly instructions.

use core::arch::asm;

use aarch64_cpu::{asm::barrier, registers::*};
use ax_memory_addr::{PhysAddr, VirtAddr};

#[cfg(not(feature = "arm-el2"))]
use super::asid::configured_tag_capacity;
#[cfg(feature = "tls")]
use crate::KernelTlsBase;
#[cfg(feature = "uspace")]
use crate::{InstalledAddressSpace, InstalledAddressSpaceMode};

/// Returns the number of AArch64 ASIDs, including reserved ASID 0.
///
/// The result reflects both the hardware capability and the ASID width selected
/// by the boot owner in `TCR_EL1.AS`. EL2 builds retain the conservative
/// full-flush path because their userspace translation register contract is
/// different from TTBR0_EL1.
pub fn address_space_tag_capacity(_cpu_count: usize) -> u32 {
    #[cfg(feature = "arm-el2")]
    {
        1
    }
    #[cfg(not(feature = "arm-el2"))]
    {
        configured_tag_capacity(
            ID_AA64MMFR0_EL1.read(ID_AA64MMFR0_EL1::ASIDBits),
            TCR_EL1.read(TCR_EL1::AS),
        )
    }
}

#[cfg(all(feature = "uspace", not(feature = "arm-el2")))]
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
/// root. Full-flush and EL2 fallback paths install ASID 0 and invalidate every
/// stage-1 translation.
///
/// # Safety
///
/// The caller must own the current CPU with interrupts disabled and the root
/// must remain alive for the complete activation lease.
#[cfg(feature = "uspace")]
pub unsafe fn install_user_address_space(address_space: InstalledAddressSpace) {
    #[cfg(not(feature = "arm-el2"))]
    if matches!(address_space.mode(), InstalledAddressSpaceMode::Tagged) {
        let capacity = address_space_tag_capacity(1);
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
    const TTBR_BADDR_MASK: u64 = (1 << 48) - 1;
    let root = TTBR0_EL1.get() & TTBR_BADDR_MASK;
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
    if let Some(vaddr) = vaddr {
        const VA_MASK: usize = (1 << 44) - 1; // VA[55:12] => bits[43:0]
        let operand = (vaddr.as_usize() >> 12) & VA_MASK;

        #[cfg(not(feature = "arm-el2"))]
        unsafe {
            // SAFETY: this runs at EL1. Complete the preceding descriptor
            // stores before invalidating walk caches, then wait for completion
            // before any retired frame can be reused.
            asm!("dsb ishst; tlbi vaae1is, {}; dsb sy; isb", in(reg) operand)
        }
        #[cfg(feature = "arm-el2")]
        unsafe {
            // SAFETY: this build owns the EL2 translation regime. The store
            // barrier and completion barrier surround the EL2 invalidation.
            asm!("dsb ishst; tlbi vae2is, {}; dsb sy; isb", in(reg) operand)
        }
    } else {
        // flush the entire TLB
        #[cfg(not(feature = "arm-el2"))]
        unsafe {
            // TLB Invalidate by VMID, All at stage 1, EL1
            asm!("dsb sy; isb; tlbi vmalle1; dsb sy; isb")
        }
        #[cfg(feature = "arm-el2")]
        unsafe {
            // SAFETY: this build owns the EL2 translation regime. The HAL
            // targets every required CPU and waits for its local completion.
            asm!("dsb sy; tlbi alle2; dsb sy; isb")
        }
    }
}

/// Makes a page-table entry installed by the local page-fault handler visible
/// before retrying the faulting instruction.
///
/// AArch64 page-table updates are coherent with the hardware walker. A rare
/// spurious refault is safe to handle again, so no unconditional barrier is
/// needed on the minor-fault fast path.
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

/// Cleans a data cache range to the point of unification.
#[inline]
pub fn clean_dcache_range_to_pou(vaddr: VirtAddr, size: usize) {
    if size == 0 {
        return;
    }

    let line_size = dcache_line_size_from_ctr();
    let start = vaddr.as_usize() & !(line_size - 1);
    let end = (vaddr.as_usize() + size + line_size - 1) & !(line_size - 1);

    for line in (start..end).step_by(line_size) {
        unsafe { asm!("dc cvau, {0:x}", in(reg) line) };
    }

    unsafe { asm!("dsb sy") };
}

/// Cleans and invalidates the data cache line that covers the given address.
///
/// This is useful for publishing small pieces of data to other agents that may
/// observe memory outside the local D-cache, such as spin tables used to start
/// secondary CPUs.
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

/// Reads the current kernel task's TLS base (`TPIDR_EL0`).
///
/// It is used to implement TLS (Thread Local Storage).
#[inline]
#[cfg(feature = "tls")]
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
#[cfg(feature = "tls")]
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
#[cfg(all(feature = "uspace", not(feature = "arm-el2")))]
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

/// `arm-el2` builds run the hypervisor at EL2, where the EL1&0 `AT` probe does
/// not describe guest-user access, so always fall back to the locked slow path.
///
/// # Safety
///
/// No precondition — this stub reads nothing and always returns `false`. It is
/// `unsafe` only to share the signature of the aarch64 EL1 probe (which requires
/// IRQs-off), so callers can use one `unsafe` block across all targets.
#[cfg(all(feature = "uspace", feature = "arm-el2"))]
#[inline]
pub unsafe fn user_access_ok_page(_vaddr: usize, _access: crate::UserAccessType) -> bool {
    false
}
