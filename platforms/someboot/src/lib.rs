#![no_std]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(target_arch = "x86_64", feature(abi_x86_interrupt))]

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

#[macro_use]
extern crate core;

#[macro_use]
extern crate log;

#[macro_use]
pub mod console;

#[cfg(target_arch = "loongarch64")]
#[path = "arch/loongarch64/mod.rs"]
pub mod arch;

#[cfg(target_arch = "aarch64")]
#[path = "arch/aarch64/mod.rs"]
pub mod arch;

#[cfg(target_arch = "x86_64")]
#[path = "arch/x86_64/mod.rs"]
pub mod arch;

#[cfg(target_arch = "riscv64")]
#[path = "arch/riscv64/mod.rs"]
pub mod arch;

mod acpi;
mod cmdline;
pub(crate) mod consts;
#[cfg(efi)]
mod efi_stub;
mod elf;
mod entry;
mod err;
pub(crate) mod fdt;
pub mod irq;
pub mod mem;
pub mod power;
pub mod rtc;
pub mod smp;
pub mod timer;

pub use acpi::rsdp_addr_phys;
pub use ax_page_table::boot::*;
pub use cmdline::cmdline;
pub use fdt::{fdt_addr, fdt_addr_phys, platform_name};
pub use somehal_macros::{entry, someboot_secondary_entry as secondary_entry};

use crate::{
    irq::IrqId,
    mem::{PageTableInfo, cpu_area_phys_to_virt},
    power::CpuOnError,
};

#[allow(unused)]
pub trait ArchTrait {
    type P: TableMeta;
    type Console: console::ArchConsoleOps;

    /// Exclusive physical-address limit for objects allocated before the
    /// runtime allocator is available.
    const EARLY_RAM_END_EXCLUSIVE: usize = usize::MAX;

    /// Architecture-owned physical range that early firmware maps may still
    /// describe as usable RAM.
    const EARLY_RESERVED_RANGE: Option<core::ops::Range<usize>> = None;

    fn _va(paddr: usize) -> *mut u8;
    fn _io(paddr: usize) -> *mut u8 {
        Self::_va(paddr)
    }
    fn ioremap_device(_addr: usize, _size: usize) -> Option<*mut u8> {
        None
    }
    fn cpu_area_phys_to_virt(paddr: usize) -> *mut u8 {
        Self::_va(paddr)
    }

    fn cpu_current_hartid() -> usize;

    fn jump_to(entry: usize, sp: usize) -> !;

    fn post_allocator();

    fn init_boot_tls() {}

    fn per_cpu_trap_init(is_primary: bool);
    fn trap_addr() -> usize;

    fn virt_to_phys(vaddr: *const u8) -> usize;

    fn canonicalize_paddr(addr: usize) -> usize {
        addr
    }
    fn user_aspace_needs_kernel_mappings() -> bool {
        true
    }

    fn kernel_space() -> core::ops::Range<usize>;
    fn is_kernel_relocated_at(addr: usize) -> bool {
        (crate::consts::VM_LOAD_ADDRESS..usize::MAX).contains(&addr)
    }

    fn is_mmu_enabled() -> bool;

    fn kernel_page_table() -> PageTableInfo;
    fn set_kernel_page_table(val: PageTableInfo);
    #[cfg(uspace)]
    fn user_page_table() -> PageTableInfo;
    #[cfg(uspace)]
    fn set_user_page_table(val: PageTableInfo);

    fn shutdown() -> !;
    fn reset() -> ! {
        Self::shutdown()
    }
    fn secondary_entry_fn_address() -> *const ();
    fn cpu_on(hartid: usize, entry: usize, arg: usize) -> Result<(), CpuOnError>;

    fn systimer_enable();
    fn systimer_irq_enable();
    fn systimer_irq_disable();
    fn systimer_irq_is_enabled() -> bool;
    /// Set the timer interval in ticks
    fn systimer_set_interval(ticks: usize);
    /// Acknowledge and clear the timer interrupt
    fn systimer_ack();
    /// Get the timer frequency in Hz
    fn systimer_freq() -> usize;
    /// Get the current timer tick count
    fn systimer_tick() -> usize;

    fn irq_all_is_enabled() -> bool;
    fn irq_all_set_enable(enable: bool);

    fn irq_is_enabled(irq: IrqId) -> bool;
    fn irq_set_enable(irq: IrqId, enable: bool);

    fn dcache_range(op: DCacheOp, addr: usize, size: usize);

    /// Prepare a cached virtual range before remapping it as uncached for DMA.
    fn dma_coherent_before_make_uncached(addr: usize, size: usize) {
        Self::dcache_range(DCacheOp::CleanInvalidate, addr, size);
    }

    /// Prepare an uncached DMA range before restoring it to cached mappings.
    fn dma_coherent_before_restore_cached(_addr: usize, _size: usize) {}

    /// Complete ordering after a DMA coherent mapping attribute update.
    fn dma_coherent_after_mapping_update() {}

    /// EFI 入口点 - 从 EFI PE 入口跳转到内核
    ///
    /// # Safety
    /// `system_table` 必须是当前 EFI 固件提供的有效 `EFI_SYSTEM_TABLE` 指针，
    /// 并且调用者必须保证此调用符合对应架构的启动约定。
    unsafe fn efi_enter_kernel(system_table: *const ::core::ffi::c_void) -> bool;
}

#[derive(Debug, Clone, Copy)]
pub enum DCacheOp {
    Clean,
    Invalidate,
    CleanInvalidate,
}

pub fn post_allocator() {
    fdt::init_with_alloc();
    smp::finalize_secondary_boot_metadata();
    debug!("Setup after allocator");
    arch::Arch::post_allocator();
}

/// Returns boot arguments captured from FDT, UEFI load options, or built into the image.
pub fn bootargs() -> Option<&'static str> {
    cmdline::cmdline()
}

/// Get the current kernel page table physical address and ASID
pub fn kernel_page_table_paddr() -> usize {
    arch::Arch::kernel_page_table().addr
}

/// Set the kernel page table physical address and ASID
pub fn set_kernel_page_table_paddr(paddr: usize) {
    arch::Arch::set_kernel_page_table(PageTableInfo {
        asid: 0,
        addr: paddr,
    });
}

#[cfg(uspace)]
pub fn user_page_table() -> PageTableInfo {
    arch::Arch::user_page_table()
}

#[cfg(uspace)]
pub fn set_user_page_table(pt: PageTableInfo) {
    arch::Arch::set_user_page_table(pt);
}

/// Entry point after enabling MMU
fn prime_entry() -> ! {
    fdt::setup_earlycon();
    let _ = acpi::earlycon::acpi_setup_earlycon();

    println!("Trap vector at {:#x}", arch::Arch::trap_addr());

    // mem::init_after_mmu();
    mem::memory_map_setup();
    mem::print_memory_map();

    smp::initialize_percpu_layout();

    unsafe extern "C" {
        fn __someboot_main() -> !;
    }

    let entry = __someboot_main as *const () as usize;
    let cpu_idx = crate::smp::early_current_cpu_idx();
    let sp = crate::smp::cpu_meta(cpu_idx).unwrap().stack_top;
    let sp = cpu_area_phys_to_virt(sp);
    println!(
        "Jumping to main entry point at {:#x} with SP {:#p}",
        entry, sp
    );
    arch::Arch::jump_to(entry, sp as _)
}
