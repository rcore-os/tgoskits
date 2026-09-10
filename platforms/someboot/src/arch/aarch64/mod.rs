#[macro_use]
mod _macros;
mod console;

#[cfg(feature = "hv")]
#[path = "el2/mod.rs"]
mod elx;

#[cfg(not(feature = "hv"))]
#[path = "el1/mod.rs"]
mod elx;

mod addrspace;
mod entry;
mod head;
pub(crate) mod irq;
pub mod paging;
mod power;
pub mod relocate;
mod systimer;
mod trap;

use aarch64_cpu::registers::*;
use elx::*;
pub(crate) use entry::_secondary_entry;
pub use paging::Entry;
#[cfg(efi)]
pub(crate) use relocate::apply as relocate;

use crate::{
    ArchTrait, SystimerArch,
    arch::{addrspace::PAGE_OFFSET, trap::trap_addr},
    consts::VM_LOAD_ADDRESS,
    mem::{__kimage_va_to_pa, PageTableInfo},
    smp::cpu_area_virtual_region,
    timer::{self, ArchTimerMode},
};

pub struct Arch;

impl ArchTrait for Arch {
    type P = paging::Generic;
    type Console = console::Console;

    fn _va(paddr: usize) -> *mut u8 {
        (paddr + PAGE_OFFSET) as *mut u8
    }

    fn cpu_area_phys_to_virt(paddr: usize) -> *mut u8 {
        (paddr + PAGE_OFFSET + 0xFF00_0000_0000) as *mut u8
    }

    fn post_allocator() {
        power::init();
    }

    fn per_cpu_trap_init(is_primary: bool) {
        trap::setup();
        if is_primary {
            println!("Disable user page table");
        }
        #[cfg(uspace)]
        elx::set_user_table(PageTableInfo { asid: 0, addr: 0 });
        elx::flush_tlb(None);
    }

    fn systimer_freq() -> usize {
        ax_cpu::timer::counter_frequency() as _
    }

    fn systimer_tick() -> usize {
        match timer::aarch64_timer_mode() {
            ArchTimerMode::El1Virt => ax_cpu::timer::virtual_counter() as _,
            ArchTimerMode::El1Phys | ArchTimerMode::El2HypPhys => {
                ax_cpu::timer::physical_counter() as _
            }
        }
    }

    fn systimer_stability() -> crate::timer::CounterStability {
        // The Arm generic timer exposes the system counter shared by all PEs;
        // a virtual counter uses the platform-provided VM-wide offset.
        crate::timer::CounterStability::Stable
    }

    fn shutdown() -> ! {
        power::shutdown()
    }

    fn reset() -> ! {
        power::reset()
    }

    fn secondary_entry_fn_address() -> *const () {
        _secondary_entry as *const ()
    }

    fn kernel_page_table() -> PageTableInfo {
        elx::get_kernal_table()
    }

    fn set_kernel_page_table(val: PageTableInfo) {
        elx::set_kernal_table(val);
        elx::flush_tlb(None);
    }

    #[cfg(uspace)]
    fn user_page_table() -> PageTableInfo {
        elx::get_user_table()
    }

    #[cfg(uspace)]
    fn set_user_page_table(val: PageTableInfo) {
        elx::set_user_table(val);
        elx::flush_tlb(None);
    }

    fn user_aspace_needs_kernel_mappings() -> bool {
        false
    }

    fn virt_to_phys(vaddr: *const u8) -> usize {
        if crate::mem::mmu::is_kernel_relocated() {
            if cpu_area_virtual_region().contains(&(vaddr as usize)) {
                vaddr as usize - 0xFF00_0000_0000 - PAGE_OFFSET
            } else if vaddr as usize >= VM_LOAD_ADDRESS {
                __kimage_va_to_pa(vaddr)
            } else {
                vaddr as usize & 0xffff_ffff_ffff
            }
        } else {
            vaddr as usize
        }
    }

    fn trap_addr() -> usize {
        trap_addr()
    }

    fn jump_to(entry: usize, sp: usize) -> ! {
        // SAFETY: the boot owner has prepared the entry mapping and aligned,
        // exclusive stack; this terminal transfer abandons the bootstrap frame.
        unsafe { ax_cpu::boot::jump_to(entry.into(), sp.into()) }
    }

    fn cpu_current_hartid() -> usize {
        const ATTR0: usize = 0xFF;
        const ATTR1: usize = 0xFF << 8;
        const ATTR2: usize = 0xFF << 16;
        const ATTR3: usize = 0xFF << 32;

        const MASK: usize = ATTR0 | ATTR1 | ATTR2 | ATTR3;

        MPIDR_EL1.get() as usize & MASK
    }

