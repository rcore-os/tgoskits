//! Wrapper functions for assembly instructions.

use core::arch::{
    asm,
    x86_64::{__cpuid, __cpuid_count},
};

use ax_memory_addr::{MemoryAddr, PhysAddr, VirtAddr};
#[cfg(feature = "tls")]
use x86::msr;
use x86::{controlregs, tlb};
#[cfg(feature = "uspace")]
use x86_64::instructions::tlb::Pcid;
use x86_64::instructions::{
    interrupts,
    tlb::{InvPcidCommand, flush_pcid},
};

#[cfg(feature = "tls")]
use crate::KernelTlsBase;
#[cfg(feature = "uspace")]
use crate::{InstalledAddressSpace, InstalledAddressSpaceMode};

#[cfg(any(not(feature = "host-test"), feature = "uspace"))]
const PCID_CAPACITY: u32 = 1 << 12;
#[cfg(feature = "uspace")]
const CR3_NOFLUSH: u64 = 1 << 63;

fn pcid_invpcid_supported() -> bool {
    let basic = __cpuid(1);
    let maximum = __cpuid(0).eax;
    let extended = (maximum >= 7).then(|| __cpuid_count(7, 0));
    basic.ecx & (1 << 17) != 0 && extended.is_some_and(|features| features.ebx & (1 << 10) != 0)
}

fn pcid_enabled() -> bool {
    // SAFETY: this backend executes at CPL0.
    unsafe { controlregs::cr4() }.contains(controlregs::Cr4::CR4_ENABLE_PCID)
}

#[cfg(feature = "uspace")]
fn ensure_pcid_enabled() -> bool {
    if !pcid_invpcid_supported() {
        return false;
    }
    // SAFETY: this backend executes at CPL0 with scheduling serialized.
    let mut cr4 = unsafe { controlregs::cr4() };
    if cr4.contains(controlregs::Cr4::CR4_ENABLE_PCID) {
        return true;
    }
    if !cr4.contains(controlregs::Cr4::CR4_ENABLE_GLOBAL_PAGES) {
        return false;
    }
    // Intel requires CR3[11:0] == 0 while CR4.PCIDE changes from 0 to 1.
    // SAFETY: reading CR3 at CPL0 is well-defined.
    if unsafe { controlregs::cr3() } & 0xfff != 0 {
        return false;
    }
    cr4.insert(controlregs::Cr4::CR4_ENABLE_PCID);
    // SAFETY: CPUID confirmed PCID and the CR3/PGE prerequisites above hold.
    unsafe { controlregs::cr4_write(cr4) };
    true
}

/// Returns the number of usable x86 PCID values, including reserved PCID 0.
///
/// Linux enables PCID only when PCID, INVPCID, and global pages are all
/// available. Returning one selects the architecture-neutral full-flush path.
pub fn address_space_tag_capacity(_cpu_count: usize) -> u32 {
    #[cfg(feature = "host-test")]
    {
        1
    }
    #[cfg(not(feature = "host-test"))]
    {
        // SAFETY: this capability is queried after privileged CPU initialization.
        let pge = unsafe { controlregs::cr4() }.contains(controlregs::Cr4::CR4_ENABLE_GLOBAL_PAGES);
        if pge && pcid_invpcid_supported() {
            PCID_CAPACITY
        } else {
            1
        }
    }
}

/// Installs one complete userspace identity into CR3.
///
/// Tagged installation invalidates the incoming PCID before a no-flush CR3
/// write. This conservative per-install invalidation is the ownership boundary
/// for tag reuse: an inactive stale translation can never become reachable
/// when its address space is scheduled again. Unsupported CPUs use PCID 0 and
/// a complete invalidation.
///
/// # Safety
///
/// The caller must own the current CPU with interrupts disabled and the root
/// must remain alive for the complete activation lease.
#[cfg(feature = "uspace")]
pub unsafe fn install_user_address_space(address_space: InstalledAddressSpace) {
    let root = address_space.root().as_usize() as u64;
    let tagged = matches!(address_space.mode(), InstalledAddressSpaceMode::Tagged)
        && u32::from(address_space.hardware_tag()) < PCID_CAPACITY
        && ensure_pcid_enabled();
    if tagged {
        let Ok(pcid) = Pcid::new(address_space.hardware_tag()) else {
            // Constructor validation and the capacity check make this branch
            // unreachable, but the fallback keeps an injected identity safe.
            unsafe { controlregs::cr3_write(root) };
            return;
        };
        // SAFETY: `ensure_pcid_enabled` confirmed INVPCID and CR4.PCIDE.
        unsafe { flush_pcid(InvPcidCommand::Single(pcid)) };
        // SAFETY: the root is aligned, PCID is 12-bit, and CR4.PCIDE is set.
        unsafe {
            controlregs::cr3_write(root | u64::from(address_space.hardware_tag()) | CR3_NOFLUSH)
        };
    } else {
        if pcid_enabled() && pcid_invpcid_supported() {
            // SAFETY: CPUID confirmed INVPCID; this also discharges CPU-offline
            // and generation-rollover obligations for inactive PCIDs.
            unsafe { flush_pcid(InvPcidCommand::All) };
        }
        // SAFETY: a zero-PCID CR3 write installs the validated aligned root.
        unsafe { controlregs::cr3_write(root) };
    }
}

