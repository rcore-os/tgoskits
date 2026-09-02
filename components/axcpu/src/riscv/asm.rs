//! Wrapper functions for assembly instructions.

use ax_memory_addr::{PhysAddr, VirtAddr};
use riscv::{
    asm,
    register::{satp, sstatus, stvec},
};

#[cfg(feature = "tls")]
use crate::KernelTlsBase;
#[cfg(feature = "uspace")]
use crate::{InstalledAddressSpace, InstalledAddressSpaceMode};

/// Probes the implemented SATP ASID width and returns its capacity, including
/// reserved ASID 0.
///
/// Linux enables the allocator only when the hardware exposes more than twice
/// the possible CPU count. The write/readback probe is restored immediately
/// and followed by a complete fence because probing itself can create tagged
/// TLB state.
pub fn address_space_tag_capacity(cpu_count: usize) -> u32 {
    let original = satp::read();
    // SAFETY: the original root and mode remain installed; only the ASID field
    // is probed and the complete register is restored below.
    unsafe { satp::set(original.mode(), u16::MAX as usize, original.ppn()) };
    let mask = satp::read().asid();
    // SAFETY: restore the exact architectural mode, ASID, and root preimage.
    unsafe { satp::set(original.mode(), original.asid(), original.ppn()) };
    asm::sfence_vma_all();

    let capacity = mask
        .checked_add(1)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(1);
    if capacity as usize > cpu_count.saturating_mul(2) {
        capacity
    } else {
        1
    }
}

#[cfg(feature = "uspace")]
fn flush_tlb_asid(asid: u16) {
    // A literal x0 in rs1 means every address; the register in rs2 selects one
    // ASID. Passing numeric zero through the generic wrapper would select ASID
    // zero instead of the architectural all-ASID encoding.
    unsafe {
        core::arch::asm!(
            "sfence.vma x0, {asid}",
            asid = in(reg) usize::from(asid),
            options(nostack),
        )
    }
}

/// Installs one complete userspace identity into SATP.
///
/// Tagged installation writes the root and then invalidates the incoming ASID.
/// Unsupported or explicitly full-flush identities use ASID 0 and a complete
/// `SFENCE.VMA`.
///
/// # Safety
///
/// The caller must own the current CPU with interrupts disabled and the root
/// must remain alive for the complete activation lease.
#[cfg(feature = "uspace")]
pub unsafe fn install_user_address_space(address_space: InstalledAddressSpace) {
    // The allocator issues `Tagged` only after the BSP SATP write/readback
    // probe passes Linux's ASID-count threshold. RISC-V requires secondary
    // harts to expose a compatible SATP format.
    let tagged = matches!(address_space.mode(), InstalledAddressSpaceMode::Tagged);
    let asid = if tagged {
        usize::from(address_space.hardware_tag())
    } else {
        0
    };
    // Preserve the boot-selected translation mode. Linux likewise replaces
    // only ASID and PPN when switching an MM; assuming Sv39 would corrupt an
    // Sv48/Sv57 kernel context.
    let mode = satp::read().mode();
    // SAFETY: the root is page-aligned and the typed tag fits SATP.ASID.
    unsafe { satp::set(mode, asid, address_space.root().as_usize() >> 12) };
    if tagged {
        flush_tlb_asid(address_space.hardware_tag());
    } else {
        asm::sfence_vma_all();
    }
}

/// Allows the current CPU to respond to interrupts.
#[inline]
pub fn enable_irqs() {
    unsafe { sstatus::set_sie() }
}

/// Makes the current CPU to ignore interrupts.
#[inline]
pub fn disable_irqs() {
    unsafe { sstatus::clear_sie() }
}

/// Returns whether the current CPU is allowed to respond to interrupts.
#[inline]
pub fn irqs_enabled() -> bool {
    sstatus::read().sie()
}

/// Relaxes the current CPU and waits for interrupts.
///
/// It must be called with interrupts enabled, otherwise it will never return.
#[inline]
pub fn wait_for_irqs() {
    riscv::asm::wfi()
}

/// Halt the current CPU.
#[inline]
pub fn halt() {
    disable_irqs();
    riscv::asm::wfi() // should never return
}

/// Reads the current page table root register for user space (`satp`).
///
/// RISC-V does not have a separate page table root register for user and
/// kernel space, so this operation is the same as [`read_kernel_page_table`].
///
/// Returns the physical address of the page table root.
#[inline]
pub fn read_user_page_table() -> PhysAddr {
    pa!(satp::read().ppn() << 12)
}