    fn virtual_address_space()
    -> Result<crate::mem::VirtualAddressSpaceLayout, crate::mem::VirtualAddressSpaceError> {
        crate::mem::VirtualAddressSpaceLayout::try_new(
            crate::mem::configured_user_space(1usize << 48),
            PAGE_OFFSET..usize::MAX,
        )
    }

    fn is_mmu_enabled() -> bool {
        elx::is_mmu_enabled()
    }

    fn kick_secondary_cpu(
        hartid: usize,
        entry: usize,
        arg: usize,
    ) -> Result<(), crate::power::CpuOnError> {
        power::cpu_on(hartid as _, entry as _, arg as _).map_err(|e| match e {
            smccc::psci::error::Error::NotSupported => crate::power::CpuOnError::NotSupported,
            smccc::psci::error::Error::InvalidParameters => {
                crate::power::CpuOnError::InvalidParameters
            }
            smccc::psci::error::Error::AlreadyOn => crate::power::CpuOnError::AlreadyOn,
            e => crate::power::CpuOnError::Other(anyhow::anyhow!("cpu_on failed: {e:?}")),
        })
    }

    fn dcache_range(op: crate::DCacheOp, addr: usize, size: usize) {
        let range = ax_cpu::cache::CacheRange::new(addr.into(), size)
            .expect("boot cache range must not wrap");
        // SAFETY: the boot/DMA owner retains the mapped range and excludes
        // conflicting CPU or device access for the requested transfer phase.
        unsafe { ax_cpu::cache::maintain_dcache_to_poc(op.into(), range) };
    }

    fn dma_coherent_before_map_uncached(addr: usize, size: usize) {
        Self::dcache_range(crate::DCacheOp::CleanInvalidate, addr, size);
        ax_cpu::barrier::data_sync_system();
    }

    fn dma_coherent_before_unmap_uncached(_addr: usize, _size: usize) {
        ax_cpu::barrier::data_sync_system();
    }

    fn dma_coherent_after_mapping_update() {
        ax_cpu::barrier::data_sync_system();
        ax_cpu::barrier::instruction_sync();
    }

    // Safety: the EFI stub guarantees the same contract as the trait docs.
    unsafe fn efi_enter_kernel(system_table: *const ::core::ffi::c_void) -> bool {
        #[cfg(efi)]
        {
            crate::efi_stub::setup_service(system_table);
            unsafe { crate::arch::entry::enter_with_boot_state() }
        }
        #[cfg(not(efi))]
        {
            let _ = system_table;
            false
        }
    }
}

impl SystimerArch for Arch {
    fn systimer_irq_id() -> crate::irq::IrqId {
        // Arm architectural timer INTIDs (GIC PPI range): 30 = EL1 physical,
        // 27 = EL1 virtual, 26 = EL2 hypervisor physical.
        let intid = match timer::aarch64_timer_mode() {
            ArchTimerMode::El1Phys => 30,
            ArchTimerMode::El1Virt => 27,
            ArchTimerMode::El2HypPhys => 26,
        };
        crate::irq::IrqId::new(intid)
    }

    fn systimer_enable() {
        systimer::stop_oneshot();
        systimer::enable();
    }

    fn systimer_irq_disable() {
        systimer::mask();
    }

    fn systimer_irq_enable() {
        systimer::unmask();
    }

    fn systimer_irq_is_enabled() -> bool {
        systimer::irq_enabled()
    }

    fn systimer_set_deadline(deadline_ticks: u64) {
        systimer::set_deadline(deadline_ticks);
    }

    fn systimer_requires_irq_quiesce() -> bool {
        true
    }

    fn systimer_cancel_oneshot() {
        systimer::stop_oneshot();
    }

    fn systimer_resume_oneshot(deadline_ticks: u64) {
        // The Arm timer is level-triggered. Replace the expired compare value
        // before unmasking it so controller EOI cannot expose a stale level.
        timer::resume_masked_level_oneshot(
            || systimer::set_deadline(deadline_ticks),
            systimer::unmask,
        );
    }
}

impl From<crate::DCacheOp> for ax_cpu::cache::DataCacheOperation {
    fn from(value: crate::DCacheOp) -> Self {
        match value {
            crate::DCacheOp::Clean => Self::Clean,
            crate::DCacheOp::Invalidate => Self::Invalidate,
            crate::DCacheOp::CleanInvalidate => Self::CleanInvalidate,
        }
    }
}