/// Allows the current CPU to respond to interrupts.
#[inline]
pub fn enable_irqs() {
    interrupts::enable()
}

/// Makes the current CPU to ignore interrupts.
#[inline]
pub fn disable_irqs() {
    interrupts::disable()
}

/// Returns whether the current CPU is allowed to respond to interrupts.
#[inline]
pub fn irqs_enabled() -> bool {
    interrupts::are_enabled()
}

/// Relaxes the current CPU and waits for interrupts.
///
/// It must be called with interrupts enabled, otherwise it will never return.
#[inline]
pub fn wait_for_irqs() {
    unsafe { asm!("hlt") }
}

/// Halt the current CPU.
#[inline]
pub fn halt() {
    disable_irqs();
    wait_for_irqs(); // should never return
}

/// Reads the current page table root register for user space (`CR3`).
///
/// x86_64 does not have a separate page table root register for user and
/// kernel space, so this operation is the same as [`read_kernel_page_table`].
///
/// Returns the physical address of the page table root.
#[inline]
pub fn read_user_page_table() -> PhysAddr {
    pa!(unsafe { controlregs::cr3() } as usize).align_down_4k()
}

/// Reads the current page table root register for kernel space (`CR3`).
///
/// x86_64 does not have a separate page table root register for user and
/// kernel space, so this operation is the same as [`read_user_page_table`].
///
/// Returns the physical address of the page table root.
#[inline]
pub fn read_kernel_page_table() -> PhysAddr {
    read_user_page_table()
}

/// Writes the register to update the current page table root for user space
/// (`CR3`).
///
/// x86_64 does not have a separate page table root register for user
/// and kernel space, so this operation is the same as [`write_kernel_page_table`].
///
/// Note that the TLB will be **flushed** after this operation.
///
/// # Safety
///
/// This function is unsafe as it changes the virtual memory address space.
#[inline]
pub unsafe fn write_user_page_table(root_paddr: PhysAddr) {
    unsafe { controlregs::cr3_write(root_paddr.as_usize() as _) }
}

/// Writes the register to update the current page table root for kernel space
/// (`CR3`).
///
/// x86_64 does not have a separate page table root register for user
/// and kernel space, so this operation is the same as [`write_user_page_table`].
///
/// Note that the TLB will be **flushed** after this operation.
///
/// # Safety
///
/// This function is unsafe as it changes the virtual memory address space.
#[inline]
pub unsafe fn write_kernel_page_table(root_paddr: PhysAddr) {
    unsafe { write_user_page_table(root_paddr) }
}

/// Flushes the entire instruction cache.
#[inline]
pub fn flush_icache_all() {}

/// Flushes the TLB.
///
/// If `vaddr` is [`None`], flushes the entire TLB. Otherwise, flushes the TLB
/// entry that maps the given virtual address.
#[inline]
pub fn flush_tlb(vaddr: Option<VirtAddr>) {
    if let Some(vaddr) = vaddr {
        unsafe { tlb::flush(vaddr.into()) }
    } else {
        if pcid_enabled() && pcid_invpcid_supported() {
            // SAFETY: CPUID confirmed INVPCID and CR4.PCIDE is enabled.
            unsafe { flush_pcid(InvPcidCommand::All) }
        } else {
            unsafe { tlb::flush_all() }
        }
    }
}

/// Makes a page-table entry installed by the local page-fault handler visible
/// before retrying the faulting instruction.
///
/// x86 does not cache invalid leaf entries, so the page-table write is enough.
#[inline]
pub fn update_mmu_cache(_vaddr: VirtAddr) {}

/// Reads the current kernel task's TLS base (`FS_BASE`).
///
/// It is used to implement TLS (Thread Local Storage).
#[inline]
#[cfg(feature = "tls")]
pub fn read_thread_pointer() -> KernelTlsBase {
    KernelTlsBase::new(unsafe { msr::rdmsr(msr::IA32_FS_BASE) as usize })
}

/// Writes the current kernel task's TLS base (`FS_BASE`).
///
/// It is used to implement TLS (Thread Local Storage).
///
/// # Safety
///
/// This function is unsafe as it changes the CPU states.
#[inline]
#[cfg(feature = "tls")]
pub unsafe fn write_thread_pointer(kernel_tls: KernelTlsBase) {
    unsafe { msr::wrmsr(msr::IA32_FS_BASE, kernel_tls.as_usize() as u64) }
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
