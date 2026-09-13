use aarch64_cpu::registers::*;
use page_table_generic::VirtAddr;

use crate::{arch::entry::el_entry, mem::PageTableInfo, timer};

pub fn switch_to_elx() -> ! {
    unsafe extern "C" {
        fn __cpu0_stack_top();
    }
    // SAFETY: assembly startup installed the dedicated boot stack; SP_EL0 has no owner.
    unsafe { ax_cpu::boot::select_privileged_stack() };
    let current_el = ax_cpu::registers::current_exception_level();
    let timer_mode = timer::select_aarch64_timer_mode(false, current_el >= 2);
    if current_el >= 2 {
        // SAFETY: boot owns the destination stack and entry before translation
        // handoff, and transfers only the selected timer mode as an integer.
        unsafe {
            ax_cpu::boot::El1::enter(
                sym_addr!(el_entry).into(),
                sym_addr!(__cpu0_stack_top).into(),
                timer_mode as usize,
            )
        };
    }
    el_entry(timer_mode as usize)
}

pub fn switch_to_elx_secondary(cpu_meta_paddr: usize) -> ! {
    // SAFETY: the secondary assembly entry selected its private boot stack.
    unsafe { ax_cpu::boot::select_privileged_stack() };
    let current_el = ax_cpu::registers::current_exception_level();
    if current_el >= 2 {
        // SAFETY: the primary published this live metadata before firmware
        // started the CPU; its first word is the exclusive secondary stack top.
        let stack_top = unsafe { (cpu_meta_paddr as *const usize).read_volatile() };
        // SAFETY: the boot owner retains metadata and stack until secondary
        // handoff completes. CPU entry preserves the metadata address in x0.
        unsafe {
            ax_cpu::boot::El1::enter(
                sym_addr!(crate::arch::entry::secondary_el_entry).into(),
                stack_top.into(),
                cpu_meta_paddr,
            )
        };
    }
    // SAFETY: the same published metadata remains owned by this secondary CPU.
    unsafe { crate::arch::entry::secondary_el_entry(cpu_meta_paddr) }
}

#[inline(always)]
pub fn flush_tlb(vaddr: Option<VirtAddr>) {
    match vaddr {
        Some(address) => ax_cpu::mmu::El1::flush_tlb_inner_shareable(Some(address)),
        None => ax_cpu::mmu::El1::flush_tlb(None),
    }
}

#[inline(always)]
pub fn setup_table_regs() {
    // Device-nGnRE
    let attr0 = MAIR_EL1::Attr0_Device::nonGathering_nonReordering_EarlyWriteAck;
    // Normal
    let attr1 = MAIR_EL1::Attr1_Normal_Inner::WriteBack_NonTransient_ReadWriteAlloc
        + MAIR_EL1::Attr1_Normal_Outer::WriteBack_NonTransient_ReadWriteAlloc;
    // No cache
    let attr2 =
        MAIR_EL1::Attr2_Normal_Inner::NonCacheable + MAIR_EL1::Attr2_Normal_Outer::NonCacheable;
    // WriteThrough
    let attr3 = MAIR_EL1::Attr3_Normal_Inner::WriteThrough_Transient_WriteAlloc
        + MAIR_EL1::Attr3_Normal_Outer::WriteThrough_Transient_WriteAlloc;

    // SAFETY: the boot owner has not enabled its new translation regime;
    // these slots match the descriptors constructed by boot paging.
    unsafe { ax_cpu::mmu::El1::configure_stage1((attr0 + attr1 + attr2 + attr3).value) };
}

pub fn get_kernal_table() -> PageTableInfo {
    let space = ax_cpu::mmu::El1::read_kernel_address_space();
    PageTableInfo {
        asid: space.hardware_tag() as usize,
        addr: space.root().as_usize(),
    }
}

pub fn set_kernal_table(table: PageTableInfo) {
    let space = ax_cpu::mmu::HardwareAddressSpace::new(table.addr.into(), table.asid as u16);
    // SAFETY: the boot mapping owner retains the aligned tables and active mappings.
    unsafe { ax_cpu::mmu::El1::write_kernel_address_space(space) };
}

pub fn set_user_table(table: PageTableInfo) {
    let space = ax_cpu::mmu::HardwareAddressSpace::new(table.addr.into(), table.asid as u16);
    // SAFETY: the boot mapping owner retains the lower-address mapping lease.
    unsafe { ax_cpu::mmu::El1::write_user_address_space(space) };
}

pub fn get_user_table() -> PageTableInfo {
    let space = ax_cpu::mmu::El1::read_user_address_space();
    PageTableInfo {
        asid: space.hardware_tag() as usize,
        addr: space.root().as_usize(),
    }
}

#[inline(always)]
pub fn is_mmu_enabled() -> bool {
    ax_cpu::mmu::El1::is_mmu_enabled()
}

#[inline(always)]
pub fn setup_sctlr() {
    // SAFETY: boot paging has installed roots covering the active execution
    // window, stack and handoff data before enabling this regime.
    unsafe { ax_cpu::mmu::El1::enable_mmu_and_caches() };
}