/// Reads the current page table root register for kernel space (`satp`).
///
/// RISC-V does not have a separate page table root register for user and
/// kernel space, so this operation is the same as [`read_user_page_table`].
///
/// Returns the physical address of the page table root.
#[inline]
pub fn read_kernel_page_table() -> PhysAddr {
    read_user_page_table()
}

/// Writes the register to update the current page table root for user space
/// (`satp`).
///
/// RISC-V does not have a separate page table root register for user
/// and kernel space, so this operation is the same as [`write_kernel_page_table`].
///
/// Note that the TLB is **NOT** flushed after this operation.
///
/// # Safety
///
/// This function is unsafe as it changes the virtual memory address space.
#[inline]
pub unsafe fn write_user_page_table(root_paddr: PhysAddr) {
    let mode = satp::read().mode();
    unsafe { satp::set(mode, 0, root_paddr.as_usize() >> 12) };
}

/// Writes the register to update the current page table root for user space
/// (`satp`).
///
/// RISC-V does not have a separate page table root register for user
/// and kernel space, so this operation is the same as [`write_user_page_table`].
///
/// Note that the TLB is **NOT** flushed after this operation.
///
/// # Safety
///
/// This function is unsafe as it changes the virtual memory address space.
#[inline]
pub unsafe fn write_kernel_page_table(root_paddr: PhysAddr) {
    unsafe { write_user_page_table(root_paddr) };
}

/// Flushes the entire instruction cache.
#[inline]
pub fn flush_icache_all() {
    riscv::asm::fence_i();
}

/// Flushes the TLB.
///
/// If `vaddr` is [`None`], flushes the entire TLB. Otherwise, flushes the TLB
/// entry that maps the given virtual address.
#[inline]
pub fn flush_tlb(vaddr: Option<VirtAddr>) {
    if let Some(vaddr) = vaddr {
        // A literal x0 in rs2 invalidates this address for every ASID.
        unsafe {
            core::arch::asm!(
                "sfence.vma {addr}, x0",
                addr = in(reg) vaddr.as_usize(),
                options(nostack),
            )
        }
    } else {
        asm::sfence_vma_all();
    }
}

/// Makes a page-table entry installed by the local page-fault handler visible
/// before retrying the faulting instruction.
///
/// RISC-V permits implementations to cache invalid entries, so an `SFENCE.VMA`
/// is required after turning an invalid entry into a valid one.
#[inline]
pub fn update_mmu_cache(vaddr: VirtAddr) {
    flush_tlb(Some(vaddr));
}

/// Writes the Supervisor Trap Vector Base Address register (`stvec`).
///
/// # Safety
///
/// This function is unsafe as it changes the exception handling behavior of the
/// current CPU.
#[inline]
pub unsafe fn write_trap_vector_base(stvec: usize) {
    let mut reg = stvec::read();
    reg.set_address(stvec);
    reg.set_trap_mode(stvec::TrapMode::Direct);
    unsafe { stvec::write(reg) }
}

/// Reads the current task's kernel thread pointer (`tp`).
///
/// The value is task-owned kernel TLS. CPU-local state is anchored by
/// `sscratch` and must not be inferred from this register.
#[inline]
#[cfg(feature = "tls")]
pub fn read_thread_pointer() -> KernelTlsBase {
    let tp;
    unsafe { core::arch::asm!("mv {}, tp", out(reg) tp) };
    KernelTlsBase::new(tp)
}

/// Writes the current task's kernel thread pointer (`tp`).
///
/// The value is task-owned kernel TLS. CPU-local state is anchored by
/// `sscratch` and must not be installed through this API.
///
/// # Safety
///
/// The caller must ensure that `tls_base` belongs to the execution context
/// currently being installed and remains valid while that context can run.
#[inline]
#[cfg(feature = "tls")]
pub unsafe fn write_thread_pointer(tls_base: KernelTlsBase) {
    unsafe { core::arch::asm!("mv tp, {}", in(reg) tls_base.as_usize()) }
}

#[cfg(feature = "uspace")]
core::arch::global_asm!(
    include_asm_macros!(),
    include_str!("user_copy.S"),
    include_str!("user_atomic.S"),
);

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

/// Lock-free EL0/user access probe. No hardware address-translation probe is
/// wired up on this architecture yet, so always report a present-page probe miss
/// and let the caller take the locked slow path (correctness preserved).
///
/// # Safety
///
/// No precondition — this stub reads nothing and always returns `false`. It is
/// `unsafe` only to share the signature of the aarch64 EL1 probe (which requires
/// IRQs-off), so callers can use one `unsafe` block across all targets.
#[cfg(feature = "uspace")]
#[inline]
pub unsafe fn user_access_ok_page(_vaddr: usize, _access: crate::UserAccessType) -> bool {
    false
}
